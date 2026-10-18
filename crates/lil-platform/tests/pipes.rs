//! User-only pipe instances, pipe peers, process inspection and socket tables (ARCH-006,
//! ARCH-024, ARCH-026, ARCH-027, ARCH-045).
//!
//! Every pipe here has a unique `test-<random>` name under the fixed test SID, so no test can
//! meet a real core's pipe or another test's.

use std::collections::BTreeSet;

use lil_platform::pipe::{
    ConnectError, CreateError, client_pid, create_instance, instance_dacl, instance_dacl_sids,
    open_client, pipe_sddl, server_pid,
};
use lil_platform::process::{ProcessInspector, SystemInspector, current_user_sid};
use lil_platform::sockets::{SocketKind, sockets_of};
use lil_platform::{IdSource, RandomIds, TEST_PIPE_SID};

/// `GENERIC_ALL` (`GA` in SDDL).
const GENERIC_ALL_MASK: u32 = 0x1000_0000;
/// `FILE_ALL_ACCESS` (`STANDARD_RIGHTS_REQUIRED | SYNCHRONIZE | 0x1FF`): what `GENERIC_ALL`
/// maps to for file objects, named pipes included.
const FILE_ALL_ACCESS_MASK: u32 = 0x001F_01FF;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// `\\.\pipe\worldwright-core-<test SID>-test-<random>`.
fn unique_pipe() -> String {
    let id = RandomIds.id("p").replace('_', "");
    format!(r"\\.\pipe\worldwright-core-{TEST_PIPE_SID}-test-{id}")
}

#[test] // spec: ARCH-026
fn pipe_sddl_format() {
    assert_eq!(
        pipe_sddl("S-1-5-21-1-2-3-1001"),
        "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)(A;;GA;;;SY)"
    );
}

#[test] // spec: ARCH-026
fn instance_dacl_is_user_and_system_only() {
    runtime().block_on(async {
        let name = unique_pipe();
        let server = create_instance(&name, true).unwrap();
        let sids: BTreeSet<String> = instance_dacl_sids(&server).unwrap().into_iter().collect();
        let expected: BTreeSet<String> = [current_user_sid().unwrap(), "S-1-5-18".to_string()]
            .into_iter()
            .collect();
        assert_eq!(sids, expected);
        // Each entry grants `GA` (GENERIC_ALL), as the SDDL says, and nothing narrower or wider.
        // Windows maps generic rights to the object's specific rights when it stores the
        // descriptor, so the stored mask is GENERIC_ALL through the file generic mapping,
        // FILE_ALL_ACCESS; the unmapped bit is accepted too.
        let entries = instance_dacl(&server).unwrap();
        assert_eq!(entries.len(), 2, "{entries:?}");
        for entry in &entries {
            assert!(entry.allowed, "{entry:?}");
            assert!(
                entry.mask == GENERIC_ALL_MASK || entry.mask == FILE_ALL_ACCESS_MASK,
                "{entry:?}: mask {:#x}",
                entry.mask
            );
        }
    });
}

#[test] // spec: ARCH-026
fn second_first_instance_is_already_owned() {
    runtime().block_on(async {
        let name = unique_pipe();
        let _first = create_instance(&name, true).unwrap();
        match create_instance(&name, true) {
            Err(CreateError::AlreadyOwned) => {}
            other => panic!("expected AlreadyOwned, got {other:?}"),
        }
        // A further, non-first instance of the same pipe is allowed.
        let _second = create_instance(&name, false).unwrap();
    });
}

#[test] // spec: ARCH-027, ARCH-045
fn client_and_server_pids_are_this_process() {
    runtime().block_on(async {
        let name = unique_pipe();
        let server = create_instance(&name, true).unwrap();
        let client = open_client(&name).unwrap();
        server.connect().await.unwrap();
        assert_eq!(client_pid(&server).unwrap(), std::process::id());
        assert_eq!(server_pid(&client).unwrap(), std::process::id());
    });
}

#[test]
fn open_client_on_missing_pipe_is_not_found() {
    runtime().block_on(async {
        match open_client(&unique_pipe()) {
            Err(ConnectError::NotFound) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    });
}

#[test] // spec: ARCH-027
fn inspector_reports_own_image_and_user() {
    let info = SystemInspector.inspect(std::process::id()).unwrap();
    assert_eq!(info.pid, std::process::id());
    let exe = std::env::current_exe().unwrap();
    assert_eq!(
        info.image.to_string_lossy().to_lowercase(),
        exe.to_string_lossy().to_lowercase()
    );
    assert_eq!(info.user_sid, current_user_sid().unwrap());
    assert!(info.parent_pid.is_some());
}

#[test] // spec: ARCH-006
fn sockets_of_reports_a_loopback_listener() {
    // Bound on loopback only; nothing ever connects to it.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mine = sockets_of(std::process::id()).unwrap();
    assert!(mine.contains(&SocketKind::TcpListen), "{mine:?}");
    drop(listener);

    let comspec = std::env::var_os("ComSpec").expect("%ComSpec% is set");
    let mut child = std::process::Command::new(comspec)
        .args(["/c", "exit 0"])
        .spawn()
        .unwrap();
    let theirs = sockets_of(child.id()).unwrap();
    child.wait().unwrap();
    assert!(theirs.is_empty(), "{theirs:?}");
}
