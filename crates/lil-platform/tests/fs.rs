//! The platform file layer against real files in temp folders (ARCH-074, ARCH-089, CORE-040,
//! CORE-041, ARCH-028, ARCH-081).
//!
//! Every test works in its own `tempfile` folder on the temp volume. Nothing here touches a
//! workspace, OneDrive, a network share or a FAT volume: those volume kinds are covered by the
//! pure `stable_ids` and `sync_root_from` unit tests, and cloud placeholders are faked through
//! `FILE_ATTRIBUTE_OFFLINE`.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use lil_platform::fs::{
    self, RetryClass, Share, attributes, classify, copy_new, identity, long_name, long_path,
    move_new, open_shared, replace_file, set_readonly, try_lock_append, try_lock_exclusive,
    volume_info,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_READONLY, GetShortPathNameW,
    SetFileAttributesW,
};
use windows::core::PCWSTR;

fn write(path: &Path, text: &str) {
    std::fs::write(long_path(path), text).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(long_path(path)).unwrap()
}

fn exists(path: &Path) -> bool {
    std::fs::metadata(long_path(path)).is_ok()
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// Sets raw attributes on a test file (only `OFFLINE` and `NORMAL` are used here).
fn set_attributes(path: &Path, attrs: u32) {
    let w = wide(&long_path(path));
    // SAFETY: `w` is NUL-terminated and outlives the call.
    unsafe {
        SetFileAttributesW(
            PCWSTR(w.as_ptr()),
            windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(attrs),
        )
    }
    .unwrap();
}

#[test] // spec: ARCH-074
fn replace_file_swaps_content_and_keeps_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.txt");
    let temp = dir.path().join(".target.txt.ww-tmp-1-1");
    let backup = dir.path().join(".target.txt.ww-bak-1-1");
    write(&target, "old");
    write(&temp, "new");
    replace_file(&target, &temp, &backup).unwrap();
    assert_eq!(read(&target), "new");
    assert_eq!(read(&backup), "old");
    assert!(!exists(&temp), "the replacement takes the target's name");
}

/// `ReplaceFileW` documents that "the resulting file has the same file ID as the replacement
/// file"; the index records the identity the store reports after each write.
#[test] // spec: ARCH-074, CORE-041
fn replaced_file_takes_the_replacement_file_id() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.txt");
    let temp = dir.path().join(".target.txt.ww-tmp-1-1");
    let backup = dir.path().join(".target.txt.ww-bak-1-1");
    write(&target, "old");
    write(&temp, "new");
    let old = identity(&target).unwrap();
    let replacement = identity(&temp).unwrap();
    assert!(
        replacement.file_id.is_some(),
        "the temp volume reports file ids"
    );
    assert_ne!(old.file_id, replacement.file_id);
    replace_file(&target, &temp, &backup).unwrap();
    let after = identity(&target).unwrap();
    assert_eq!(after.file_id, replacement.file_id);
    assert_eq!(after.volume_serial, replacement.volume_serial);
    assert_eq!(after.size, 3);
}

#[test] // spec: ARCH-074
fn move_new_refuses_an_existing_target() {
    let dir = tempfile::tempdir().unwrap();
    let from = dir.path().join("from.txt");
    let to = dir.path().join("to.txt");
    write(&from, "from");
    write(&to, "to");
    let err = move_new(&from, &to).unwrap_err();
    assert_eq!(classify(&err), RetryClass::TargetExists, "{err:?}");
    assert_eq!(read(&from), "from");
    assert_eq!(read(&to), "to");

    // A free target name: the file moves.
    let free = dir.path().join("free.txt");
    move_new(&from, &free).unwrap();
    assert_eq!(read(&free), "from");
    assert!(!exists(&from));

    // Folders move too, children included, and an existing folder is never replaced.
    let a = dir.path().join("a");
    std::fs::create_dir(&a).unwrap();
    write(&a.join("child.txt"), "child");
    let b = dir.path().join("b");
    move_new(&a, &b).unwrap();
    assert_eq!(read(&b.join("child.txt")), "child");
    std::fs::create_dir(&a).unwrap();
    let err = move_new(&b, &a).unwrap_err();
    assert_eq!(classify(&err), RetryClass::TargetExists, "{err:?}");
}

#[test] // spec: ARCH-074
fn copy_new_refuses_an_existing_target() {
    let dir = tempfile::tempdir().unwrap();
    let from = dir.path().join("from.txt");
    let to = dir.path().join("to.txt");
    write(&from, "from bytes");
    write(&to, "to");
    let err = copy_new(&from, &to).unwrap_err();
    assert_eq!(classify(&err), RetryClass::TargetExists, "{err:?}");
    assert_eq!(read(&to), "to", "the existing target is never overwritten");

    let fresh = dir.path().join("fresh.txt");
    assert_eq!(copy_new(&from, &fresh).unwrap(), 10);
    assert_eq!(read(&fresh), "from bytes");
    assert_eq!(read(&from), "from bytes");
}

/// Defender, Explorer's preview pane or an image viewer holding the target (Review Focus 2).
#[test] // spec: ARCH-074
fn held_target_is_a_sharing_violation() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.txt");
    let temp = dir.path().join(".target.txt.ww-tmp-1-1");
    let backup = dir.path().join(".target.txt.ww-bak-1-1");
    write(&target, "old");
    write(&temp, "new");
    let held = open_shared(&target, Share::Read).unwrap();
    let err = replace_file(&target, &temp, &backup).unwrap_err();
    assert_eq!(classify(&err), RetryClass::SharingViolation, "{err:?}");
    drop(held);
    assert_eq!(read(&target), "old");
    assert_eq!(read(&temp), "new");
    replace_file(&target, &temp, &backup).unwrap();
    assert_eq!(read(&target), "new");
}

/// Pads `base` with nested folders until its length in UTF-16 units reaches `units`.
fn nested_to(base: &Path, units: usize) -> PathBuf {
    let mut path = base.to_path_buf();
    let mut n = 0;
    while path.as_os_str().encode_wide().count() + 1 + 40 < units {
        n += 1;
        path.push(format!("{n:02}-{}", "d".repeat(36)));
    }
    let len = path.as_os_str().encode_wide().count();
    let pad = units - len - 1;
    path.push("e".repeat(pad));
    assert_eq!(path.as_os_str().encode_wide().count(), units);
    path
}

/// Review Focus 3: absolute paths past 260 characters work without the `LongPathsEnabled`
/// opt-in, because every function goes through `long_path`.
#[test] // spec: CORE-040
fn long_path_round_trip_past_260() {
    let dir = tempfile::tempdir().unwrap();
    // A folder whose own path is 250 characters: past CreateDirectory's 248 limit.
    let folder_250 = nested_to(dir.path(), 250);
    std::fs::create_dir_all(long_path(&folder_250)).unwrap();
    assert!(std::fs::metadata(long_path(&folder_250)).unwrap().is_dir());

    let deep = nested_to(dir.path(), 300 - "\\target.txt".len());
    std::fs::create_dir_all(long_path(&deep)).unwrap();
    let target = deep.join("target.txt");
    assert_eq!(target.as_os_str().encode_wide().count(), 300);
    let temp = deep.join(".t.ww-tmp-1-1");
    let backup = deep.join(".t.ww-bak-1-1");
    write(&target, "old");
    write(&temp, "new");

    replace_file(&target, &temp, &backup).unwrap();
    assert_eq!(read(&target), "new");
    assert_eq!(read(&backup), "old");

    let moved = deep.join("moved.txt");
    move_new(&backup, &moved).unwrap();
    assert_eq!(read(&moved), "old");

    let id = identity(&target).unwrap();
    assert_eq!(id.size, 3);
    set_readonly(&target, true).unwrap();
    assert_ne!(attributes(&target).unwrap() & FILE_ATTRIBUTE_READONLY.0, 0);
    set_readonly(&target, false).unwrap();
    assert_eq!(attributes(&target).unwrap() & FILE_ATTRIBUTE_READONLY.0, 0);
}

/// NTFS keeps a file's id across a rename in one folder. Other file systems are not asserted:
/// ReFS builds ids from the parent folder, so ids only detect changes in place (CORE-041).
#[test] // spec: CORE-041
fn file_id_survives_a_same_folder_rename() {
    let dir = tempfile::tempdir().unwrap();
    let volume = volume_info(dir.path()).unwrap();
    if volume.fs_name != "NTFS" {
        eprintln!(
            "skipped: the temp volume is {}, not NTFS; nothing is asserted",
            volume.fs_name
        );
        return;
    }
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    write(&a, "x");
    let before = identity(&a).unwrap();
    move_new(&a, &b).unwrap();
    let after = identity(&b).unwrap();
    assert!(before.file_id.is_some());
    assert_eq!(before.file_id, after.file_id);
    assert_eq!(before.volume_serial, after.volume_serial);
}

fn short_name(path: &Path) -> PathBuf {
    let w = wide(path);
    let mut buf = vec![0u16; 32_768];
    // SAFETY: `w` is NUL-terminated; `buf` is a writable slice of the length passed.
    let len = unsafe { GetShortPathNameW(PCWSTR(w.as_ptr()), Some(&mut buf)) } as usize;
    assert!(len > 0 && len < buf.len(), "GetShortPathNameW failed");
    PathBuf::from(OsString::from_wide(&buf[..len]))
}

#[test] // spec: CORE-040
fn long_name_expands_a_short_component() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let base = lil_platform::paths::strip_verbatim(&base);
    let long = base.join("A Long Ünïcödé space Name.txt");
    write(&long, "x");
    let short = short_name(&long);
    if short.file_name() == long.file_name() {
        eprintln!("skipped: 8.3 names are disabled on the temp volume");
        return;
    }
    assert_eq!(long_name(&short).unwrap(), long);
    std::fs::remove_file(&long).unwrap();
    let err = long_name(&short).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err:?}");
}

/// The store's `index.lock` check: `GetFileAttributesW` opens nothing, so a held file is no
/// obstacle.
#[test] // spec: ARCH-074
fn attributes_reads_a_held_file_without_opening_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.lock");
    write(&path, "x");
    let _held = open_shared(&path, Share::None).unwrap();
    let err = std::fs::File::open(&path).unwrap_err();
    assert_eq!(classify(&err), RetryClass::SharingViolation, "{err:?}");
    let attrs = attributes(&path).unwrap();
    assert_eq!(attrs & 0x10, 0, "not a directory");
    let missing = attributes(&dir.path().join("missing")).unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
}

/// An attribute-only open reads no data, so a cloud placeholder is never hydrated (Review
/// Focus 4).
#[test] // spec: CORE-041, ARCH-089
fn identity_needs_no_data_access() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("held.txt");
    write(&path, "12345");
    {
        let _held = open_shared(&path, Share::None).unwrap();
        let id = identity(&path).unwrap();
        assert_eq!(id.size, 5);
        assert!(!id.is_cloud_placeholder());
        assert!(id.last_write > 0);
    }
    let placeholder = dir.path().join("placeholder.txt");
    write(&placeholder, "abc");
    set_attributes(&placeholder, FILE_ATTRIBUTE_OFFLINE.0);
    let id = identity(&placeholder).unwrap();
    assert!(id.is_cloud_placeholder(), "{:#x}", id.attributes);
    set_attributes(&placeholder, FILE_ATTRIBUTE_NORMAL.0);
    assert!(!identity(&placeholder).unwrap().is_cloud_placeholder());
}

#[test] // spec: CORE-041, ARCH-089
fn volume_info_reports_ntfs_for_the_temp_volume() {
    let dir = tempfile::tempdir().unwrap();
    let info = volume_info(dir.path()).unwrap();
    assert_eq!(info.fs_name, "NTFS", "{info:?}");
    assert!(!info.remote, "{info:?}");
    assert!(!info.cloud_sync_root, "{info:?}");
    assert!(info.stable_ids());
    // A path that does not exist yet answers for its nearest existing folder.
    let missing = volume_info(&dir.path().join("not").join("yet")).unwrap();
    assert_eq!(missing, info);
}

#[test] // spec: ARCH-028
fn exclusive_lock_is_refused_while_held_and_free_after_drop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("core.lock");
    let first = try_lock_exclusive(&path).unwrap().expect("the first lock");
    assert!(exists(&path), "the lock file is created");
    assert!(try_lock_exclusive(&path).unwrap().is_none());
    drop(first);
    let again = try_lock_exclusive(&path).unwrap();
    assert!(again.is_some());
}

fn append_handle(path: &Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(long_path(path))
        .unwrap()
}

#[test] // spec: ARCH-081, CORE-009
fn append_lock_excludes_a_second_holder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.testmachine.2026-10.jsonl");
    let a = append_handle(&path);
    let b = append_handle(&path);
    let lock = try_lock_append(&a).unwrap().expect("the first holder");
    assert!(try_lock_append(&b).unwrap().is_none(), "refused at once");
    drop(lock);
    let lock_b = try_lock_append(&b).unwrap();
    assert!(lock_b.is_some());
    // The lock is on a byte past any real data: appends by the holder still work.
    use std::io::Write as _;
    (&b).write_all(b"{\"machine\":\"testmachine\",\"seq\":1}\n")
        .unwrap();
    drop(lock_b);
    assert!(read(&path).ends_with('\n'));
}

#[test] // spec: ARCH-074
fn readonly_attribute_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sealed.txt");
    write(&path, "x");
    set_readonly(&path, true).unwrap();
    assert_ne!(attributes(&path).unwrap() & FILE_ATTRIBUTE_READONLY.0, 0);
    assert!(std::fs::metadata(&path).unwrap().permissions().readonly());
    set_readonly(&path, true).unwrap();
    assert_ne!(attributes(&path).unwrap() & FILE_ATTRIBUTE_READONLY.0, 0);
    set_readonly(&path, false).unwrap();
    assert_eq!(attributes(&path).unwrap() & FILE_ATTRIBUTE_READONLY.0, 0);
    assert!(!std::fs::metadata(&path).unwrap().permissions().readonly());
}

#[test]
fn flush_and_probe_open_read_work_on_plain_files() {
    use fs::Probe as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    let mut file = std::fs::File::create(&path).unwrap();
    use std::io::{Read as _, Write as _};
    file.write_all(b"flushed").unwrap();
    fs::flush(&file).unwrap();
    drop(file);
    let probe = fs::SystemProbe;
    let mut text = String::new();
    probe
        .open_read(&path)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, "flushed");
    assert_eq!(probe.identity(&path).unwrap().size, 7);
    assert!(probe.volume_info(&path).unwrap().stable_ids());
}
