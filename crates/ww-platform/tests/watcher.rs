//! `DirWatcher` (ARCH-079, ARCH-083): `ReadDirectoryChangesW` on a real NTFS temp folder.
//!
//! Every wait is on the watcher's channel with a failure deadline, never a sleep before an
//! assertion (QA-031). The overflow test holds the thread's reads ([`DirWatcher::hold`]) while
//! it makes its burst, so the overflow is certain rather than a race.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use ww_platform::watcher::{DirChange, DirWatcher, MAX_BUFFER_BYTES};

/// A failure deadline only.
const DEADLINE: Duration = Duration::from_secs(20);

fn temp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ww-watcher-Ünïcödé space-")
        .tempdir()
        .unwrap()
}

/// The next change, waiting at most [`DEADLINE`]; `None` once the channel has closed.
async fn next(rx: &mut UnboundedReceiver<DirChange>) -> Option<DirChange> {
    tokio::time::timeout(DEADLINE, rx.recv())
        .await
        .expect("no change within the deadline")
}

/// Changes until one matches `want`, which is returned with everything before it.
async fn until(
    rx: &mut UnboundedReceiver<DirChange>,
    want: impl Fn(&DirChange) -> bool,
) -> Vec<DirChange> {
    let mut seen = Vec::new();
    loop {
        let change = next(rx)
            .await
            .unwrap_or_else(|| panic!("the channel closed; seen {seen:?}"));
        let done = want(&change);
        seen.push(change);
        if done {
            return seen;
        }
    }
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

/// Runs `f` on a current-thread runtime with time (`ww-platform` has no tokio test macros).
fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(f)
}

#[test] // spec: ARCH-079
fn dir_watcher_reports_create_modify_rename_delete() {
    block(async {
        let dir = temp();
        let root = dir.path();
        std::fs::create_dir(root.join("sub")).unwrap();
        let (watcher, mut rx) = DirWatcher::start(root, MAX_BUFFER_BYTES).unwrap();

        let file = root.join("sub").join("a.txt");
        std::fs::write(&file, b"one").unwrap();
        let seen = until(&mut rx, |c| *c == DirChange::Added(p(r"sub\a.txt"))).await;
        assert!(!seen.contains(&DirChange::Overflow), "{seen:?}");

        std::fs::write(&file, b"two, longer").unwrap();
        until(&mut rx, |c| *c == DirChange::Modified(p(r"sub\a.txt"))).await;

        std::fs::rename(&file, root.join("sub").join("Ünïcödé space.txt")).unwrap();
        let seen = until(&mut rx, |c| {
            *c == DirChange::RenamedTo(p(r"sub\Ünïcödé space.txt"))
        })
        .await;
        assert!(
            seen.contains(&DirChange::RenamedFrom(p(r"sub\a.txt"))),
            "{seen:?}"
        );

        std::fs::remove_file(root.join("sub").join("Ünïcödé space.txt")).unwrap();
        until(&mut rx, |c| {
            *c == DirChange::Removed(p(r"sub\Ünïcödé space.txt"))
        })
        .await;

        // A folder is one name too.
        std::fs::create_dir(root.join("new folder")).unwrap();
        until(&mut rx, |c| *c == DirChange::Added(p("new folder"))).await;
        watcher.stop();
    });
}

#[test] // spec: ARCH-079
fn tiny_buffer_overflows_into_an_overflow_event() {
    block(async {
        let dir = temp();
        let root = dir.path();
        let names: Vec<PathBuf> = (0..2000)
            .map(|i| root.join(format!("f{i:04}.txt")))
            .collect();
        for name in &names {
            std::fs::write(name, b"x").unwrap();
        }
        let (watcher, mut rx) = DirWatcher::start(root, 4096).unwrap();
        // No read is issued after the pending one completes, so the system's 4 KiB buffer for the
        // folder takes the rest of the burst and overflows.
        watcher.hold();
        for name in &names {
            std::fs::rename(name, name.with_extension("moved")).unwrap();
        }
        watcher.release();
        let seen = until(&mut rx, |c| *c == DirChange::Overflow).await;
        assert!(
            seen.len() < 2 * names.len(),
            "{} changes came through a 4 KiB buffer",
            seen.len()
        );
        watcher.stop();
    });
}

#[test] // spec: ARCH-079
fn oversized_buffer_is_refused_up_front() {
    let dir = temp();
    let err = DirWatcher::start(dir.path(), 65537).expect_err("a 65537-byte buffer");
    assert_eq!(err.raw_os_error(), Some(87), "{err:?}");
    let err = DirWatcher::start(dir.path(), 0).expect_err("an empty buffer");
    assert_eq!(err.raw_os_error(), Some(87), "{err:?}");
    // The largest a network folder takes is fine.
    let (watcher, _rx) = DirWatcher::start(dir.path(), 65536).unwrap();
    watcher.stop();
    // A folder that isn't there is the open's error.
    let err = DirWatcher::start(&dir.path().join("missing"), 4096).expect_err("no folder");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err:?}");
}

#[test] // spec: ARCH-079
fn watched_root_can_still_be_renamed_into() {
    block(async {
        let parent = temp();
        let root = parent.path().join("watched");
        std::fs::create_dir_all(root.join("folder")).unwrap();
        std::fs::create_dir_all(parent.path().join("sibling")).unwrap();
        std::fs::write(root.join("a.txt"), b"a").unwrap();
        let (watcher, mut rx) = DirWatcher::start(&root, MAX_BUFFER_BYTES).unwrap();

        // The watch handle shares delete, so renames inside the tree and beside it go through.
        std::fs::rename(root.join("a.txt"), root.join("b.txt")).unwrap();
        std::fs::rename(root.join("folder"), root.join("folder 2")).unwrap();
        std::fs::rename(
            parent.path().join("sibling"),
            parent.path().join("sibling 2"),
        )
        .unwrap();
        // Something renamed into the watched tree from beside it.
        std::fs::write(parent.path().join("outside.txt"), b"o").unwrap();
        std::fs::rename(parent.path().join("outside.txt"), root.join("inside.txt")).unwrap();

        let seen = until(&mut rx, |c| {
            matches!(c, DirChange::Added(n) if n == Path::new("inside.txt"))
                || matches!(c, DirChange::RenamedTo(n) if n == Path::new("inside.txt"))
        })
        .await;
        for want in [
            DirChange::RenamedFrom(p("a.txt")),
            DirChange::RenamedTo(p("b.txt")),
            DirChange::RenamedFrom(p("folder")),
            DirChange::RenamedTo(p("folder 2")),
        ] {
            assert!(seen.contains(&want), "{want:?} not in {seen:?}");
        }
        assert!(
            !seen.iter().any(
                |c| matches!(c, DirChange::RenamedTo(n) if n.to_string_lossy().contains("sibling"))
            ),
            "a folder beside the watched one is not reported: {seen:?}"
        );
        watcher.stop();
    });
}

#[test] // spec: ARCH-079
fn removing_the_watched_root_ends_with_failed() {
    block(async {
        let parent = temp();
        let root = parent.path().join("watched");
        std::fs::create_dir(&root).unwrap();
        let (_watcher, mut rx) = DirWatcher::start(&root, MAX_BUFFER_BYTES).unwrap();
        std::fs::remove_dir(&root).unwrap();
        let mut seen = Vec::new();
        loop {
            match next(&mut rx).await {
                Some(DirChange::Failed(code)) => {
                    assert_ne!(code, 995, "our own cancel is never a failure");
                    break;
                }
                Some(other) => seen.push(other),
                None => panic!("the channel closed without a Failed: {seen:?}"),
            }
        }
        assert_eq!(next(&mut rx).await, None, "the thread ends after Failed");
    });
}

#[test] // spec: ARCH-079
fn stop_ends_the_thread() {
    block(async {
        let dir = temp();
        let (watcher, mut rx) = DirWatcher::start(dir.path(), MAX_BUFFER_BYTES).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
        until(&mut rx, |c| *c == DirChange::Added(p("a.txt"))).await;
        watcher.stop();
        // A stop is a cancel: the channel closes without a Failed.
        while let Some(change) = next(&mut rx).await {
            assert!(!matches!(change, DirChange::Failed(_)), "{change:?}");
        }
        // A held watcher stops too.
        let (held, mut rx) = DirWatcher::start(dir.path(), MAX_BUFFER_BYTES).unwrap();
        held.hold();
        std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
        held.stop();
        while let Some(change) = next(&mut rx).await {
            assert!(!matches!(change, DirChange::Failed(_)), "{change:?}");
        }
        // Dropping stops it as well.
        let (dropped, mut rx) = DirWatcher::start(dir.path(), MAX_BUFFER_BYTES).unwrap();
        drop(dropped);
        assert_eq!(next(&mut rx).await, None);
    });
}
