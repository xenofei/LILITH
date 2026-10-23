//! Automation review M1: `winshell-core power --parent-pid P` releases its request and exits
//! once P has gone, so a closed console never leaves the PC held awake. The test uses
//! `--api none`, which holds no power request at all, a parent it starts itself (`ping`), and a
//! temp log folder (`WINSHELL_LOG_DIR`); it kills only the processes it started.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("winshell-core-parent-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).expect("mkdir");
    d
}

#[test]
fn the_core_clears_and_exits_when_its_parent_is_gone() {
    let dir = temp_dir("gone");
    let out = dir.join("power.json");
    let mut parent = Command::new("ping")
        .args(["-n", "60", "127.0.0.1"])
        .stdout(Stdio::null())
        .spawn()
        .expect("start ping");
    let mut core = Command::new(env!("CARGO_BIN_EXE_winshell-core"))
        .args([
            "power",
            "--api",
            "none",
            "--max-secs",
            "120",
            "--parent-pid",
        ])
        .arg(parent.id().to_string())
        .arg("--out")
        .arg(&out)
        .env("WINSHELL_LOG_DIR", &dir)
        .spawn()
        .expect("start core");
    thread::sleep(Duration::from_millis(1500));
    assert!(
        core.try_wait().expect("poll").is_none(),
        "the core exited while its parent ran"
    );

    parent.kill().expect("kill ping");
    let _ = parent.wait();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(s) = core.try_wait().expect("poll") {
            break Some(s);
        }
        if Instant::now() > deadline {
            break None;
        }
        thread::sleep(Duration::from_millis(100));
    };
    if status.is_none() {
        let _ = core.kill();
    }
    assert!(
        status.is_some(),
        "the core still ran 10 s after its parent was gone"
    );
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("result")).expect("json");
    let kinds: Vec<&str> = doc["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"parent_gone"), "{kinds:?}");
    assert!(kinds.contains(&"cleared"), "{kinds:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_parent_that_is_already_gone_holds_nothing() {
    let dir = temp_dir("none");
    let out = dir.join("power.json");
    let status = Command::new(env!("CARGO_BIN_EXE_winshell-core"))
        .args([
            "power",
            "--api",
            "none",
            "--parent-pid",
            "4294967295",
            "--out",
        ])
        .arg(&out)
        .env("WINSHELL_LOG_DIR", &dir)
        .status()
        .expect("run core");
    assert!(status.success());
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("result")).expect("json");
    assert!(
        doc["error"]
            .as_str()
            .unwrap_or("")
            .contains("parent process"),
        "{doc}"
    );
    assert!(doc.get("events").is_none(), "{doc}");
    let _ = std::fs::remove_dir_all(&dir);
}
