//! Targeted edits of `suite.toml` (CORE-007, ARCH-090) through `toml_edit`, so comments, key
//! order and unknown keys and tables survive. The core is the file's only writer.

use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value, value};

use crate::manifest::BOM;

/// Registers `path` as a workspace unless one is already registered, and with `make_default`
/// sets `default_workspace` to it. Returns the new text; when nothing changes it is `text`
/// itself, byte for byte.
///
/// - Paths are compared through `key` (ruling Q6): lil-core passes
///   `|p| lil_platform::paths::folder_key(Path::new(p))`, so `C:\Work` and `c:\work\` are the
///   same workspace. On a match the existing spelling is kept (also for the default), and a
///   differently spelled register is a no-op.
/// - Entries may be `[[workspaces]]` tables or an inline array (`workspaces = []`,
///   `workspaces = [{path = '…'}]`); a new entry follows the form already there.
/// - A leading UTF-8 byte-order mark is kept.
pub fn register(
    text: &str,
    path: &str,
    make_default: bool,
    key: &dyn Fn(&str) -> String,
) -> Result<String, String> {
    let (bom, body) = match text.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let mut doc: DocumentMut = body
        .parse()
        .map_err(|e| format!("suite.toml is not valid TOML: {e}"))?;
    let want = key(path);
    let mut changed = false;

    // The spelling of an entry that already registers this workspace.
    let existing: Option<String> = match doc.get("workspaces") {
        None => None,
        Some(Item::ArrayOfTables(list)) => list
            .iter()
            .filter_map(|t| t.get("path").and_then(Item::as_str))
            .find(|p| key(p) == want)
            .map(str::to_string),
        Some(Item::Value(Value::Array(list))) => {
            let mut found = None;
            for v in list.iter() {
                let Some(t) = v.as_inline_table() else {
                    return Err("suite.toml: a `workspaces` entry is not a table".into());
                };
                if let Some(p) = t.get("path").and_then(Value::as_str)
                    && key(p) == want
                    && found.is_none()
                {
                    found = Some(p.to_string());
                }
            }
            found
        }
        Some(_) => {
            return Err("suite.toml: `workspaces` is not a list of tables".into());
        }
    };
    let spelling = existing.clone().unwrap_or_else(|| path.to_string());

    if make_default {
        match doc.get_mut("default_workspace") {
            Some(item) if item.as_str().is_some_and(|d| key(d) == want) => {}
            Some(item) => {
                let Some(old) = item.as_value() else {
                    return Err("suite.toml: `default_workspace` is not a string".into());
                };
                let decor = old.decor().clone();
                let mut new = value(spelling.as_str());
                if let Some(v) = new.as_value_mut() {
                    *v.decor_mut() = decor;
                }
                *item = new;
                changed = true;
            }
            None => {
                // Root keys are written before the first table, so a new key never lands
                // inside `[[workspaces]]`.
                doc.insert("default_workspace", value(spelling.as_str()));
                changed = true;
            }
        }
    }

    if existing.is_none() {
        match doc.get_mut("workspaces") {
            Some(Item::ArrayOfTables(list)) => {
                let mut entry = Table::new();
                entry.insert("path", value(path));
                list.push(entry);
            }
            Some(Item::Value(Value::Array(list))) => {
                let mut entry = InlineTable::new();
                entry.insert("path", Value::from(path));
                list.push(entry);
            }
            _ => {
                let mut entry = Table::new();
                entry.insert("path", value(path));
                let mut list = ArrayOfTables::new();
                list.push(entry);
                doc.insert("workspaces", Item::ArrayOfTables(list));
            }
        }
        changed = true;
    }

    Ok(if changed {
        let mut out = doc.to_string();
        if bom {
            out.insert(0, BOM);
        }
        out
    } else {
        text.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "# note: the suite's workspace registry\n\
default_workspace = 'C:\\work\\first'\n\
theme = \"dark\" # kept\n\
\n\
[[workspaces]]\n\
path = 'C:\\work\\first'\n\
pinned = true\n\
\n\
[caps]\n\
monthly_usd = 50\n";

    #[test] // spec: CORE-007, ARCH-090
    fn suite_register_keeps_comments_and_unknown_tables() {
        let path = r"D:\Ünïcödé space\ws";
        let once = register(TEXT, path, false, &fold).unwrap();
        assert!(
            once.starts_with("# note: the suite's workspace registry\n"),
            "{once}"
        );
        assert!(once.contains("theme = \"dark\" # kept\n"), "{once}");
        assert!(once.contains("[caps]\nmonthly_usd = 50\n"), "{once}");
        assert!(once.contains("pinned = true\n"), "{once}");
        // Every line of the original survives, in order.
        let mut rest = once.as_str();
        for line in TEXT.lines() {
            let at = rest
                .find(line)
                .unwrap_or_else(|| panic!("lost {line:?}:\n{once}"));
            rest = &rest[at + line.len()..];
        }

        let suite = crate::SuiteToml::parse(&once).unwrap();
        let paths: Vec<&str> = suite.workspaces.iter().map(|w| w.path.as_str()).collect();
        assert_eq!(paths, [r"C:\work\first", path]);
        assert_eq!(suite.default_workspace.as_deref(), Some(r"C:\work\first"));
        assert_eq!(suite.other["caps"]["monthly_usd"].as_integer(), Some(50));

        // A second register is a no-op.
        assert_eq!(register(&once, path, false, &fold).unwrap(), once);
        assert_eq!(
            register(TEXT, r"C:\work\first", false, &fold).unwrap(),
            TEXT
        );
        assert_eq!(register(TEXT, r"C:\work\first", true, &fold).unwrap(), TEXT);

        // make_default replaces the value in place.
        let def = register(&once, path, true, &fold).unwrap();
        let suite = crate::SuiteToml::parse(&def).unwrap();
        assert_eq!(suite.default_workspace.as_deref(), Some(path));
        assert_eq!(suite.workspaces.len(), 2);
        assert!(def.starts_with("# note: the suite's workspace registry\ndefault_workspace = "));
        assert_eq!(register(&def, path, true, &fold).unwrap(), def);

        // An empty file gets the entry and the default.
        let fresh = register("", path, true, &fold).unwrap();
        let suite = crate::SuiteToml::parse(&fresh).unwrap();
        assert_eq!(suite.default_workspace.as_deref(), Some(path));
        assert_eq!(suite.workspaces.len(), 1);
        assert_eq!(suite.workspaces[0].path, path);

        // Text that isn't TOML, or a `workspaces` that isn't an array of tables, is refused.
        assert!(register("this is = = not toml", path, false, &fold).is_err());
        assert!(register("workspaces = 3\n", path, false, &fold).is_err());
        assert!(register("workspaces = [3]\n", path, false, &fold).is_err());
    }

    /// A stand-in for lil-core's `folder_key`: case-insensitive, no trailing separator.
    fn fold(p: &str) -> String {
        p.trim_end_matches('\\').to_lowercase()
    }

    #[test] // spec: CORE-007, ARCH-090
    fn suite_register_matches_paths_by_key_and_keeps_their_spelling() {
        // A differently cased (or trailing-separator) path is the same workspace: a no-op.
        assert_eq!(
            register(TEXT, r"c:\WORK\First\", false, &fold).unwrap(),
            TEXT
        );
        assert_eq!(register(TEXT, r"C:\Work\FIRST", true, &fold).unwrap(), TEXT);
        // An exact key sees a different workspace.
        let exact = |p: &str| p.to_string();
        let other = register(TEXT, r"c:\work\first", false, &exact).unwrap();
        assert_eq!(crate::SuiteToml::parse(&other).unwrap().workspaces.len(), 2);

        // Making a registered workspace the default writes the registered spelling.
        let two = "default_workspace = 'D:\\b'\n\n[[workspaces]]\npath = 'C:\\Work\\A'\n\n[[workspaces]]\npath = 'D:\\b'\n";
        let out = register(two, r"c:\work\a", true, &fold).unwrap();
        let suite = crate::SuiteToml::parse(&out).unwrap();
        assert_eq!(suite.default_workspace.as_deref(), Some(r"C:\Work\A"));
        assert_eq!(suite.workspaces.len(), 2);
    }

    #[test] // spec: CORE-007, ARCH-090
    fn suite_register_handles_inline_arrays_and_a_bom() {
        let path = r"D:\ws";
        // `workspaces = []` and inline tables.
        let out = register("# c\nworkspaces = []\n", path, false, &fold).unwrap();
        assert!(out.starts_with("# c\n"), "{out}");
        let suite = crate::SuiteToml::parse(&out).unwrap();
        assert_eq!(suite.workspaces.len(), 1);
        assert_eq!(suite.workspaces[0].path, path);
        assert_eq!(register(&out, r"d:\WS", false, &fold).unwrap(), out);

        let inline = "workspaces = [{path = 'C:\\a', pinned = true}]\n";
        let out = register(inline, path, true, &fold).unwrap();
        let suite = crate::SuiteToml::parse(&out).unwrap();
        let paths: Vec<&str> = suite.workspaces.iter().map(|w| w.path.as_str()).collect();
        assert_eq!(paths, [r"C:\a", path]);
        assert_eq!(suite.default_workspace.as_deref(), Some(path));
        assert_eq!(suite.workspaces[0].other["pinned"].as_bool(), Some(true));
        assert_eq!(register(inline, r"c:\A", false, &fold).unwrap(), inline);

        // A byte-order mark is kept on a rewrite, and a no-op is byte for byte.
        let bom = format!("\u{feff}{TEXT}");
        let out = register(&bom, path, false, &fold).unwrap();
        assert!(out.starts_with("\u{feff}# note:"), "{out:?}");
        assert_eq!(out.matches('\u{feff}').count(), 1);
        assert_eq!(register(&bom, r"C:\work\first", false, &fold).unwrap(), bom);
    }
}
