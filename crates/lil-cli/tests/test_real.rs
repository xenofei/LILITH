//! `lil test --real` (ARCH-016, QA-063): the real-engine run works only from the source
//! checkout `lil` was built from. Outside it, in a folder that is no repository or in another
//! repository, it says so and exits 2 before reading any setting or starting anything. The
//! real run itself needs a running ComfyUI and is the owner's to start.

use std::ffi::OsString;

use lil_testkit::TestEnv;

#[test] // spec: ARCH-016, QA-063
fn test_real_outside_a_checkout_exits_2() {
    let env = TestEnv::new();
    let plain = env.root().join("not-a-repo");
    let other = env.root().join("other-repo");
    for dir in [&plain, &other] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&other)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .status()
        .unwrap();
    assert!(git.success());
    // git never looks above the test's own folder for a repository.
    let ceiling = [(
        "GIT_CEILING_DIRECTORIES",
        OsString::from(env.root().as_os_str()),
    )];
    for (dir, args) in [
        (&plain, vec!["test", "--real"]),
        (&other, vec!["test", "--real"]),
        (&plain, vec!["test", "--real", "--record", "fixtures"]),
    ] {
        let out = env.lil_in_with(dir, &args, &ceiling, None);
        assert_eq!(out.code, 2, "{} {args:?}: {out:?}", dir.display());
        assert!(
            out.stderr.contains("source checkout"),
            "{} {args:?}: {out:?}",
            dir.display()
        );
        assert!(out.stdout.is_empty(), "{out:?}");
    }
    assert!(!plain.join("fixtures").exists(), "nothing was recorded");
}
