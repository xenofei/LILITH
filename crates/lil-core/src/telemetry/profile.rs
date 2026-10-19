//! The hardware profile (CORE-067): on core start the measured GPU, VRAM and RAM are compared
//! with the workspace file's `hardware` key, and the file is edited, with a targeted YAML edit
//! through the store, only when `gpu`, `vram_gb` or `ram_gb` differs.
//!
//! The key is `hardware: {gpu, vram_gb, ram_gb, detected}`: `gpu` is the name NVML gives (or
//! `none` without a GPU), the sizes are in GB rounded to the nearest, `detected` is the UTC date
//! of the reading. The `hardware:` key is **machine-owned**: when it changes, the
//! whole block is rewritten (CORE-067), so a comment or a key added by hand inside it does not
//! survive; comments and keys around it do. Only `lilith.yaml` is written: a workspace that still has only the legacy
//! file keeps it untouched (R4), and a read-only workspace is not written either.

use std::sync::Arc;

use lil_platform::names::WORKSPACE_FILE;
use lil_protocol::ErrorBody;
use lil_schema::workspace_yaml::{HardwareYaml, read_hardware};
use lil_schema::{YamlEdit, YamlValue};

use super::{Snapshot, Telemetry, parse_ts};
use crate::store::{StoreError, WsPath};
use crate::workspace::{Access, OpenWorkspace};

/// What `hardware.gpu` says when the machine has no GPU.
const NO_GPU: &str = "none";
/// The first samples after the start that may still lack a GPU reading before the machine is
/// taken to have none (a GPU that is still starting up must not be written down as absent).
const GPU_PATIENCE: u64 = 10;

/// The measured profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareProfile {
    pub gpu: String,
    pub vram_gb: u64,
    pub ram_gb: u64,
}

fn gb(mib: u64) -> u64 {
    (mib + 512) / 1024
}

impl HardwareProfile {
    /// The profile `snap` shows; `None` before RAM has been read (nothing is known yet).
    pub fn from_snapshot(snap: &Snapshot) -> Option<HardwareProfile> {
        if snap.ram.total_mib == 0 {
            return None;
        }
        Some(HardwareProfile {
            gpu: snap
                .gpu
                .as_ref()
                .map_or_else(|| NO_GPU.to_string(), |g| g.name.clone()),
            vram_gb: snap.gpu.as_ref().map_or(0, |g| gb(g.vram_total_mib)),
            ram_gb: gb(snap.ram.total_mib),
        })
    }

    /// Whether the file's `hardware` already says this. A GPU-less profile has no `vram_gb`
    /// to compare when the file has none either.
    fn matches(&self, file: &HardwareYaml) -> bool {
        file.gpu.as_deref() == Some(self.gpu.as_str())
            && (file.vram_gb == Some(self.vram_gb)
                || (self.gpu == NO_GPU && file.vram_gb.is_none()))
            && file.ram_gb == Some(self.ram_gb)
    }
}

/// Compares `snap`'s hardware with `open`'s workspace file and edits the file when `gpu`,
/// `vram_gb` or `ram_gb` differs. Returns whether it was edited.
///
/// # Errors
/// The store's error for a workspace file that can't be read, parsed or edited
/// (`yaml.unsupported_edit` for a form the line editor won't change).
pub async fn profile_check(open: &OpenWorkspace, snap: &Snapshot) -> Result<bool, ErrorBody> {
    if open.access != Access::ReadWrite {
        return Ok(false);
    }
    let Some(profile) = HardwareProfile::from_snapshot(snap) else {
        return Ok(false);
    };
    let rel = WsPath::parse(WORKSPACE_FILE).expect("the workspace file name is a plain name");
    let store_error = |e: StoreError| e.to_error("");
    let Some(bytes) = open.store.read(&rel).await.map_err(store_error)? else {
        return Ok(false);
    };
    let unsupported = |why: String| StoreError::YamlUnsupported(why).to_error("");
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| unsupported(format!("{WORKSPACE_FILE} isn't UTF-8 text")))?;
    let current = read_hardware(text).map_err(unsupported)?;
    if profile.matches(&current) {
        return Ok(false);
    }
    let detected = lil_schema::time::date(parse_ts(&snap.ts).unwrap_or(0).div_euclid(1000));
    open.store
        .edit_yaml(
            &rel,
            &[YamlEdit::SetMapping {
                path: vec!["hardware".to_string()],
                entries: vec![
                    ("gpu".to_string(), YamlValue::Str(profile.gpu)),
                    (
                        "vram_gb".to_string(),
                        YamlValue::Int(i64::try_from(profile.vram_gb).unwrap_or(i64::MAX)),
                    ),
                    (
                        "ram_gb".to_string(),
                        YamlValue::Int(i64::try_from(profile.ram_gb).unwrap_or(i64::MAX)),
                    ),
                    ("detected".to_string(), YamlValue::Str(detected)),
                ],
            }],
        )
        .await
        .map_err(store_error)?;
    Ok(true)
}

/// Whether the profile can be checked with a snapshot, `samples` samples in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// A GPU reading is still to come.
    Wait,
    Check,
    /// There is a GPU but it never answered: what the machine has is not known, and `none`
    /// would be a false value in a file that is committed.
    Skip,
}

fn verdict(snap: &Snapshot, has_gpu: bool, samples: u64) -> Verdict {
    if snap.gpu.is_some() || !has_gpu {
        Verdict::Check
    } else if samples >= GPU_PATIENCE {
        Verdict::Skip
    } else {
        Verdict::Wait
    }
}

/// Checks `open`'s profile once, on the first reading that settles what the machine has (a
/// reading with a GPU, or, for a machine with no GPU source, the first one). A GPU that has not
/// answered after [`GPU_PATIENCE`] samples is skipped, with a log line: nothing is written
/// rather than `none`. It does nothing if the telemetry stops or the workspace is gone first.
pub fn spawn_check(telemetry: Arc<Telemetry>, open: &Arc<OpenWorkspace>) {
    let open = Arc::downgrade(open);
    let stopped = telemetry.stopped();
    tokio::spawn(async move {
        let settled = async {
            let mut want = 1;
            loop {
                let snap = telemetry.wait_for_samples(want).await;
                match verdict(&snap, telemetry.has_gpu(), want) {
                    Verdict::Wait => want += 1,
                    v => return (v, snap),
                }
            }
        };
        let (verdict, snap) = tokio::select! {
            () = stopped.cancelled() => return,
            settled = settled => settled,
        };
        if verdict == Verdict::Skip {
            tracing::warn!(
                "the GPU has not answered; the workspace's hardware profile is left as it is"
            );
            return;
        }
        let Some(open) = open.upgrade() else {
            return;
        };
        match profile_check(&open, &snap).await {
            Ok(true) => tracing::info!("the workspace's hardware profile was updated"),
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(code = %e.code, error = %e.message, "the hardware profile could not be checked");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-067
    fn sizes_round_to_the_nearest_gigabyte() {
        assert_eq!(gb(16_376), 16);
        assert_eq!(gb(16_384), 16);
        assert_eq!(gb(32_616), 32);
        assert_eq!(gb(511), 0);
        assert_eq!(gb(512), 1);
    }

    /// A GPU that is there but silent is waited for, then skipped, never written as `none`
    /// (Task 9 review M3); a machine with no GPU source is checked at once.
    #[test] // spec: CORE-067
    fn a_silent_gpu_is_skipped_not_written_as_none() {
        let mut snap = Snapshot::empty(0);
        assert_eq!(verdict(&snap, false, 1), Verdict::Check);
        assert_eq!(verdict(&snap, true, 1), Verdict::Wait);
        assert_eq!(verdict(&snap, true, GPU_PATIENCE - 1), Verdict::Wait);
        assert_eq!(verdict(&snap, true, GPU_PATIENCE), Verdict::Skip);
        snap.gpu = Some(crate::telemetry::GpuView {
            name: "G".into(),
            driver: "1".into(),
            util_pct: 0,
            power_mw: 0,
            energy_mj: None,
            temp_c: 0,
            sm_clock_mhz: 0,
            mem_clock_mhz: 0,
            fan_pct: None,
            throttle: Vec::new(),
            vram_total_mib: 0,
            vram_used_mib: 0,
            measured: true,
        });
        assert_eq!(verdict(&snap, true, 1), Verdict::Check);
    }

    #[test] // spec: CORE-067
    fn a_machine_without_a_gpu_matches_a_file_that_says_none() {
        let none = HardwareProfile {
            gpu: NO_GPU.into(),
            vram_gb: 0,
            ram_gb: 16,
        };
        let file = |gpu: Option<&str>, vram: Option<u64>, ram: Option<u64>| HardwareYaml {
            gpu: gpu.map(str::to_string),
            vram_gb: vram,
            ram_gb: ram,
        };
        assert!(none.matches(&file(Some("none"), None, Some(16))));
        assert!(none.matches(&file(Some("none"), Some(0), Some(16))));
        assert!(!none.matches(&file(Some("none"), None, Some(8))));
        assert!(!none.matches(&file(None, None, None)));
        let gpu = HardwareProfile {
            gpu: "RTX".into(),
            vram_gb: 16,
            ram_gb: 32,
        };
        assert!(gpu.matches(&file(Some("RTX"), Some(16), Some(32))));
        assert!(!gpu.matches(&file(Some("RTX"), None, Some(32))));
        assert!(!gpu.matches(&file(Some("RTX"), Some(8), Some(32))));
    }
}
