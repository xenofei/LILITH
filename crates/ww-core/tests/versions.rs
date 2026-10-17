//! Sealed versions (ARCH-075, CORE-026, CORE-028, CORE-033, CORE-035, CORE-041): `seal` under
//! the per-asset `version_lock`, read-only sealed files and snapshot, copies of files others
//! were derived from, `source/` outside the content hash, no half-filled version after a
//! failure, post-move identities, sealed folders and manifest `locks` refused, and legacy
//! folders read as they are.
//!
//! Every test runs on tokio's paused clock and a `FakeClock`, so retry budgets pass without
//! real sleeps (QA-031). Fixture files are made with `ww_testkit::files::write_file`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use ww_core::state::{StateDb, StateScope};
use ww_core::store::{Expect, FailPoints, SealRequest, Store, StoreError, WsPath};
use ww_platform::fs::{Probe, Share, SystemProbe};
use ww_schema::{Manifest, Version, version_content_hash};
use ww_testkit::files::write_file;
use ww_testkit::{FakeClock, HandleHolder, SeqIds, TestEnv, sha256_bytes, version_content_hash_of};

// ---------------------------------------------------------------------------------------------
// Fixture.
// ---------------------------------------------------------------------------------------------

const PROJECT: &str = "sketchbook";
const ASSET_ID: &str = "crane";
/// The fixture asset's folder.
const ASSET: &str = "projects/sketchbook/assets/crane";
/// Its manifest.
const MANIFEST: &str = "projects/sketchbook/assets/crane/crane.manifest.json";
const FILE_ATTRIBUTE_READONLY: u32 = 0x1;

/// A workspace folder and its `state.db`, both under the test's own root.
struct Ws {
    env: TestEnv,
    root: PathBuf,
    state: Arc<StateDb>,
    clock: Arc<FakeClock>,
}

fn no_fatal() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|what: &str| panic!("unexpected fatal report: {what}"))
}

fn workspace() -> Ws {
    let env = TestEnv::with_label("Ünïcödé space");
    let root = env.make_workspace("versions");
    Ws::at(env, root)
}

impl Ws {
    fn at(env: TestEnv, root: PathBuf) -> Ws {
        let root = std::fs::canonicalize(root).unwrap();
        let text = root.to_string_lossy().into_owned();
        let root = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text));
        let db = env.root().join("local-ws").join("state.db");
        let state =
            Arc::new(StateDb::open(&db, StateScope::Workspace, no_fatal()).expect("state.db"));
        Ws {
            env,
            root,
            state,
            clock: FakeClock::new(),
        }
    }

    fn store(&self, fp: Arc<FailPoints>) -> Arc<Store> {
        Arc::new(
            Store::new(
                self.root.clone(),
                self.state.clone(),
                fp,
                self.clock.clone(),
                Arc::new(SeqIds::new()),
                Arc::new(SystemProbe),
            )
            .with_machine("testmachine"),
        )
    }

    fn plain(&self) -> Arc<Store> {
        self.store(FailPoints::none())
    }

    fn file(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.root.clone(), |p, c| p.join(c))
    }

    fn read(&self, rel: &str) -> Vec<u8> {
        std::fs::read(self.file(rel)).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
    }

    fn exists(&self, rel: &str) -> bool {
        self.file(rel).exists()
    }

    fn read_only(&self, rel: &str) -> bool {
        read_only(&self.file(rel))
    }

    /// Writes the files of run `run` into `candidates/<run>/` and a manifest with that run:
    /// `files` are `(name, role, from)`; a name may hold `/` (a `source/` file, which is not
    /// a run file record).
    fn run(&self, run: &str, files: &[(&str, &str, Option<&str>)]) -> Manifest {
        let mut records = Vec::new();
        for (name, role, from) in files {
            let bytes = format!("{run}/{name}").into_bytes();
            write_file(
                &self.file(&format!("{ASSET}/candidates/{run}/{name}")),
                &bytes,
            );
            if name.starts_with("source/") {
                continue;
            }
            let mut record = json!({
                "path": format!("candidates/{run}/{name}"),
                "role": role,
                "sha256": sha256_bytes(&bytes),
            });
            if let Some(from) = from {
                record["from"] = json!(from);
            }
            records.push(record);
        }
        let mut m = self.manifest().unwrap_or_else(|| {
            Manifest::new(
                PROJECT,
                ASSET_ID,
                "image",
                "00000000-0000-4000-8000-0000000000aa",
            )
        });
        m.push(
            "candidate_runs",
            json!({
                "run_id": run,
                "path": format!("candidates/{run}/"),
                "status": "open",
                "created": "2027-01-15T08:00:00Z",
                "target_version": "1.0.0",
                "files": records,
                "pinned": false,
            }),
        )
        .unwrap();
        write_file(&self.file(MANIFEST), m.to_text().as_bytes());
        m
    }

    fn manifest(&self) -> Option<Manifest> {
        let bytes = std::fs::read(self.file(MANIFEST)).ok()?;
        Some(
            Manifest::parse(std::str::from_utf8(&bytes).unwrap())
                .unwrap()
                .0,
        )
    }

    /// The names under `rel` (recursive, `/`-separated), sorted; none when it is missing.
    fn tree(&self, rel: &str) -> Vec<String> {
        let mut out = Vec::new();
        walk(&self.file(rel), "", &mut out);
        out.sort();
        out
    }

    /// The staging folder's `seal-*` folders, workspace-relative.
    fn seal_folders(&self) -> Vec<String> {
        let staging = self.file(".worldwright/staging");
        let Ok(entries) = std::fs::read_dir(&staging) else {
            return Vec::new();
        };
        entries
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("seal-"))
            .map(|n| format!(".worldwright/staging/{n}"))
            .collect()
    }
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let entry = entry.unwrap();
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if entry.file_type().unwrap().is_dir() {
            walk(&entry.path(), &format!("{name}/"), out);
        } else {
            out.push(name);
        }
    }
}

fn read_only(path: &Path) -> bool {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .readonly()
}

fn p(rel: &str) -> WsPath {
    WsPath::parse(rel).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn v(text: &str) -> Version {
    text.parse().unwrap()
}

fn request(run: &str, version: &str, files: &[&str], snapshot: Manifest) -> SealRequest {
    SealRequest {
        project: PROJECT.to_string(),
        asset_id: ASSET_ID.to_string(),
        version: v(version),
        run_id: run.to_string(),
        files: files.iter().map(|f| (*f).to_string()).collect(),
        snapshot,
        expected: Default::default(),
    }
}

/// Seals under the asset's version lock, as `asset.signoff` does.
async fn seal(store: &Store, req: SealRequest) -> Result<ww_core::store::Sealed, StoreError> {
    let guard = store
        .version_lock(&req.project, &req.asset_id)
        .lock_owned()
        .await;
    store.seal(&guard, req).await
}

fn bytes_of(run: &str, name: &str) -> Vec<u8> {
    format!("{run}/{name}").into_bytes()
}

// ---------------------------------------------------------------------------------------------
// Sealing.
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: CORE-026
async fn seal_moves_files_and_writes_a_read_only_snapshot() {
    let ws = workspace();
    let m = ws.run(
        "r_1",
        &[("a.png", "candidate", None), ("b.png", "candidate", None)],
    );
    let store = ws.plain();
    let sealed = seal(&store, request("r_1", "1.0.0", &["a.png"], m.clone()))
        .await
        .expect("the seal");

    let a = sha256_bytes(&bytes_of("r_1", "a.png"));
    assert_eq!(sealed.dir.as_str(), format!("{ASSET}/v1.0.0"));
    assert_eq!(sealed.files, [("v1.0.0/a.png".to_string(), a.clone())]);
    assert_eq!(
        sealed.content_hash,
        version_content_hash(&[("a.png".to_string(), a.clone())])
    );
    assert_eq!(
        sealed.content_hash,
        version_content_hash_of(&ws.file(&format!("{ASSET}/v1.0.0")))
    );

    assert_eq!(
        ws.tree(&format!("{ASSET}/v1.0.0")),
        ["a.png", "manifest.snapshot.json"]
    );
    assert_eq!(
        ws.read(&format!("{ASSET}/v1.0.0/a.png")),
        bytes_of("r_1", "a.png")
    );
    assert_eq!(
        ws.read(&format!("{ASSET}/v1.0.0/manifest.snapshot.json")),
        m.to_text().into_bytes()
    );
    for name in ["a.png", "manifest.snapshot.json"] {
        assert!(
            ws.read_only(&format!("{ASSET}/v1.0.0/{name}")),
            "{name} is not read-only"
        );
    }
    // Moved, not copied; the run's other file stays.
    assert_eq!(ws.tree(&format!("{ASSET}/candidates/r_1")), ["b.png"]);
    assert!(ws.seal_folders().is_empty(), "{:?}", ws.seal_folders());
    // The sealed files are the core's own (the watcher never takes them for human edits).
    assert!(
        store
            .accepts_as_own(&p(&format!("{ASSET}/v1.0.0/a.png")), &a)
            .await
            .unwrap()
    );
    let snapshot = sha256_bytes(m.to_text().as_bytes());
    assert!(
        store
            .accepts_as_own(
                &p(&format!("{ASSET}/v1.0.0/manifest.snapshot.json")),
                &snapshot
            )
            .await
            .unwrap()
    );
    // Sealing records no version in the manifest (the Record stage does): until then the
    // version is an orphan, and nothing in it is lost.
    let orphans = store.orphans().await.unwrap();
    assert_eq!(
        orphans.iter().map(WsPath::as_str).collect::<Vec<_>>(),
        [format!("{ASSET}/v1.0.0").as_str()]
    );
}

#[tokio::test(start_paused = true)] // spec: CORE-026
async fn derived_from_files_are_copied_not_moved() {
    let ws = workspace();
    let m = ws.run(
        "r_1",
        &[
            ("c.png", "candidate", None),
            ("c_cut.png", "derived", Some("c.png")),
            ("c_big.png", "derived", Some("candidates/r_1/c_cut.png")),
            ("d.png", "candidate", None),
            ("d_fix.png", "repair", Some("d.png")),
        ],
    );
    let store = ws.plain();
    let sealed = seal(&store, request("r_1", "1.0.0", &["c.png"], m))
        .await
        .expect("the seal");

    // The chosen file and the files derived from it (directly or not) are sealed.
    assert_eq!(
        ws.tree(&format!("{ASSET}/v1.0.0")),
        ["c.png", "c_big.png", "c_cut.png", "manifest.snapshot.json"]
    );
    let names: Vec<&str> = sealed.files.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(
        names,
        ["v1.0.0/c.png", "v1.0.0/c_big.png", "v1.0.0/c_cut.png"]
    );
    // Files other run files were derived from are copied; the rest are moved. A repair is
    // not a derived file, so `d.png` and its fix stay where they are.
    assert_eq!(
        ws.tree(&format!("{ASSET}/candidates/r_1")),
        ["c.png", "c_cut.png", "d.png", "d_fix.png"]
    );
    assert!(!ws.read_only(&format!("{ASSET}/candidates/r_1/c.png")));
    assert!(ws.read_only(&format!("{ASSET}/v1.0.0/c.png")));
    assert_eq!(
        ws.read(&format!("{ASSET}/v1.0.0/c.png")),
        ws.read(&format!("{ASSET}/candidates/r_1/c.png"))
    );
}

#[tokio::test(start_paused = true)] // spec: CORE-028
async fn source_moves_with_the_version_and_is_outside_the_content_hash() {
    let ws = workspace();
    let m = ws.run(
        "r_1",
        &[
            ("a.png", "candidate", None),
            ("source/a.psd", "", None),
            ("source/sub/notes.txt", "", None),
        ],
    );
    let store = ws.plain();
    let sealed = seal(&store, request("r_1", "1.0.0", &["a.png"], m))
        .await
        .expect("the seal");

    assert_eq!(
        ws.tree(&format!("{ASSET}/v1.0.0")),
        [
            "a.png",
            "manifest.snapshot.json",
            "source/a.psd",
            "source/sub/notes.txt"
        ]
    );
    assert!(
        ws.tree(&format!("{ASSET}/candidates/r_1")).is_empty(),
        "{:?}",
        ws.tree(&format!("{ASSET}/candidates/r_1"))
    );
    for name in ["source/a.psd", "source/sub/notes.txt"] {
        assert!(ws.read_only(&format!("{ASSET}/v1.0.0/{name}")), "{name}");
    }
    let a = sha256_bytes(&bytes_of("r_1", "a.png"));
    assert_eq!(
        sealed.content_hash,
        version_content_hash(&[("a.png".to_string(), a)]),
        "source/** is outside the content hash"
    );
    assert_eq!(
        sealed.content_hash,
        version_content_hash_of(&ws.file(&format!("{ASSET}/v1.0.0")))
    );
    let names: Vec<&str> = sealed.files.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(
        names,
        [
            "v1.0.0/a.png",
            "v1.0.0/source/a.psd",
            "v1.0.0/source/sub/notes.txt"
        ]
    );
}

#[tokio::test(start_paused = true)] // spec: CORE-026
async fn seals_of_one_asset_are_serialised() {
    let ws = workspace();
    ws.run("r_1", &[("x.png", "candidate", None)]);
    ws.run("r_2", &[("y.png", "candidate", None)]);
    let m = ws.run("r_3", &[("z.png", "candidate", None)]);
    let store = ws.plain();

    // One lock per asset, whatever the spelling; another asset has its own.
    assert!(Arc::ptr_eq(
        &store.version_lock(PROJECT, ASSET_ID),
        &store.version_lock("SKETCHBOOK", "Crane")
    ));
    assert!(!Arc::ptr_eq(
        &store.version_lock(PROJECT, ASSET_ID),
        &store.version_lock(PROJECT, "kite")
    ));

    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let task = |run: &'static str, version: &'static str, file: &'static str| {
        let (store, log, m) = (store.clone(), log.clone(), m.clone());
        tokio::spawn(async move {
            let guard = store.version_lock(PROJECT, ASSET_ID).lock_owned().await;
            log.lock().unwrap().push(format!("start {version}"));
            tokio::task::yield_now().await;
            let sealed = store.seal(&guard, request(run, version, &[file], m)).await;
            tokio::task::yield_now().await;
            log.lock().unwrap().push(format!("end {version}"));
            drop(guard);
            sealed
        })
    };
    let first = task("r_1", "1.1.0", "x.png");
    let second = task("r_2", "1.2.0", "y.png");
    first.await.unwrap().expect("1.1.0");
    second.await.unwrap().expect("1.2.0");
    let log = log.lock().unwrap().clone();
    assert!(
        log == ["start 1.1.0", "end 1.1.0", "start 1.2.0", "end 1.2.0"]
            || log == ["start 1.2.0", "end 1.2.0", "start 1.1.0", "end 1.1.0"],
        "the seals interleaved: {log:?}"
    );
    assert_eq!(
        ws.tree(&format!("{ASSET}/v1.1.0"))[0],
        "manifest.snapshot.json"
    );
    assert!(ws.exists(&format!("{ASSET}/v1.1.0/x.png")));
    assert!(ws.exists(&format!("{ASSET}/v1.2.0/y.png")));

    // A second 1.1.0 is refused before anything moves.
    let again = seal(&store, request("r_3", "1.1.0", &["z.png"], m)).await;
    assert_eq!(again, Err(StoreError::VersionExists("1.1.0".to_string())));
    assert_eq!(again.unwrap_err().to_error("t").code, "version.exists");
    assert_eq!(ws.tree(&format!("{ASSET}/candidates/r_3")), ["z.png"]);
    assert!(ws.seal_folders().is_empty(), "{:?}", ws.seal_folders());
}

#[tokio::test(start_paused = true)] // spec: CORE-026, ARCH-074
async fn held_file_during_seal_is_retried() {
    let ws = workspace();
    let m = ws.run("r_1", &[("a.png", "candidate", None)]);
    let held = ws.file(&format!("{ASSET}/candidates/r_1/a.png"));
    let holder = HandleHolder::hold(&held, Share::Read);
    let (attempts_tx, mut attempts) = tokio::sync::mpsc::unbounded_channel();
    let fp = FailPoints::none();
    fp.on(
        "store.replace",
        Box::new(move || {
            let _ = attempts_tx.send(());
            Ok(())
        }),
    );
    let store = ws.store(fp);
    let start = tokio::time::Instant::now();
    let mut sealing = {
        let store = store.clone();
        tokio::spawn(async move { seal(&store, request("r_1", "1.0.0", &["a.png"], m)).await })
    };
    let mut tries = 0;
    while start.elapsed() < Duration::from_secs(3) {
        tokio::select! {
            got = attempts.recv() => {
                got.expect("the seal keeps trying");
                tries += 1;
            }
            done = &mut sealing => panic!("the seal ended while the file was held: {done:?}"),
        }
    }
    assert!(tries >= 5, "{tries} attempts in 3 s");
    assert!(!ws.exists(&format!("{ASSET}/v1.0.0")));
    holder.release();
    let sealed = sealing.await.unwrap().expect("the seal after the release");
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(sealed.files.len(), 1);
    assert!(ws.read_only(&format!("{ASSET}/v1.0.0/a.png")));
}

/// Where each name of `names` is now: the number of copies in the run folder and in any
/// sealing folder.
fn copies(ws: &Ws, run: &str, names: &[&str]) -> HashMap<String, usize> {
    let mut found: HashMap<String, usize> = names.iter().map(|n| ((*n).to_string(), 0)).collect();
    let mut folders = vec![format!("{ASSET}/candidates/{run}")];
    folders.extend(ws.seal_folders());
    for folder in folders {
        for name in ws.tree(&folder) {
            if let Some(n) = found.get_mut(&name) {
                *n += 1;
            }
        }
    }
    found
}

#[tokio::test(start_paused = true)] // spec: CORE-026, QA-066
async fn failed_seal_leaves_no_half_version() {
    let names = ["a.png", "b.png", "c.png", "a_cut.png"];
    // The seal makes these file calls at `store.replace`: a.png copied, a_cut.png moved,
    // b.png moved, then the folder moved into place. Fail each in turn.
    for fail_at in 1..=4 {
        let ws = workspace();
        let m = ws.run(
            "r_1",
            &[
                ("a.png", "candidate", None),
                ("a_cut.png", "derived", Some("a.png")),
                ("b.png", "candidate", None),
                ("c.png", "candidate", None),
            ],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let fp = FailPoints::none();
        let seen = calls.clone();
        fp.on(
            "store.replace",
            Box::new(move || {
                if seen.fetch_add(1, Ordering::SeqCst) + 1 == fail_at {
                    Err(io::Error::other("fail point store.replace: io"))
                } else {
                    Ok(())
                }
            }),
        );
        let store = ws.store(fp);
        let result = seal(&store, request("r_1", "1.0.0", &["a.png", "b.png"], m)).await;
        assert!(
            matches!(result, Err(StoreError::Io { .. })),
            "call {fail_at}: {result:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), fail_at, "call {fail_at}");
        assert!(
            !ws.exists(&format!("{ASSET}/v1.0.0")),
            "call {fail_at}: a half version exists"
        );
        let found = copies(&ws, "r_1", &names);
        for name in names {
            assert_eq!(found[name], 1, "call {fail_at}: {name} is in {found:?}");
            assert!(
                !ws.read_only(&format!("{ASSET}/candidates/r_1/{name}")),
                "call {fail_at}: {name} came back read-only"
            );
        }
        assert!(
            ws.seal_folders().is_empty(),
            "call {fail_at}: {:?}",
            ws.seal_folders()
        );
        // A later seal of the same version works.
        let m = ws.manifest().unwrap();
        seal(&ws.plain(), request("r_1", "1.0.0", &["a.png", "b.png"], m))
            .await
            .unwrap_or_else(|e| panic!("call {fail_at}: the retry: {e}"));
    }
}

#[tokio::test(start_paused = true)] // spec: CORE-041
async fn seal_reports_post_move_identities() {
    let ws = workspace();
    let m = ws.run(
        "r_1",
        &[
            ("a.png", "candidate", None),
            ("a_cut.png", "derived", Some("a.png")),
            ("source/a.psd", "", None),
        ],
    );
    let store = ws.plain();
    let sealed = seal(&store, request("r_1", "1.0.0", &["a.png"], m))
        .await
        .expect("the seal");
    let mut paths: Vec<&str> = sealed.identities.iter().map(|(p, _)| p.as_str()).collect();
    paths.sort_unstable();
    let want: Vec<String> = [
        "a.png",
        "a_cut.png",
        "manifest.snapshot.json",
        "source/a.psd",
    ]
    .iter()
    .map(|n| format!("{ASSET}/v1.0.0/{n}"))
    .collect();
    assert_eq!(paths, want);
    for (path, identity) in &sealed.identities {
        let fresh = SystemProbe.identity(&ws.file(path.as_str())).unwrap();
        assert_eq!(*identity, fresh, "{path}");
        assert_ne!(identity.attributes & FILE_ATTRIBUTE_READONLY, 0, "{path}");
    }
}

#[tokio::test(start_paused = true)] // spec: CORE-026
async fn writes_into_sealed_folders_are_refused() {
    let ws = workspace();
    let m = ws.run("r_1", &[("a.png", "candidate", None)]);
    let store = ws.plain();
    seal(&store, request("r_1", "1.0.0", &["a.png"], m))
        .await
        .expect("the seal");
    let sealed = format!("{ASSET}/v1.0.0/a.png");
    let before = ws.read(&sealed);
    for (rel, expect) in [
        (sealed.clone(), Expect::Generated),
        (format!("{ASSET}/V1.0.0/a.png"), Expect::Generated),
        (format!("{ASSET}/v1.0.0/new.txt"), Expect::New),
        (format!("{ASSET}/v1.0.0/sub/new.txt"), Expect::New),
    ] {
        let result = store.write(&p(&rel), b"over".to_vec(), expect).await;
        let Err(e) = result else {
            panic!("{rel} was written");
        };
        assert_eq!(e.to_error("t").code, "store.path_invalid", "{rel}: {e}");
    }
    let edit = store
        .edit_text(&p(&format!("{ASSET}/v1.0.0/notes.txt")), &|t| {
            Ok(format!("{t}x"))
        })
        .await;
    assert!(
        matches!(edit, Err(StoreError::PathInvalid { .. })),
        "{edit:?}"
    );
    assert_eq!(ws.read(&sealed), before);
    assert_eq!(
        ws.tree(&format!("{ASSET}/v1.0.0")),
        ["a.png", "manifest.snapshot.json"]
    );
}

#[tokio::test(start_paused = true)] // spec: CORE-035
async fn locked_paths_are_refused_with_store_locked() {
    let ws = workspace();
    let mut m = Manifest::new(
        PROJECT,
        ASSET_ID,
        "image",
        "00000000-0000-4000-8000-0000000000aa",
    );
    m.set("locks", json!(["prompts/crane.txt", "work/held"]));
    write_file(&ws.file(MANIFEST), m.to_text().as_bytes());
    write_file(&ws.file(&format!("{ASSET}/prompts/crane.txt")), b"a crane");
    let store = ws.plain();

    for rel in [
        format!("{ASSET}/prompts/crane.txt"),
        format!("{ASSET}/Prompts/CRANE.txt"),
        format!("{ASSET}/work/held/a.txt"),
        format!("{ASSET}/work/held"),
    ] {
        let result = store
            .write(&p(&rel), b"over".to_vec(), Expect::Generated)
            .await;
        let Err(e) = result else {
            panic!("{rel} was written");
        };
        assert!(
            matches!(&e, StoreError::Locked(path) if path.as_str() == rel),
            "{rel}: {e:?}"
        );
        assert_eq!(e.to_error("t").code, "store.locked", "{rel}");
    }
    let edit = store
        .edit_text(&p(&format!("{ASSET}/prompts/crane.txt")), &|t| {
            Ok(format!("{t}!"))
        })
        .await;
    assert!(matches!(edit, Err(StoreError::Locked(_))), "{edit:?}");
    assert_eq!(ws.read(&format!("{ASSET}/prompts/crane.txt")), b"a crane");

    // Paths that aren't locked are written: a sibling, a name that only starts like a lock,
    // and the same path in another asset.
    for rel in [
        format!("{ASSET}/prompts/other.txt"),
        format!("{ASSET}/work/held2/a.txt"),
        "projects/sketchbook/assets/kite/prompts/crane.txt".to_string(),
    ] {
        store
            .write(&p(&rel), b"fine".to_vec(), Expect::New)
            .await
            .unwrap_or_else(|e| panic!("{rel}: {e}"));
    }
    // The manifest itself stays editable.
    store
        .edit_manifest(&p(MANIFEST), &|m| {
            m.set("notes", Value::from("edited"));
            Ok(())
        })
        .await
        .expect("the manifest edit");
}

#[tokio::test(start_paused = true)] // spec: CORE-033
async fn legacy_folder_is_listed_as_legacy() {
    let env = TestEnv::with_label("Ünïcödé space");
    let root = env.copy_fixture_workspace("legacy");
    let ws = Ws::at(env, root);
    let lamp = "projects/sketchbook/assets/prop_signal_lamp";
    let before = ws.tree(&format!("{lamp}/v1.0.0"));
    let store = ws.plain();

    let versions = store.versions(PROJECT, "prop_signal_lamp").await.unwrap();
    assert_eq!(versions.len(), 1, "{versions:?}");
    let legacy = &versions[0];
    assert_eq!(legacy.version, v("1.0.0"));
    assert!(legacy.legacy, "{legacy:?}");
    assert_eq!(legacy.dir.as_str(), format!("{lamp}/v1.0.0"));
    assert_eq!(
        legacy.content_hash,
        version_content_hash_of(&ws.file(&format!("{lamp}/v1.0.0")))
    );
    // Read as it is: nothing moved, nothing made read-only.
    assert_eq!(ws.tree(&format!("{lamp}/v1.0.0")), before);
    for name in &before {
        assert!(!ws.read_only(&format!("{lamp}/v1.0.0/{name}")), "{name}");
    }

    // A sealed version with its snapshot and run is not legacy.
    let crane = store.versions(PROJECT, "chr_paper_crane").await.unwrap();
    assert_eq!(crane.len(), 1, "{crane:?}");
    assert!(!crane[0].legacy, "{crane:?}");
    assert_eq!(crane[0].version, v("1.0.0"));
    assert!(
        store
            .versions(PROJECT, "no_such_asset")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(ws.env.root().exists());
}

#[tokio::test(start_paused = true)] // spec: CORE-026
async fn seal_refuses_a_guard_that_isnt_the_assets_version_lock() {
    let ws = workspace();
    let m = ws.run("r_1", &[("a.png", "candidate", None)]);
    let store = ws.plain();
    let other_asset = store.version_lock(PROJECT, "kite").lock_owned().await;
    let stray = Arc::new(tokio::sync::Mutex::new(())).lock_owned().await;
    for guard in [&other_asset, &stray] {
        let refused = store
            .seal(guard, request("r_1", "1.0.0", &["a.png"], m.clone()))
            .await;
        let Err(StoreError::Io { why, .. }) = &refused else {
            panic!("a seal under another lock ran: {refused:?}");
        };
        assert!(why.contains("version lock"), "{why}");
    }
    assert!(!ws.exists(&format!("{ASSET}/v1.0.0")));
    assert_eq!(ws.tree(&format!("{ASSET}/candidates/r_1")), ["a.png"]);
    assert!(ws.seal_folders().is_empty(), "{:?}", ws.seal_folders());
    // The asset's own lock, spelled another way, is the same lock.
    let own = store.version_lock("Sketchbook", "CRANE").lock_owned().await;
    store
        .seal(&own, request("r_1", "1.0.0", &["a.png"], m))
        .await
        .expect("the seal under the asset's lock");
}

/// The real probe, except that it reports the files named `link_name` as symbolic links and
/// those named `cloud_name` as cloud-only placeholders, which it refuses to open.
#[derive(Debug)]
struct FakeEntries {
    link_name: &'static str,
    cloud_name: &'static str,
}

impl FakeEntries {
    fn named(&self, path: &Path) -> (bool, bool) {
        let name = path.file_name().unwrap().to_string_lossy();
        (name == self.link_name, name == self.cloud_name)
    }
}

impl Probe for FakeEntries {
    fn identity(&self, path: &Path) -> io::Result<ww_platform::fs::FileIdentity> {
        let mut id = SystemProbe.identity(path)?;
        let (link, cloud) = self.named(path);
        if link {
            id.attributes |= 0x400; // FILE_ATTRIBUTE_REPARSE_POINT
        }
        if cloud {
            id.attributes |= ww_platform::fs::PLACEHOLDER_ATTRIBUTES;
        }
        Ok(id)
    }

    fn volume_info(&self, path: &Path) -> io::Result<ww_platform::fs::VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<std::fs::File> {
        assert!(
            !self.named(path).1,
            "a placeholder was opened: {}",
            path.display()
        );
        SystemProbe.open_read(path)
    }

    fn reparse_tag(&self, path: &Path) -> io::Result<Option<u32>> {
        if self.named(path).0 {
            return Ok(Some(0xA000_000C)); // IO_REPARSE_TAG_SYMLINK
        }
        SystemProbe.reparse_tag(path)
    }
}

#[tokio::test(start_paused = true)] // spec: CORE-026, CORE-028
async fn links_and_placeholders_inside_source_are_refused() {
    for (link_name, cloud_name, want) in [
        ("inner.psd", "", "store.path_invalid"),
        ("", "inner.psd", "store.hash_conflict"),
    ] {
        let ws = workspace();
        let m = ws.run(
            "r_1",
            &[
                ("a.png", "candidate", None),
                ("source/top.txt", "", None),
                ("source/sub/inner.psd", "", None),
            ],
        );
        let store = Arc::new(
            Store::new(
                ws.root.clone(),
                ws.state.clone(),
                FailPoints::none(),
                ws.clock.clone(),
                Arc::new(SeqIds::new()),
                Arc::new(FakeEntries {
                    link_name,
                    cloud_name,
                }),
            )
            .with_machine("testmachine"),
        );
        let refused = seal(&store, request("r_1", "1.0.0", &["a.png"], m)).await;
        let Err(e) = refused else {
            panic!("{want}: the seal went ahead");
        };
        assert_eq!(e.to_error("t").code, want, "{e}");
        assert!(
            e.to_string()
                .contains(&format!("{ASSET}/candidates/r_1/source/sub/inner.psd")),
            "{e}"
        );
        // Refused before anything moved.
        assert!(!ws.exists(&format!("{ASSET}/v1.0.0")));
        assert_eq!(
            ws.tree(&format!("{ASSET}/candidates/r_1")),
            ["a.png", "source/sub/inner.psd", "source/top.txt"]
        );
        assert!(ws.seal_folders().is_empty(), "{:?}", ws.seal_folders());
    }
}

#[tokio::test(start_paused = true)] // spec: CORE-026, CORE-038, QA-066
async fn crash_after_seal_leaves_the_version_as_an_orphan() {
    let ws = workspace();
    let m = ws.run("r_1", &[("a.png", "candidate", None)]);
    let fp = Arc::new(FailPoints::parse("store.after_seal=io").unwrap());
    let store = ws.store(fp);
    let result = seal(&store, request("r_1", "1.0.0", &["a.png"], m)).await;
    assert!(matches!(result, Err(StoreError::Io { .. })), "{result:?}");

    // The version stays whole, sealed and read-only; nothing went back to the run.
    assert_eq!(
        ws.tree(&format!("{ASSET}/v1.0.0")),
        ["a.png", "manifest.snapshot.json"]
    );
    for name in ["a.png", "manifest.snapshot.json"] {
        assert!(ws.read_only(&format!("{ASSET}/v1.0.0/{name}")), "{name}");
    }
    assert!(ws.tree(&format!("{ASSET}/candidates/r_1")).is_empty());
    assert!(ws.seal_folders().is_empty(), "{:?}", ws.seal_folders());
    // No manifest records it: it is an orphan, listed and kept.
    let orphans = store.orphans().await.unwrap();
    assert_eq!(
        orphans.iter().map(WsPath::as_str).collect::<Vec<_>>(),
        [format!("{ASSET}/v1.0.0").as_str()]
    );
    assert!(ws.exists(&format!("{ASSET}/v1.0.0/a.png")));
}
