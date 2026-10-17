//! Path spellings for comparison and display (ARCH-027, ARCH-097, QA-038).
//!
//! - [`folder_key`] is the one spelling folders are compared by (trusted executable folders,
//!   workspace paths). It is for comparison only and never hashed: the workspace hash keeps its
//!   own plan-1 ASCII-lowercase normalisation (`env::normalise_for_hash`).
//! - [`strip_verbatim`] removes the `\\?\` prefix from a path that is shown, keeping its case.
//! - [`upcase_name`] folds a name the way Windows compares file names without case.

use std::path::{Path, PathBuf};

use windows::Wdk::System::SystemServices::RtlUpcaseUnicodeChar;

/// `name` with every UTF-16 unit replaced by `RtlUpcaseUnicodeChar` of it: the operating
/// system's uppercase table, one unit to one unit, independent of locale.
///
/// This is how Windows compares file names without case. Microsoft Learn ("Handling Sorting
/// in Your Applications") names NTFS file names as non-linguistic strings to compare with
/// `CompareStringOrdinal`, whose case-insensitive mode "maps case using the operating system
/// uppercasing table … independent of locale"; NTFS writes that table to each volume's
/// `$UpCase` when it is formatted. `LCMapStringEx(LCMAP_UPPERCASE)` is linguistic instead: it
/// maps both `σ` and `ς` to `Σ` (same page), which NTFS keeps as two names. A volume formatted
/// by an older Windows can carry an older `$UpCase`; characters added to Unicode since then
/// may fold differently there.
pub fn upcase_name(name: &str) -> String {
    let units: Vec<u16> = name
        .encode_utf16()
        // SAFETY: a plain table lookup on a value; it has no pointers and no failure mode.
        .map(|unit| unsafe { RtlUpcaseUnicodeChar(unit) })
        .collect();
    String::from_utf16_lossy(&units)
}

/// A folder's spelling for comparison:
/// - canonical when it exists (short names, junctions and `subst` drives resolved);
/// - without `\\?\`, and `\\?\UNC\server\share` as `\\server\share`;
/// - `\` separators;
/// - no trailing separator, except on a bare drive root, which stays `x:\`;
/// - Unicode lowercase.
pub fn folder_key(path: &Path) -> String {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    spelling_key(&path)
}

/// The pure half of [`folder_key`]: everything except canonicalisation.
fn spelling_key(path: &Path) -> String {
    let text = strip_verbatim(path).to_string_lossy().replace('/', r"\");
    let trimmed = text.trim_end_matches('\\');
    let key = if trimmed.ends_with(':') {
        format!(r"{trimmed}\")
    } else {
        trimmed.to_string()
    };
    key.to_lowercase()
}

/// `path` relative to the folder `root`, with `/` separators, when it is inside it; `None` when
/// it isn't (or is `root` itself). Inside means under `root` as [`folder_key`] compares folders:
/// canonical (short names, junctions and `subst` drives resolved) and without regard to case.
/// The part of `path` that exists keeps its spelling on disk; a tail that doesn't exist (a file
/// just removed) keeps the spelling given.
pub fn relative_inside(root: &Path, path: &Path) -> Option<String> {
    let root =
        std::fs::canonicalize(root).map_or_else(|_| root.to_path_buf(), |c| strip_verbatim(&c));
    let mut tail: Vec<String> = Vec::new();
    let mut at = path.to_path_buf();
    let found = loop {
        match std::fs::canonicalize(&at) {
            Ok(c) => break strip_verbatim(&c),
            Err(_) => {
                tail.push(at.file_name()?.to_string_lossy().into_owned());
                at = at.parent()?.to_path_buf();
            }
        }
    };
    let head: Vec<std::path::Component<'_>> = root.components().collect();
    let all: Vec<std::path::Component<'_>> = found.components().collect();
    if all.len() < head.len() {
        return None;
    }
    let prefix: PathBuf = all[..head.len()].iter().collect();
    if spelling_key(&prefix) != spelling_key(&root) {
        return None;
    }
    let mut parts: Vec<String> = all[head.len()..]
        .iter()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.extend(tail.into_iter().rev());
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Removes a leading `\\?\` (and turns `\\?\UNC\server\share` into `\\server\share`). Keeps the
/// case and everything else: for paths that are shown, such as `WorkspaceRef.path`.
pub fn strip_verbatim(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::time::Duration;

    use super::*;
    use crate::process::run_hidden;

    #[test] // spec: ARCH-016, ARCH-073
    fn relative_inside_maps_paths_under_a_folder() {
        let dir = tempfile::Builder::new()
            .prefix("ww-paths-Ünïcödé space-")
            .tempdir()
            .unwrap();
        let root = dir.path().join("Work Space");
        std::fs::create_dir_all(root.join("Notes")).unwrap();
        std::fs::write(root.join("Notes").join("A.txt"), b"a").unwrap();
        // Another case finds the file and keeps the spelling on disk.
        let upper = PathBuf::from(root.to_string_lossy().to_uppercase()).join(r"notes\a.txt");
        assert_eq!(
            relative_inside(&root, &upper).as_deref(),
            Some("Notes/A.txt")
        );
        // A removed file keeps the name given.
        assert_eq!(
            relative_inside(&root, &root.join("Notes").join("gone.png")).as_deref(),
            Some("Notes/gone.png")
        );
        // Outside, beside (a common text prefix is no folder) and the folder itself.
        std::fs::create_dir_all(dir.path().join("Work Space 2")).unwrap();
        assert_eq!(
            relative_inside(&root, &dir.path().join("Work Space 2").join("x")),
            None
        );
        assert_eq!(relative_inside(&root, dir.path()), None);
        assert_eq!(relative_inside(&root, &root), None);
    }

    #[test] // spec: CORE-040
    fn upcase_name_follows_the_system_table() {
        assert_eq!(upcase_name("notes/a.txt"), "NOTES/A.TXT");
        assert_eq!(upcase_name("Ärger"), upcase_name("äRGER"));
        // One name on NTFS: U+1FB3 and U+1FBC.
        assert_eq!(upcase_name("\u{1FB3}"), upcase_name("\u{1FBC}"));
        // Two names on NTFS: final and medial sigma.
        assert_ne!(upcase_name("\u{03C2}"), upcase_name("\u{03C3}"));
        // One unit to one unit: lengths never change.
        for s in ["straße", "İı", "😀x"] {
            assert_eq!(
                upcase_name(s).encode_utf16().count(),
                s.encode_utf16().count(),
                "{s}"
            );
        }
    }

    #[test] // spec: ARCH-027, ARCH-097
    fn folder_key_strips_prefixes_case_and_trailing_separators() {
        assert_eq!(folder_key(Path::new(r"\\?\C:\Ws\")), r"c:\ws");
        assert_eq!(folder_key(Path::new(r"c:\ws")), r"c:\ws");
        assert_eq!(folder_key(Path::new("C:/Ws//")), r"c:\ws");
        // UNC spellings go through the pure half: canonicalising `\\srv\share` would look the
        // server up on the network, and tests use no network.
        assert_eq!(
            spelling_key(Path::new(r"\\?\UNC\srv\share\x")),
            r"\\srv\share\x"
        );
        assert_eq!(spelling_key(Path::new(r"\\srv\share\X\")), r"\\srv\share\x");
        assert_eq!(folder_key(Path::new(r"Z:\")), r"z:\");
        assert_eq!(folder_key(Path::new(r"\\?\Z:\")), r"z:\");
        assert_eq!(folder_key(Path::new("Z:")), r"z:\");
        // Unicode lowercase, not just ASCII.
        assert_eq!(
            folder_key(Path::new(r"Z:\NO SUCH\ÜNÏCÖDÉ SPACE")),
            r"z:\no such\ünïcödé space"
        );
    }

    #[test] // spec: ARCH-097
    fn strip_verbatim_keeps_case() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\C:\Ünïcödé space\Ws")),
            PathBuf::from(r"C:\Ünïcödé space\Ws")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\srv\share\X")),
            PathBuf::from(r"\\srv\share\X")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"C:\Plain\Ws")),
            PathBuf::from(r"C:\Plain\Ws")
        );
    }

    /// A junction compares equal to its target: `mklink /J` in a temp folder, run through
    /// `%ComSpec%` with relative names so no quoting is involved.
    #[test] // spec: ARCH-027
    fn folder_key_resolves_a_junction_to_its_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        let comspec = std::env::var_os("ComSpec").expect("ComSpec is set");
        let args: Vec<&OsStr> = ["/c", "mklink", "/J", "link", "target"]
            .iter()
            .map(OsStr::new)
            .collect();
        let out = run_hidden(
            Path::new(&comspec),
            &args,
            dir.path(),
            &[],
            Duration::from_secs(60),
        )
        .unwrap();
        assert_eq!(out.code, Some(0), "{out:?}");
        let link = dir.path().join("link");
        let target = dir.path().join("target");
        assert!(link.is_dir());
        assert_eq!(folder_key(&link), folder_key(&target));
        assert_ne!(folder_key(&link), folder_key(dir.path()));
        // The junction itself is removed, never its target's contents.
        std::fs::remove_dir(&link).unwrap();
    }
}
