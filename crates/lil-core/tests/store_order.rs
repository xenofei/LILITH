//! The store's write order, areas and logs (ARCH-028, ARCH-033, ARCH-075, ARCH-080, ARCH-081,
//! CORE-003, CORE-009, CORE-038): `commit` in the ARCH-075 order with its fail points, orphans,
//! the locked per-machine monthly append logs, staging handles and run ids.
//!
//! Every test runs on tokio's paused clock or a `FakeClock`, so retry budgets pass without
//! real sleeps (QA-031). Fixture files are made with `lil_testkit::files::write_file` and
//! folders with `store::fsops::ensure_dir`; the one outside lock holder carries its own
//! allowance.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lil_core::state::{StateDb, StateScope};
use lil_core::store::{
    ChangeKind, ChangeSink, CommitOp, FailPoints, LogKind, ManifestEdit, OutputMove, Store,
    StoreError, StoreEvent, TextEdit, Written, WsPath, append_loose_usage, fsops,
};
use lil_core::workspace::Access;
use lil_platform::fs::{Probe, SystemProbe};
use lil_schema::Manifest;
use lil_schema::logs::read_lines;
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, SeqIds, TestEnv, sha256_bytes, utc};
use serde_json::{Map, Value, json};

// ---------------------------------------------------------------------------------------------
// Fixture.
// ---------------------------------------------------------------------------------------------

/// The fixture asset of these tests.
const ASSET: &str = "projects/sketchbook/assets/crane";
/// Its manifest.
const MANIFEST: &str = "projects/sketchbook/assets/crane/crane.manifest.json";
/// The month of `FakeClock`'s UTC start (2027-01-15T08:00:00Z).
const MONTH: &str = "2027-01";

/// A workspace folder and its `state.db`, both under the test's own root.
struct Ws {
    root: PathBuf,
    state: Arc<StateDb>,
    clock: Arc<FakeClock>,
    /// Last: fields drop in order, and the folder cannot go while `state` holds state.db open.
    env: TestEnv,
}

fn no_fatal() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|what: &str| panic!("unexpected fatal report: {what}"))
}

fn workspace() -> Ws {
    let env = TestEnv::with_label("Ünïcödé space");
    let root = std::fs::canonicalize(env.make_workspace("order")).unwrap();
    let text = root.to_string_lossy().into_owned();
    let root = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text));
    let db = env.root().join("local-ws").join("state.db");
    let state = Arc::new(StateDb::open(&db, StateScope::Workspace, no_fatal()).expect("state.db"));
    Ws {
        root,
        state,
        clock: FakeClock::new(),
        env,
    }
}

impl Ws {
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

    fn text(&self, rel: &str) -> String {
        String::from_utf8(self.read(rel)).unwrap()
    }

    fn exists(&self, rel: &str) -> bool {
        self.file(rel).exists()
    }

    /// The asset's manifest, with no runs yet.
    fn write_manifest(&self) {
        let m = Manifest::new(
            "sketchbook",
            "crane",
            "image",
            "00000000-0000-4000-8000-0000000000aa",
        );
        write_file(&self.file(MANIFEST), m.to_text().as_bytes());
    }
}

fn p(rel: &str) -> WsPath {
    WsPath::parse(rel).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn record(v: Value) -> Map<String, Value> {
    v.as_object().expect("an object").clone()
}

fn jobs_log(month: &str) -> String {
    format!("queue/jobs.testmachine.{month}.jsonl")
}

/// An op that ingests `name` from `from` into run `run` of the fixture asset: the output, the
/// manifest's run entry, a jobs line and a progress entry.
fn ingest_op(from: PathBuf, run: &str, name: &str) -> CommitOp {
    let (run_id, file) = (run.to_string(), name.to_string());
    let manifest: ManifestEdit = Arc::new(move |m: &mut Manifest| {
        let entry = json!({
            "run_id": run_id,
            "path": format!("candidates/{run_id}/"),
            "status": "open",
            "files": [{"path": format!("candidates/{run_id}/{file}"), "role": "candidate"}],
        });
        m.push("candidate_runs", entry)
            .map_err(|e| StoreError::manifest_edit(&p(MANIFEST), &e))
    });
    let progress: TextEdit = Arc::new(|t: &str| Ok(format!("{t}- 2027-01-15: a run\n")));
    CommitOp {
        outputs: vec![OutputMove {
            from,
            to: p(&format!("{ASSET}/candidates/{run}/{name}")),
        }],
        moved: Vec::new(),
        removed: Vec::new(),
        manifest: Some((p(MANIFEST), manifest)),
        job_line: Some(record(json!({"kind": "ingest", "run_id": run}))),
        progress: Some((p("projects/sketchbook/progress.md"), progress)),
    }
}

/// A file written into a fresh staging handle of `store`, as an engine would.
async fn staged(store: &Store, name: &str, bytes: &[u8]) -> PathBuf {
    let handle = store.staging_handle().await.expect("a staging handle");
    let path = handle.dir.join(name);
    write_file(&path, bytes);
    path
}

/// Records every `applied` call, with what the hooks saw before it.
#[derive(Default)]
struct Recorder {
    steps: Mutex<Vec<String>>,
    applied: Mutex<Vec<Vec<StoreEvent>>>,
}

impl ChangeSink for Recorder {
    fn applied<'a>(
        &'a self,
        events: &'a [StoreEvent],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.steps.lock().unwrap().push("index".to_string());
        self.applied.lock().unwrap().push(events.to_vec());
        Box::pin(std::future::ready(()))
    }
}

// ---------------------------------------------------------------------------------------------
// Write order (ARCH-075).
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-075
async fn commit_writes_output_manifest_log_progress_then_index() {
    let ws = Arc::new(workspace());
    ws.write_manifest();
    let recorder = Arc::new(Recorder::default());
    let fp = FailPoints::none();
    for point in [
        "store.after_output",
        "store.after_manifest",
        "store.after_jobs_log",
        "store.after_progress",
        "store.after_index",
    ] {
        let (ws, recorder) = (ws.clone(), recorder.clone());
        fp.on(
            point,
            Box::new(move || {
                // What is on disk when each point is reached.
                let output = ws.exists(&format!("{ASSET}/candidates/r_1/a.png"));
                let manifest = ws.text(MANIFEST).contains("r_1");
                let log = ws.exists(&jobs_log(MONTH));
                let progress = ws.exists("projects/sketchbook/progress.md");
                recorder.steps.lock().unwrap().push(format!(
                    "{point} output={output} manifest={manifest} log={log} progress={progress}"
                ));
                Ok(())
            }),
        );
    }
    let store = ws.store(fp);
    store.set_sink(recorder.clone());

    // A sealed file a caller already moved into place: it reaches the index only in step 5.
    let sealed = format!("{ASSET}/v1.1.0/x.png");
    write_file(&ws.file(&sealed), b"sealed");
    let identity = SystemProbe.identity(&ws.file(&sealed)).unwrap();
    let moved = Written {
        path: WsPath::parse(&sealed).unwrap(),
        sha256: sha256_bytes(b"sealed"),
        identity,
    };
    let from = staged(&store, "a.png", b"a").await;
    let mut op = ingest_op(from, "r_1", "a.png");
    op.moved = vec![moved.clone()];

    let report = store.commit(op).await.expect("the commit");
    assert_eq!(
        *recorder.steps.lock().unwrap(),
        [
            "store.after_output output=true manifest=false log=false progress=false",
            "store.after_manifest output=true manifest=true log=false progress=false",
            "store.after_jobs_log output=true manifest=true log=true progress=false",
            "store.after_progress output=true manifest=true log=true progress=true",
            "index",
            "store.after_index output=true manifest=true log=true progress=true",
        ]
    );

    let applied = recorder.applied.lock().unwrap().clone();
    assert_eq!(applied.len(), 1, "one index step: {applied:?}");
    let events = &applied[0];
    let paths: Vec<&str> = events.iter().map(|e| e.path.as_str()).collect();
    let output = format!("{ASSET}/candidates/r_1/a.png");
    let log = jobs_log(MONTH);
    assert_eq!(
        paths,
        [
            output.as_str(),
            MANIFEST,
            log.as_str(),
            "projects/sketchbook/progress.md",
            sealed.as_str()
        ]
    );
    let sealed_event = &events[4];
    assert_eq!(sealed_event.kind, ChangeKind::Written);
    assert_eq!(sealed_event.sha256.as_deref(), Some(moved.sha256.as_str()));
    assert_eq!(
        sealed_event.identity,
        Some(identity),
        "the identity as given"
    );

    assert_eq!(report.outputs.len(), 1);
    assert_eq!(report.outputs[0].sha256, sha256_bytes(b"a"));
    assert_eq!(events[0].identity, Some(report.outputs[0].identity));
    assert_eq!(report.job_seq, Some(1));
    let manifest = report.manifest.expect("the edited manifest");
    assert_eq!(manifest.view().candidate_runs[0].run_id, "r_1");
    assert_eq!(ws.read(&output), b"a");
    assert!(ws.text("projects/sketchbook/progress.md").contains("a run"));
}

#[tokio::test(start_paused = true)] // spec: ARCH-075, CORE-038
async fn error_after_output_leaves_a_listed_orphan_never_deleted() {
    let ws = workspace();
    ws.write_manifest();
    let fp = Arc::new(FailPoints::parse("store.after_output=io").unwrap());
    let store = ws.store(fp);
    let from = staged(&store, "a.png", b"kept").await;
    let err = store
        .commit(ingest_op(from, "r_1", "a.png"))
        .await
        .expect_err("the fail point stops the commit before the manifest edit");
    assert_eq!(err.to_error("t").code, "store.io", "{err}");

    // Nothing is ever deleted: the output stays where step 1 put it.
    let output = format!("{ASSET}/candidates/r_1/a.png");
    assert_eq!(ws.read(&output), b"kept");
    // The manifest doesn't record the run, so its folder is an orphan.
    let orphans = ws.plain().orphans().await.unwrap();
    assert!(
        orphans.contains(&p(&format!("{ASSET}/candidates/r_1"))),
        "{orphans:?}"
    );
    assert!(!ws.text(MANIFEST).contains("r_1"));
    // The later steps never ran.
    assert!(!ws.exists(&jobs_log(MONTH)));
    assert!(!ws.exists("projects/sketchbook/progress.md"));
}

#[tokio::test(start_paused = true)] // spec: ARCH-075, CORE-020
async fn failures_after_the_manifest_edit_are_warnings_and_every_step_still_runs() {
    // Ruling Q31: once the manifest records the operation, the jobs line, `progress.md`, the
    // index and the fail points between them are best-effort.
    for point in [
        "store.after_manifest",
        "store.after_jobs_log",
        "store.after_progress",
        "store.after_index",
    ] {
        let ws = workspace();
        ws.write_manifest();
        let fp = Arc::new(FailPoints::parse(&format!("{point}=io")).unwrap());
        let store = ws.store(fp);
        let recorder = Arc::new(Recorder::default());
        store.set_sink(recorder.clone());
        let from = staged(&store, "a.png", b"kept").await;
        let report = store
            .commit(ingest_op(from, "r_1", "a.png"))
            .await
            .unwrap_or_else(|e| panic!("{point}: the operation is recorded, not failed: {e}"));
        assert_eq!(report.warnings.len(), 1, "{point}: {:?}", report.warnings);
        assert!(
            report.warnings[0].contains(point),
            "{point}: {:?}",
            report.warnings
        );
        let output = format!("{ASSET}/candidates/r_1/a.png");
        assert_eq!(ws.read(&output), b"kept", "{point}");
        assert!(ws.text(MANIFEST).contains("r_1"), "{point}");
        // The steps after the failed one still ran, and the index learnt all of them.
        assert!(ws.exists(&jobs_log(MONTH)), "{point}");
        assert!(
            ws.text("projects/sketchbook/progress.md").contains("a run"),
            "{point}"
        );
        assert_eq!(report.job_seq, Some(1), "{point}");
        let applied = recorder.applied.lock().unwrap().clone();
        assert_eq!(applied.len(), 1, "{point}: {applied:?}");
        let paths: Vec<&str> = applied[0].iter().map(|e| e.path.as_str()).collect();
        let log = jobs_log(MONTH);
        assert_eq!(
            paths,
            [
                output.as_str(),
                MANIFEST,
                log.as_str(),
                "projects/sketchbook/progress.md"
            ],
            "{point}"
        );
        let orphans = ws.plain().orphans().await.unwrap();
        assert!(orphans.is_empty(), "{point}: {orphans:?}");
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-075, CORE-039
async fn a_cloud_only_progress_md_is_a_warning_and_the_index_still_learns() {
    let ws = workspace();
    ws.write_manifest();
    write_file(
        &ws.file("projects/sketchbook/progress.md"),
        b"# Progress\n\n## Done\n",
    );
    let progress = ws.file("projects/sketchbook/progress.md");
    let store = Arc::new(
        Store::new(
            ws.root.clone(),
            ws.state.clone(),
            FailPoints::none(),
            ws.clock.clone(),
            Arc::new(SeqIds::new()),
            Arc::new(PlaceholderAt(progress.clone())),
        )
        .with_machine("testmachine"),
    );
    let recorder = Arc::new(Recorder::default());
    store.set_sink(recorder.clone());
    let from = staged(&store, "a.png", b"kept").await;
    let report = store
        .commit(ingest_op(from, "r_1", "a.png"))
        .await
        .expect("a cloud-only progress.md doesn't fail a recorded operation");
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("progress.md"),
        "{:?}",
        report.warnings
    );
    // The placeholder was never read or written.
    assert_eq!(
        std::fs::read(&progress).unwrap(),
        b"# Progress\n\n## Done\n"
    );
    assert!(ws.text(MANIFEST).contains("r_1"));
    let applied = recorder.applied.lock().unwrap().clone();
    assert_eq!(applied.len(), 1, "{applied:?}");
    let output = format!("{ASSET}/candidates/r_1/a.png");
    assert!(
        applied[0].iter().any(|e| e.path.as_str() == output),
        "{applied:?}"
    );
}

/// The real probe, except that one path reports the cloud placeholder attributes and its data
/// is never opened.
#[derive(Debug)]
struct PlaceholderAt(PathBuf);

impl Probe for PlaceholderAt {
    fn identity(&self, path: &Path) -> io::Result<lil_platform::fs::FileIdentity> {
        let mut id = SystemProbe.identity(path)?;
        if path == self.0 {
            id.attributes |= lil_platform::fs::PLACEHOLDER_ATTRIBUTES;
        }
        Ok(id)
    }

    fn volume_info(&self, path: &Path) -> io::Result<lil_platform::fs::VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<std::fs::File> {
        assert_ne!(path, self.0, "a placeholder's data was opened");
        SystemProbe.open_read(path)
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-083, ARCH-075
async fn commit_outputs_wait_at_the_git_gate() {
    let ws = workspace();
    ws.write_manifest();
    let store = ws.plain();
    let from = staged(&store, "a.png", b"a").await;
    let lock = ws.file(".git/index.lock");
    write_file(&lock, b"");
    let commit = {
        let store = store.clone();
        tokio::spawn(async move { store.commit(ingest_op(from, "r_1", "a.png")).await })
    };
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(!commit.is_finished(), "the commit waits while git works");
    let output = format!("{ASSET}/candidates/r_1/a.png");
    assert!(!ws.exists(&output), "no output is moved meanwhile");
    // Still busy after 60 s: `store.git_busy`, and nothing moved.
    let err = commit
        .await
        .unwrap()
        .expect_err("git is still working after 60 s");
    assert_eq!(err.to_error("t").code, "store.git_busy", "{err}");
    assert!(!ws.exists(&output));
    assert!(!ws.text(MANIFEST).contains("r_1"));
}

#[tokio::test(start_paused = true)] // spec: ARCH-028
async fn a_read_only_store_never_writes_or_resolves() {
    let ws = workspace();
    let store = ws.plain();
    store
        .write(
            &p("notes/a.txt"),
            b"v1".to_vec(),
            lil_core::store::Expect::New,
        )
        .await
        .unwrap();
    // A swap that stopped between the fallback's renames: the target is missing, the earlier
    // content is in a backup, and the pending row stays.
    let fp = Arc::new(FailPoints::parse("store.replace=unsupported").unwrap());
    fp.on(
        "store.between_renames",
        Box::new(|| Err(io::Error::other("the process stops here"))),
    );
    ws.store(fp)
        .write(
            &p("notes/a.txt"),
            b"v2".to_vec(),
            lil_core::store::Expect::Generated,
        )
        .await
        .expect_err("the write stops");
    assert!(!ws.exists("notes/a.txt"));
    let names = || -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(ws.file("notes"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let before = names();
    assert_eq!(before.len(), 1, "{before:?}");

    let read_only = Arc::new(
        Store::new(
            ws.root.clone(),
            ws.state.clone(),
            FailPoints::none(),
            ws.clock.clone(),
            Arc::new(SeqIds::new()),
            Arc::new(SystemProbe),
        )
        .with_access(Access::ReadOnly),
    );
    assert_eq!(read_only.access(), Access::ReadOnly);
    let err = read_only
        .read(&p("notes/a.txt"))
        .await
        .expect_err("a read-only store doesn't resolve the row");
    assert_eq!(err.to_error("t").code, "store.io", "{err}");
    assert!(
        err.to_string().contains(&format!("notes/{}", before[0])),
        "the error names the backup: {err}"
    );
    assert_eq!(names(), before, "nothing was moved");
    assert!(
        ws.state.pending("notes/a.txt").await.unwrap().is_some(),
        "the row stays"
    );

    // It never writes: every write call is `workspace.read_only`.
    let refused = [
        read_only
            .write(
                &p("notes/b.txt"),
                b"x".to_vec(),
                lil_core::store::Expect::New,
            )
            .await
            .unwrap_err(),
        read_only
            .edit_text(&p("notes/c.txt"), &|t| Ok(format!("{t}x")))
            .await
            .unwrap_err(),
        read_only
            .append_log(LogKind::Jobs, record(json!({"a": 1})))
            .await
            .unwrap_err(),
        read_only.staging_handle().await.unwrap_err(),
        read_only.recover().await.unwrap_err(),
    ];
    for err in refused {
        assert_eq!(err, StoreError::ReadOnly);
        assert_eq!(err.to_error("t").code, "workspace.read_only");
    }
    assert!(!ws.exists("notes/b.txt") && !ws.exists("queue"));

    // The read-write store resolves it.
    assert_eq!(
        ws.plain().read(&p("notes/a.txt")).await.unwrap().as_deref(),
        Some(&b"v1"[..])
    );
}

// ---------------------------------------------------------------------------------------------
// Append logs (ARCH-081, CORE-009).
// ---------------------------------------------------------------------------------------------

/// Opens `path` (created if missing) and holds its append lock, as `lil` or another core would.
#[allow(clippy::disallowed_methods)] // an outside appender holding the shared lock
fn open_log(path: &Path) -> std::fs::File {
    fsops::ensure_dir(path.parent().unwrap()).unwrap();
    std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)
        .unwrap()
}

#[allow(clippy::disallowed_methods)] // the outside appender's lock
fn hold_append_lock(file: &std::fs::File) -> lil_platform::fs::AppendLock<'_> {
    lil_platform::fs::try_lock_append(file)
        .unwrap()
        .expect("the lock is free")
}

#[tokio::test(start_paused = true)] // spec: ARCH-081, CORE-009
async fn append_takes_the_shared_lock_and_numbers_lines() {
    let ws = workspace();
    // Two appenders (two stores over one workspace) at once: one seq each, never the same.
    let (a, b) = (ws.plain(), ws.plain());
    let (one, two) = tokio::join!(
        a.append_log(LogKind::Jobs, record(json!({"n": "a"}))),
        b.append_log(LogKind::Jobs, record(json!({"n": "b"}))),
    );
    let mut seqs = vec![one.unwrap(), two.unwrap()];
    seqs.sort_unstable();
    assert_eq!(seqs, [1, 2]);

    // Another holder of the lock delays the append until it lets go.
    let log = ws.file(&jobs_log(MONTH));
    let file = open_log(&log);
    let held = hold_append_lock(&file);
    let pending = tokio::spawn({
        let a = a.clone();
        async move { a.append_log(LogKind::Jobs, record(json!({"n": "c"}))).await }
    });
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        !pending.is_finished(),
        "the append went ahead under another's lock"
    );
    assert_eq!(read_lines(&ws.text(&jobs_log(MONTH))).len(), 2);
    drop(held);
    assert_eq!(pending.await.unwrap().unwrap(), 3);

    // Held past the 10 s budget: `store.busy`, and nothing is written.
    let held = hold_append_lock(&file);
    let busy = a
        .append_log(LogKind::Jobs, record(json!({"n": "d"})))
        .await
        .expect_err("held past the budget");
    drop(held);
    assert_eq!(busy.to_error("t").code, "store.busy", "{busy}");

    let text = ws.text(&jobs_log(MONTH));
    assert!(text.ends_with('\n'), "{text:?}");
    let lines = read_lines(&text);
    let got: Vec<(String, u64)> = lines.iter().map(|l| (l.machine.clone(), l.seq)).collect();
    assert_eq!(
        got,
        [
            ("testmachine".to_string(), 1),
            ("testmachine".to_string(), 2),
            ("testmachine".to_string(), 3)
        ]
    );
    for raw in text.lines() {
        let value: Value = serde_json::from_str(raw).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .take(2)
            .collect();
        assert_eq!(keys, ["machine", "seq"], "{raw}");
    }
    assert_eq!(lines[2].record, json!({"n": "c"}));
}

#[tokio::test(start_paused = true)] // spec: ARCH-081
async fn month_rollover_starts_a_new_file() {
    let ws = workspace();
    let store = ws.plain();
    assert_eq!(
        store
            .append_log(LogKind::Jobs, record(json!({"m": 1})))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .append_log(LogKind::Usage, record(json!({"m": 1})))
            .await
            .unwrap(),
        1
    );
    // 2027-01-15 + 20 days is in February (UTC).
    ws.clock.shift_utc(20 * 24 * 60 * 60);
    assert_eq!(
        store
            .append_log(LogKind::Jobs, record(json!({"m": 2})))
            .await
            .unwrap(),
        1
    );
    for (rel, n) in [
        (jobs_log("2027-01"), 1),
        (jobs_log("2027-02"), 1),
        ("registry/usage.testmachine.2027-01.jsonl".to_string(), 1),
    ] {
        assert_eq!(read_lines(&ws.text(&rel)).len(), n, "{rel}");
    }
}

#[tokio::test(start_paused = true)] // spec: ARCH-081, CORE-009
async fn torn_last_line_is_closed_and_skipped_by_readers() {
    let ws = workspace();
    let store = ws.plain();
    for n in 1..=2 {
        assert_eq!(
            store
                .append_log(LogKind::Jobs, record(json!({"n": n})))
                .await
                .unwrap(),
            n
        );
    }
    // An appender died mid-line; another line was copied in twice (a sync conflict).
    let rel = jobs_log(MONTH);
    let mut text = ws.text(&rel);
    let first = text.lines().next().unwrap().to_string();
    text.push_str(&first);
    text.push('\n');
    text.push_str(r#"{"machine":"testmachine","seq":3,"n""#);
    write_file(&ws.file(&rel), text.as_bytes());

    assert_eq!(
        store
            .append_log(LogKind::Jobs, record(json!({"n": 3})))
            .await
            .unwrap(),
        3
    );
    let text = ws.text(&rel);
    assert!(text.ends_with('\n'), "{text:?}");
    let raw: Vec<&str> = text.lines().collect();
    assert_eq!(raw.len(), 5, "{text}");
    assert_eq!(
        raw[3], r#"{"machine":"testmachine","seq":3,"n""#,
        "the fragment stays alone"
    );
    let lines = read_lines(&text);
    let seqs: Vec<u64> = lines.iter().map(|l| l.seq).collect();
    assert_eq!(seqs, [1, 2, 3], "readers skip the fragment and dedupe");
    assert_eq!(lines[2].record, json!({"n": 3}));
}

#[tokio::test(start_paused = true)] // spec: ARCH-081
async fn loose_usage_goes_to_the_local_folder() {
    let ws = workspace();
    let env = &ws.env.env;
    assert_eq!(
        append_loose_usage(env, record(json!({"u": 1})))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        append_loose_usage(env, record(json!({"u": 2})))
            .await
            .unwrap(),
        2
    );
    let loose = env.local_dir().join("usage-loose.jsonl");
    let lines = read_lines(&std::fs::read_to_string(&loose).unwrap());
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|l| l.machine == "testmachine"));
    assert!(!ws.exists("registry"), "nothing goes into the workspace");
}

// ---------------------------------------------------------------------------------------------
// Staging handles, run ids and orphans (ARCH-080, CORE-003, CORE-038).
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-080, CORE-003, ARCH-033
async fn staging_handles_are_in_the_workspace_and_ingest_keeps_file_ids() {
    let ws = workspace();
    ws.write_manifest();
    let store = ws.plain();
    let handle = store.staging_handle().await.unwrap();
    let hex = handle.id.strip_prefix("sh_").expect("sh_<16 hex>");
    assert!(
        hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "{}",
        handle.id
    );
    assert_eq!(
        handle.dir,
        ws.root.join(".lilith").join("staging").join(&handle.id)
    );
    assert!(handle.dir.is_dir());

    let staged = handle.dir.join("a.png");
    write_file(&staged, b"engine output");
    let before = SystemProbe.identity(&staged).unwrap();
    let root = SystemProbe.identity(&ws.root).unwrap();
    assert_eq!(
        before.volume_serial, root.volume_serial,
        "staging is on the workspace volume"
    );

    let report = store
        .commit(ingest_op(staged, "r_1", "a.png"))
        .await
        .unwrap();
    let moved = ws.file(&format!("{ASSET}/candidates/r_1/a.png"));
    assert_eq!(std::fs::read(&moved).unwrap(), b"engine output");
    let after = SystemProbe.identity(&moved).unwrap();
    assert_eq!(after.volume_serial, root.volume_serial);
    if SystemProbe.volume_info(&ws.root).unwrap().fs_name == "NTFS" {
        // A rename keeps the file id on NTFS; on ReFS a move across folders changes it.
        assert_eq!(after.file_id, before.file_id, "ingest is a rename");
    }
    assert_eq!(
        report.outputs[0].identity, after,
        "the identity after the move"
    );

    store.release_handle(handle.clone()).await.unwrap();
    assert!(!handle.dir.exists());
}

#[tokio::test(start_paused = true)] // spec: CORE-047, CORE-038
async fn run_id_skips_orphan_candidate_folders() {
    let ws = workspace();
    let store = ws.plain();
    let asset = p(ASSET);
    let noon = utc::parse_rfc3339("2026-10-16T12:00:00Z");
    assert_eq!(
        store.next_run_id(&asset, None, noon).await.unwrap(),
        "r_20261016_1200"
    );

    // An orphan folder from a crashed run takes the plain id.
    write_file(
        &ws.file(&format!("{ASSET}/candidates/r_20261016_1200/x.png")),
        b"x",
    );
    assert_eq!(
        store.next_run_id(&asset, None, noon).await.unwrap(),
        "r_20261016_1200_2"
    );

    // The manifest's runs count too, whatever their folders.
    let mut m = Manifest::new(
        "sketchbook",
        "crane",
        "image",
        "00000000-0000-4000-8000-0000000000aa",
    );
    m.push("candidate_runs", json!({"run_id": "r_20261016_1200_2"}))
        .unwrap();
    assert_eq!(
        store.next_run_id(&asset, Some(&m), noon).await.unwrap(),
        "r_20261016_1200_3"
    );
    // Folder names compare without regard to case.
    write_file(
        &ws.file(&format!("{ASSET}/candidates/R_20261016_1200_3/x.png")),
        b"x",
    );
    assert_eq!(
        store.next_run_id(&asset, Some(&m), noon).await.unwrap(),
        "r_20261016_1200_4"
    );
}

#[tokio::test(start_paused = true)] // spec: CORE-003, CORE-038
async fn recover_sweeps_empty_handles_and_lists_the_rest() {
    let ws = workspace();
    let staging = ws.root.join(".lilith").join("staging");
    let empty = staging.join("sh_00000000000000aa");
    fsops::ensure_dir(&empty).unwrap();
    write_file(
        &staging.join("sh_00000000000000bb").join("out.png"),
        b"left by a dead core",
    );
    write_file(
        &staging.join("seal-sh_00000000000000cc").join("v.png"),
        b"sealing",
    );

    let store = ws.plain();
    let report = store.recover().await.unwrap();
    assert_eq!(
        report.staging_removed,
        [p(".lilith/staging/sh_00000000000000aa")]
    );
    assert!(!empty.exists(), "the dead core's empty handle is removed");
    let kept = [
        p(".lilith/staging/seal-sh_00000000000000cc"),
        p(".lilith/staging/sh_00000000000000bb"),
    ];
    assert_eq!(report.staging_kept, kept);
    assert_eq!(
        std::fs::read(staging.join("sh_00000000000000bb").join("out.png")).unwrap(),
        b"left by a dead core"
    );
    assert!(
        staging
            .join("seal-sh_00000000000000cc")
            .join("v.png")
            .is_file()
    );
    let orphans = store.orphans().await.unwrap();
    for k in &kept {
        assert!(orphans.contains(k), "{k} is not listed: {orphans:?}");
    }

    // This core's own handle, even empty, is never swept or listed.
    let live = store.staging_handle().await.unwrap();
    assert_eq!(store.sweep_staging().await.unwrap(), kept);
    assert!(live.dir.is_dir());
    let rel = p(&format!(".lilith/staging/{}", live.id));
    assert!(!store.orphans().await.unwrap().contains(&rel));
}

#[tokio::test(start_paused = true)] // spec: CORE-003, CORE-038
async fn legacy_staging_leftovers_are_orphans_and_untouched() {
    use lil_platform::names::LEGACY_WORKSPACE_DIR;
    let ws = workspace();
    let legacy = ws.root.join(LEGACY_WORKSPACE_DIR).join("staging");
    let empty = legacy.join("sh_00000000000000aa");
    fsops::ensure_dir(&empty).unwrap();
    let full = legacy.join("sh_00000000000000bb").join("out.png");
    write_file(&full, b"left by a core from before the rename");
    let seal = legacy.join("seal-sh_00000000000000cc").join("v.png");
    write_file(&seal, b"sealing");

    let store = ws.plain();
    let report = store.recover().await.unwrap();
    // The sweep never looks there: nothing is removed or reported as kept.
    assert!(report.staging_removed.is_empty(), "{report:?}");
    assert!(report.staging_kept.is_empty(), "{report:?}");
    assert!(empty.is_dir(), "an empty legacy handle was removed");
    assert_eq!(
        std::fs::read(&full).unwrap(),
        b"left by a core from before the rename"
    );
    assert!(seal.is_file());
    // The non-empty entries are listed as orphans, read-only.
    let orphans = store.orphans().await.unwrap();
    let rel = |name: &str| p(&format!("{LEGACY_WORKSPACE_DIR}/staging/{name}"));
    for name in ["sh_00000000000000bb", "seal-sh_00000000000000cc"] {
        assert!(orphans.contains(&rel(name)), "{name}: {orphans:?}");
    }
    assert!(
        !orphans.contains(&rel("sh_00000000000000aa")),
        "{orphans:?}"
    );
    // New handles are made only under the new folder.
    let live = store.staging_handle().await.unwrap();
    assert!(
        live.dir
            .starts_with(ws.root.join(".lilith").join("staging"))
    );
    assert_eq!(std::fs::read_dir(&legacy).unwrap().count(), 3);
}

#[tokio::test(start_paused = true)] // spec: CORE-038
async fn conflict_scan_skips_both_workspace_folders() {
    use lil_platform::names::{LEGACY_CONFLICT_MARKER, LEGACY_WORKSPACE_DIR, WORKSPACE_DIR};
    let ws = workspace();
    let mut want = Vec::new();
    for dir in [WORKSPACE_DIR, LEGACY_WORKSPACE_DIR] {
        // Inside a workspace data folder: never a conflict copy of the person's.
        write_file(
            &ws.file(&format!("{dir}/state.json.lil-conflict-20261016T120000Z")),
            b"x",
        );
        write_file(
            &ws.file(&format!(
                "{dir}/state.json{LEGACY_CONFLICT_MARKER}20261016T120000Z"
            )),
            b"x",
        );
        // A folder of that name deeper down is an ordinary folder.
        let deeper = format!("notes/{dir}/a.txt.lil-conflict-20261016T120000Z");
        write_file(&ws.file(&deeper), b"x");
        want.push(p(&deeper));
    }
    let legacy_copy = format!("notes/b.txt{LEGACY_CONFLICT_MARKER}20261016T120000Z");
    write_file(&ws.file(&legacy_copy), b"x");
    want.push(p(&legacy_copy));
    want.sort();

    let orphans = ws.plain().orphans().await.unwrap();
    assert_eq!(orphans, want);
}

#[tokio::test(start_paused = true)] // spec: CORE-038, CORE-026
async fn orphans_include_unrecorded_sealed_versions_and_conflict_files() {
    let ws = workspace();
    let mut m = Manifest::new(
        "sketchbook",
        "crane",
        "image",
        "00000000-0000-4000-8000-0000000000aa",
    );
    m.set("approved_version", json!("1.0.0"));
    m.push(
        "candidate_runs",
        json!({"run_id": "r_known", "status": "selected",
               "signed_off": {"by": "user", "date": "2026-10-02", "version": "1.0.1"}}),
    )
    .unwrap();
    write_file(&ws.file(MANIFEST), m.to_text().as_bytes());
    for (rel, bytes) in [
        // Recorded: the approved version and a signed-off run's version.
        (format!("{ASSET}/v1.0.0/manifest.snapshot.json"), &b"{}"[..]),
        (format!("{ASSET}/v1.0.1/manifest.snapshot.json"), b"{}"),
        // Sealed, but no manifest field records it (a crash before the manifest edit).
        (format!("{ASSET}/v1.1.0/manifest.snapshot.json"), b"{}"),
        (format!("{ASSET}/v1.1.0/x.png"), b"x"),
        // No snapshot: not a sealed version.
        (format!("{ASSET}/v1.2.0/x.png"), b"x"),
        (format!("{ASSET}/candidates/r_known/a.png"), b"a"),
        (format!("{ASSET}/candidates/r_unknown/b.png"), b"b"),
        (
            "notes/notes.txt.lil-conflict-20261016T120000Z".to_string(),
            b"outside",
        ),
        ("notes/notes.txt".to_string(), b"current"),
    ] {
        write_file(&ws.file(&rel), bytes);
    }
    let orphans = ws.plain().orphans().await.unwrap();
    assert_eq!(
        orphans,
        [
            p("notes/notes.txt.lil-conflict-20261016T120000Z"),
            p(&format!("{ASSET}/candidates/r_unknown")),
            p(&format!("{ASSET}/v1.1.0")),
        ]
    );
}
