//! `lil notify` against the real `lil` and `lil-core` (ARCH-016, ARCH-018, ARCH-073).
//!
//! Every test runs on its own `TestEnv` channel; any core `lil` starts is drained when the env
//! drops (QA-041). Every workspace is under the env's root.

use lil_testkit::{RawPipe, TestEnv};

#[test] // spec: ARCH-016, ARCH-018
fn notify_no_start_is_silent_without_core() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.make_workspace("Ünïcödé space");
    env.register_workspace(&ws, true);
    let file = ws.join("notes").join("a.txt");
    let arg = file.to_str().expect("a UTF-8 test path");

    let out = env.lil(&["notify", "--no-start", arg]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.stdout, "", "{out:?}");
    assert_eq!(out.stderr, "", "{out:?}");
    // Nothing was started.
    assert!(
        RawPipe::try_connect(&env, std::time::Duration::ZERO).is_none(),
        "--no-start started a core"
    );

    // Without --no-start the core starts; a path outside the workspace is named on stderr and
    // nothing goes to stdout.
    let outside = env.root().join("elsewhere.txt");
    let out = env.lil(&["notify", arg, outside.to_str().expect("a UTF-8 test path")]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.stdout, "", "{out:?}");
    assert!(out.stderr.contains("elsewhere.txt"), "{out:?}");
    assert!(out.stderr.contains("left alone"), "{out:?}");
}
