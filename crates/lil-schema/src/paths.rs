//! Workspace-relative path rules (CORE-040; Appendix E "Workspace-relative paths").
//!
//! A workspace-relative path uses `/` and is checked before anything is written:
//! - at most [`PATH_BUDGET`] (180) characters;
//! - no component may be a reserved Windows name (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`,
//!   `LPT1`–`LPT9`, and `COM¹`–`COM³`/`LPT¹`–`LPT³`), whatever its case and extension;
//! - no component may end in a dot or a space, contain `<>:"\|?*` or a control character, or
//!   be `.` or `..`;
//! - every component is at most 255 characters.

/// The relative-path budget, in characters (CORE-040: "about 180", fixed at 180).
pub const PATH_BUDGET: usize = 180;

/// The longest component, in characters.
pub const COMPONENT_MAX: usize = 255;

/// Why a path was refused (`store.path_invalid`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathProblem {
    /// Longer than [`PATH_BUDGET`]; carries the length in characters.
    TooLong(usize),
    /// A component is a reserved Windows device name; carries the component.
    Reserved(String),
    /// A component is empty, `.` or `..`, ends in a dot or a space, holds a forbidden or
    /// control character, or is longer than 255 characters; carries the component.
    BadComponent(String),
    /// Empty, absolute (`/x`, `C:…`), or written with `\`.
    NotRelative,
}

impl std::fmt::Display for PathProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathProblem::TooLong(n) => write!(
                f,
                "the path is {n} characters long; the budget is {PATH_BUDGET}"
            ),
            PathProblem::Reserved(c) => write!(f, "`{c}` is a reserved Windows name"),
            PathProblem::BadComponent(c) => write!(f, "`{c}` is not a valid file or folder name"),
            PathProblem::NotRelative => {
                f.write_str("the path must be relative to the workspace and use `/`")
            }
        }
    }
}

/// Checks a workspace-relative path against the rules above.
pub fn check_rel_path(rel: &str) -> Result<(), PathProblem> {
    let b = rel.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    if rel.is_empty() || rel.starts_with('/') || rel.contains('\\') || drive {
        return Err(PathProblem::NotRelative);
    }
    let len = rel.chars().count();
    if len > PATH_BUDGET {
        return Err(PathProblem::TooLong(len));
    }
    rel.split('/').try_for_each(check_component)
}

/// `CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, `COM¹`–`COM³`, `LPT¹`–`LPT³`
/// (already uppercased).
fn is_reserved(stem: &str) -> bool {
    if matches!(stem, "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let Some(n) = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
    else {
        return false;
    };
    matches!(
        n,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "\u{b9}" | "\u{b2}" | "\u{b3}"
    )
}

/// Checks one path component (a file or folder name).
pub fn check_component(component: &str) -> Result<(), PathProblem> {
    let bad = || Err(PathProblem::BadComponent(component.to_string()));
    // Windows treats `NUL.tar.gz` and `NUL .txt` as the device too: the name before the first
    // dot, without trailing spaces, is what counts.
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ');
    if is_reserved(&stem.to_uppercase()) {
        return Err(PathProblem::Reserved(component.to_string()));
    }
    if component.is_empty() || component == "." || component == ".." {
        return bad();
    }
    if component.ends_with('.') || component.ends_with(' ') {
        return bad();
    }
    if component
        .chars()
        .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*'))
    {
        return bad();
    }
    if component.chars().count() > COMPONENT_MAX {
        return bad();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-040
    fn path_budget_is_180() {
        assert_eq!(PATH_BUDGET, 180);
        let ok = format!("projects/{}", "a".repeat(180 - 9));
        assert_eq!(ok.chars().count(), 180);
        assert_eq!(check_rel_path(&ok), Ok(()));
        let long = format!("{ok}b");
        assert_eq!(check_rel_path(&long), Err(PathProblem::TooLong(181)));
        // Characters, not bytes.
        let unicode = format!("projects/{}", "ü".repeat(171));
        assert_eq!(unicode.chars().count(), 180);
        assert_eq!(check_rel_path(&unicode), Ok(()));
        // A component may be at most 255 characters (checked only below the budget here).
        assert!(matches!(
            check_component(&"c".repeat(256)),
            Err(PathProblem::BadComponent(_))
        ));
        assert_eq!(check_component(&"c".repeat(255)), Ok(()));
    }

    #[test] // spec: CORE-040
    fn reserved_names_are_refused_in_any_case_and_extension() {
        for (path, component) in [
            ("con.png", "con.png"),
            ("assets/Lpt1", "Lpt1"),
            ("a/com¹.txt", "com¹.txt"),
            ("nul.tar.gz", "nul.tar.gz"),
            ("PRN", "PRN"),
            ("aux.", "aux."),
            ("x/COM9.json", "COM9.json"),
            ("LPT³", "LPT³"),
            ("Con .txt", "Con .txt"),
        ] {
            assert_eq!(
                check_rel_path(path),
                Err(PathProblem::Reserved(component.to_string())),
                "{path}"
            );
        }
        for fine in [
            "console.png",
            "com10.txt",
            "lpt0",
            "com.txt",
            "nul_x.png",
            "a.con",
            "Ünïcödé space/x.png",
        ] {
            assert_eq!(check_rel_path(fine), Ok(()), "{fine}");
        }
    }

    #[test] // spec: CORE-040
    fn trailing_dot_space_and_bad_chars_are_refused() {
        for (path, component) in [
            ("a/b.", "b."),
            ("a /b", "a "),
            ("a/b c ", "b c "),
            ("a/.", "."),
            ("../a", ".."),
            ("a//b", ""),
            ("a/", ""),
            ("a<b", "a<b"),
            ("a>b", "a>b"),
            ("ab:c", "ab:c"),
            ("a\"b", "a\"b"),
            ("a|b", "a|b"),
            ("a?b", "a?b"),
            ("a*b", "a*b"),
            ("a\u{1}b", "a\u{1}b"),
            ("a\tb", "a\tb"),
            ("a\u{7f}b", "a\u{7f}b"),
        ] {
            assert_eq!(
                check_rel_path(path),
                Err(PathProblem::BadComponent(component.to_string())),
                "{path:?}"
            );
        }
        // `a:b` is drive-relative on Windows (drive A), so it isn't workspace-relative.
        for path in ["", "/a", "a\\b", "C:/a", "c:", "a:b", "//server/share"] {
            assert_eq!(
                check_rel_path(path),
                Err(PathProblem::NotRelative),
                "{path}"
            );
        }
        for fine in [
            "projects/sketchbook/assets/chr_paper_crane/v1.0.0/crane_b.png",
            ".gitignore",
            ".lilith/state.json",
            "a b/c d.txt",
            "x..y",
        ] {
            assert_eq!(check_rel_path(fine), Ok(()), "{fine}");
        }
    }
}
