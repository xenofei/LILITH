//! Fixture workspaces and workspace facts for tests (QA-066, QA-040).
//!
//! [`TestEnv::copy_fixture_workspace`] copies the synthetic `sample-workspace` of
//! `crates/ww-spec-tests/fixtures/` (spec Appendix E, owner decision 6) into the test's root, so
//! a test changes only its own copy and never the fixture. Nothing here uses `ww-schema`: spec
//! tests must not lean on the code they test (QA-018), so [`version_content_hash_of`] is the
//! Appendix E content-hash rule written again, and [`TestEnv::registered`] reads `suite.toml`
//! with the kit's own `toml`.

use std::path::{Path, PathBuf};

use ww_platform::Env;

use crate::bins::workspace_root;
use crate::env::TestEnv;
use crate::files::{sha256_bytes, sha256_file};

/// `<workspace>\crates\ww-spec-tests\fixtures\sample-workspace`.
pub fn fixture_workspace_dir() -> PathBuf {
    workspace_root()
        .join("crates")
        .join("ww-spec-tests")
        .join("fixtures")
        .join("sample-workspace")
}

impl TestEnv {
    /// Copies the fixture `sample-workspace` to `<root>\ws-<name>` byte for byte and returns
    /// that folder.
    ///
    /// # Panics
    /// If `<root>\ws-<name>` already exists or a file cannot be copied.
    pub fn copy_fixture_workspace(&self, name: &str) -> PathBuf {
        let to = self.root().join(format!("ws-{name}"));
        assert!(!to.exists(), "{} already exists", to.display());
        copy_tree(&fixture_workspace_dir(), &to);
        to
    }

    /// The workspace id the suite gives `ws`: `Env::workspace_hash` of its canonical path.
    ///
    /// # Panics
    /// If `ws` does not exist.
    pub fn workspace_id(&self, ws: &Path) -> String {
        let canonical = std::fs::canonicalize(ws)
            .unwrap_or_else(|e| panic!("cannot canonicalise {}: {e}", ws.display()));
        Env::workspace_hash(&canonical)
    }

    /// This channel's local data folder for `ws` (where its `state.db` and `index.db` live).
    ///
    /// # Panics
    /// If `ws` does not exist or its volume cannot be resolved.
    pub fn workspace_local_dir(&self, ws: &Path) -> PathBuf {
        self.env
            .workspace_local_dir(ws)
            .unwrap_or_else(|e| panic!("no local folder for {}: {e}", ws.display()))
    }

    /// What this channel's `suite.toml` registers: `default_workspace`, and every
    /// `[[workspaces]]` `path` in file order. A missing file registers nothing.
    ///
    /// # Panics
    /// If the file exists but cannot be read or is not TOML.
    pub fn registered(&self) -> (Option<PathBuf>, Vec<PathBuf>) {
        let file = self.env.suite_toml();
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (None, Vec::new()),
            Err(e) => panic!("cannot read {}: {e}", file.display()),
        };
        let table: toml::Table = text
            .parse()
            .unwrap_or_else(|e| panic!("{} is not TOML: {e}", file.display()));
        let default = table
            .get("default_workspace")
            .and_then(toml::Value::as_str)
            .map(PathBuf::from);
        let listed = table
            .get("workspaces")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.get("path").and_then(toml::Value::as_str))
            .map(PathBuf::from)
            .collect();
        (default, listed)
    }
}

/// Copies the folder `from` to the new folder `to`, recursively.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap_or_else(|e| panic!("cannot create {}: {e}", to.display()));
    let entries =
        std::fs::read_dir(from).unwrap_or_else(|e| panic!("cannot list {}: {e}", from.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("cannot list {}: {e}", from.display()));
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            copy_tree(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap_or_else(|e| {
                panic!("cannot copy {} to {}: {e}", src.display(), dst.display())
            });
        }
    }
}

/// Every file under `dir`, as `/`-separated paths relative to it.
fn relative_files(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let entries =
            std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
                .path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).expect("under the root");
                let parts: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(parts.join("/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

/// The content hash of the sealed version folder `dir` (`v<version>\`), by the Appendix E
/// rule: the SHA-256 of a text with one line `<sha256 hex>  <relative path>\n` per file,
/// except `source/**`, `manifest.snapshot.json` and OS metadata files (`Thumbs.db`,
/// `desktop.ini`, `.DS_Store`; ruling Q32), sorted by `/`-separated relative path in byte
/// order. Written independently of `ww-schema`'s (QA-018).
///
/// # Panics
/// If the folder or a file cannot be read.
pub fn version_content_hash_of(dir: &Path) -> String {
    let mut files: Vec<String> = relative_files(dir)
        .into_iter()
        .filter(|rel| !rel.starts_with("source/") && rel != "manifest.snapshot.json")
        // OS metadata files (Explorer's `Thumbs.db` and `desktop.ini`, macOS's `.DS_Store`)
        // are never part of a version's content hash (ruling Q32).
        .filter(|rel| {
            let name = rel.rsplit('/').next().unwrap_or(rel);
            !["thumbs.db", "desktop.ini", ".ds_store"]
                .iter()
                .any(|m| name.eq_ignore_ascii_case(m))
        })
        .collect();
    files.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let mut text = String::new();
    for rel in files {
        let hash = sha256_file(&dir.join(rel.replace('/', "\\")));
        text.push_str(&format!("{hash}  {rel}\n"));
    }
    sha256_bytes(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Every asset manifest of a workspace copy: `projects/*/assets/*/<asset>.manifest.json`.
    fn manifests(ws: &Path) -> Vec<(PathBuf, Value)> {
        let mut out = Vec::new();
        for project in std::fs::read_dir(ws.join("projects")).unwrap() {
            let assets = project.unwrap().path().join("assets");
            let Ok(entries) = std::fs::read_dir(&assets) else {
                continue;
            };
            for asset in entries {
                let dir = asset.unwrap().path();
                let id = dir.file_name().unwrap().to_string_lossy().into_owned();
                let file = dir.join(format!("{id}.manifest.json"));
                let value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
                out.push((dir, value));
            }
        }
        out
    }

    fn bare(hash: &str) -> &str {
        hash.strip_prefix("sha256:").unwrap_or(hash)
    }

    #[test] // spec: QA-066
    fn fixture_workspace_copies_with_recorded_hashes() {
        let t = TestEnv::with_label("Ünïcödé space");
        let ws = t.copy_fixture_workspace("sample");
        assert_eq!(ws, t.root().join("ws-sample"));
        assert!(ws.join("worldwright.yaml").is_file());

        // Byte for byte: every fixture file is copied with the same hash.
        let source = fixture_workspace_dir();
        let files = relative_files(&source);
        assert!(files.len() >= 15, "{files:?}");
        assert_eq!(relative_files(&ws).len(), files.len());
        for rel in &files {
            let native = rel.replace('/', "\\");
            assert_eq!(
                sha256_file(&ws.join(&native)),
                sha256_file(&source.join(&native)),
                "{rel}"
            );
        }

        let found = manifests(&ws);
        assert_eq!(found.len(), 2, "both fixture assets");
        let (mut hashes, mut run_files) = (0, 0);
        for (asset, manifest) in &found {
            for (rel, hash) in manifest["hashes"].as_object().expect("hashes") {
                let hash = hash.as_str().unwrap();
                assert_eq!(sha256_file(&asset.join(rel)), bare(hash), "{rel}");
                hashes += 1;
            }
            for run in manifest["candidate_runs"].as_array().into_iter().flatten() {
                let selected: Vec<&str> = run["selected"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                for file in run["files"].as_array().expect("run files") {
                    let rel = file["path"].as_str().unwrap();
                    let name = rel.rsplit('/').next().unwrap();
                    // A chosen file was moved into its sealed version by the sign-off.
                    let on_disk = if selected.contains(&name) {
                        let version = run["signed_off"]["version"].as_str().unwrap();
                        asset.join(format!("v{version}")).join(name)
                    } else {
                        asset.join(rel)
                    };
                    assert_eq!(
                        sha256_file(&on_disk),
                        bare(file["sha256"].as_str().unwrap()),
                        "{rel}"
                    );
                    run_files += 1;
                }
            }
        }
        assert_eq!((hashes, run_files), (6, 4));

        // A second copy needs a second name.
        let again = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            t.copy_fixture_workspace("sample")
        }));
        assert!(again.is_err());
    }

    #[test] // spec: QA-066, CORE-047
    fn version_content_hash_of_ignores_source_and_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let v = dir.path().join("v1.0.0");
        let write = |rel: &str, bytes: &[u8]| {
            crate::files::write_file(&v.join(rel.replace('/', "\\")), bytes);
        };
        write("a.png", b"a");
        write("B.png", b"b");
        write("sub/c.txt", "Ünïcödé space".as_bytes());
        // Built by hand: byte order puts "B.png" before "a.png", two spaces after the hash.
        let text = format!(
            "{}  B.png\n{}  a.png\n{}  sub/c.txt\n",
            sha256_bytes(b"b"),
            sha256_bytes(b"a"),
            sha256_bytes("Ünïcödé space".as_bytes())
        );
        let want = sha256_bytes(text.as_bytes());
        assert_eq!(version_content_hash_of(&v), want);

        write("source/a.psd", b"layers");
        write("manifest.snapshot.json", b"{}");
        assert_eq!(
            version_content_hash_of(&v),
            want,
            "source and snapshot are ignored"
        );
        write("Thumbs.db", b"thumbnails");
        write("sub/desktop.ini", b"[.ShellClassInfo]");
        write(".DS_Store", b"finder");
        assert_eq!(
            version_content_hash_of(&v),
            want,
            "OS metadata files are ignored (ruling Q32)"
        );

        write("a.png", b"changed");
        assert_ne!(version_content_hash_of(&v), want);

        // The fixture's sealed version: one file, whose hash its manifest records.
        let crane = fixture_workspace_dir()
            .join("projects")
            .join("sketchbook")
            .join("assets")
            .join("chr_paper_crane")
            .join("v1.0.0");
        let one = "aaae4dbdb0e697350dac691b26928d34a69061cf320b175c257c103fcaa32766";
        assert_eq!(
            version_content_hash_of(&crane),
            sha256_bytes(format!("{one}  crane_b.png\n").as_bytes())
        );
    }

    #[test] // spec: QA-040
    fn workspace_id_matches_platform_hash() {
        let t = TestEnv::with_label("Ünïcödé space");
        let ws = t.make_workspace("Ünïcödé space");
        let id = t.workspace_id(&ws);
        assert_eq!(id.len(), 16, "{id}");
        assert!(
            id.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{id}"
        );
        assert_eq!(
            id,
            Env::workspace_hash(&std::fs::canonicalize(&ws).unwrap())
        );
        // Another spelling of the same folder has the same id.
        let other = PathBuf::from(ws.to_string_lossy().to_uppercase()).join(".");
        assert_eq!(t.workspace_id(&other), id);
        // Same volume as the channel's local folder: `<local>\workspaces\<id>`.
        let local = t.workspace_local_dir(&ws);
        assert!(local.starts_with(t.root()), "{}", local.display());
        assert!(local.ends_with(&id), "{}", local.display());
        assert_ne!(t.workspace_id(&t.make_workspace("other")), id);
    }

    #[test] // spec: QA-040
    fn registered_reads_default_and_listed_paths() {
        let t = TestEnv::new();
        assert_eq!(t.registered(), (None, Vec::new()));
        let a = t.make_workspace("a");
        let b = t.make_workspace("b");
        t.register_workspace(&a, false);
        assert_eq!(t.registered(), (None, vec![a.clone()]));
        t.register_workspace(&b, true);
        assert_eq!(t.registered(), (Some(b.clone()), vec![a, b]));
    }
}
