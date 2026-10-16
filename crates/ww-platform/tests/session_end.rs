//! The session-end window (ARCH-021, ARCH-005): a hidden top-level window on its own thread
//! that reports `WM_QUERYENDSESSION` and `WM_ENDSESSION`, and holds `WM_ENDSESSION` until the
//! core's shutdown is complete. The messages are sent to the watcher's own window only.
//!
//! Wall-clock durations here are failure deadlines only, never measurements (QA-031).

use std::time::{Duration, Instant};

use ww_platform::session_end::{SessionEnd, SessionEndWatcher};

/// Failure deadline for anything the test waits on.
const DEADLINE: Duration = Duration::from_secs(30);
/// Pause between polls.
const POLL: Duration = Duration::from_millis(5);

fn recv(rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionEnd>) -> SessionEnd {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match rx.try_recv() {
            Ok(msg) => return msg,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) if Instant::now() < deadline => {
                std::thread::sleep(POLL);
            }
            Err(e) => panic!("no session-end message within {DEADLINE:?}: {e}"),
        }
    }
}

#[test] // spec: ARCH-021
fn query_end_session_is_reported_and_not_acted_on() {
    let (watcher, mut rx) = SessionEndWatcher::start().expect("the session-end window");
    assert!(
        watcher.simulate(SessionEnd::QueryEnd),
        "WM_QUERYENDSESSION must answer TRUE (the session may end)"
    );
    assert_eq!(recv(&mut rx), SessionEnd::QueryEnd);
    // Only reported: the window does not wait for anything on a query.
    assert!(!watcher.is_waiting_for_shutdown());
    assert!(
        rx.try_recv().is_err(),
        "a query is reported once, as QueryEnd"
    );
}

#[test] // spec: ARCH-021
fn end_session_blocks_until_shutdown_complete() {
    let (watcher, mut rx) = SessionEndWatcher::start().expect("the session-end window");
    let watcher = std::sync::Arc::new(watcher);
    let sender = watcher.clone();
    let sent = std::thread::spawn(move || sender.simulate(SessionEnd::End));

    assert_eq!(recv(&mut rx), SessionEnd::End);
    let deadline = Instant::now() + DEADLINE;
    while !watcher.is_waiting_for_shutdown() {
        assert!(
            Instant::now() < deadline,
            "WM_ENDSESSION did not start waiting for the shutdown"
        );
        std::thread::sleep(POLL);
    }
    // The handler is inside its wait: WM_ENDSESSION has not returned.
    assert!(
        !sent.is_finished(),
        "WM_ENDSESSION returned before shutdown_complete"
    );

    watcher.shutdown_complete();
    assert!(sent.join().expect("the sending thread"));
    assert!(!watcher.is_waiting_for_shutdown());

    // Once the shutdown is complete, a later WM_ENDSESSION returns at once.
    assert!(watcher.simulate(SessionEnd::End));
    assert_eq!(recv(&mut rx), SessionEnd::End);
}

#[test] // spec: ARCH-005
fn session_end_window_is_never_visible() {
    let (watcher, mut rx) = SessionEndWatcher::start().expect("the session-end window");
    assert!(!watcher.is_visible(), "the session-end window is visible");
    assert!(watcher.simulate(SessionEnd::QueryEnd));
    assert_eq!(recv(&mut rx), SessionEnd::QueryEnd);
    watcher.shutdown_complete();
    assert!(watcher.simulate(SessionEnd::End));
    assert_eq!(recv(&mut rx), SessionEnd::End);
    assert!(
        !watcher.is_visible(),
        "the session-end window became visible"
    );
    drop(watcher);
}

#[test] // spec: ARCH-021
fn outside_wm_close_does_not_destroy_the_window() {
    let (watcher, mut rx) = SessionEndWatcher::start().expect("the session-end window");
    // A WM_CLOSE sent to the window, as another program could send it, is ignored.
    assert!(
        watcher.simulate(SessionEnd::CloseIgnored),
        "WM_CLOSE destroyed the session-end window"
    );
    assert_eq!(recv(&mut rx), SessionEnd::CloseIgnored);
    // The window still answers the session messages.
    assert!(watcher.simulate(SessionEnd::QueryEnd));
    assert_eq!(recv(&mut rx), SessionEnd::QueryEnd);
    assert!(watcher.is_alive());
    // Dropping the watcher still destroys it and ends its thread.
    drop(watcher);
}
