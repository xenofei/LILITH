//! Targeted edits of `suite.toml` (CORE-007, ARCH-090) through `toml_edit`, so comments, key
//! order and unknown keys and tables survive. The core is the file's only writer.

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

/// Registers `path` as a `[[workspaces]]` entry unless an entry already has exactly that
/// `path`, and with `make_default` sets `default_workspace` to it. Returns the new text; when
/// nothing changes it is `text` itself, byte for byte.
///
/// Paths are compared as written: the caller passes the form it registers (the core's
/// canonical workspace path).
pub fn register(text: &str, path: &str, make_default: bool) -> Result<String, String> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("suite.toml is not valid TOML: {e}"))?;
    let mut changed = false;

    if make_default {
        match doc.get_mut("default_workspace") {
            Some(item) if item.as_str() == Some(path) => {}
            Some(item) => {
                let Some(old) = item.as_value() else {
                    return Err("suite.toml: `default_workspace` is not a string".into());
                };
                let decor = old.decor().clone();
                let mut new = value(path);
                if let Some(v) = new.as_value_mut() {
                    *v.decor_mut() = decor;
                }
                *item = new;
                changed = true;
            }
            None => {
                // Root keys are written before the first table, so a new key never lands
                // inside `[[workspaces]]`.
                doc.insert("default_workspace", value(path));
                changed = true;
            }
        }
    }

    let registered = match doc.get("workspaces") {
        None => false,
        Some(Item::ArrayOfTables(list)) => list
            .iter()
            .any(|t| t.get("path").and_then(Item::as_str) == Some(path)),
        Some(_) => return Err("suite.toml: `workspaces` is not an array of tables".into()),
    };
    if !registered {
        let mut entry = Table::new();
        entry.insert("path", value(path));
        match doc.get_mut("workspaces") {
            Some(Item::ArrayOfTables(list)) => list.push(entry),
            _ => {
                let mut list = ArrayOfTables::new();
                list.push(entry);
                doc.insert("workspaces", Item::ArrayOfTables(list));
            }
        }
        changed = true;
    }

    Ok(if changed {
        doc.to_string()
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
        let once = register(TEXT, path, false).unwrap();
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
        assert_eq!(register(&once, path, false).unwrap(), once);
        assert_eq!(register(TEXT, r"C:\work\first", false).unwrap(), TEXT);
        assert_eq!(register(TEXT, r"C:\work\first", true).unwrap(), TEXT);

        // make_default replaces the value in place.
        let def = register(&once, path, true).unwrap();
        let suite = crate::SuiteToml::parse(&def).unwrap();
        assert_eq!(suite.default_workspace.as_deref(), Some(path));
        assert_eq!(suite.workspaces.len(), 2);
        assert!(def.starts_with("# note: the suite's workspace registry\ndefault_workspace = "));
        assert_eq!(register(&def, path, true).unwrap(), def);

        // An empty file gets the entry and the default.
        let fresh = register("", path, true).unwrap();
        let suite = crate::SuiteToml::parse(&fresh).unwrap();
        assert_eq!(suite.default_workspace.as_deref(), Some(path));
        assert_eq!(suite.workspaces.len(), 1);
        assert_eq!(suite.workspaces[0].path, path);

        // Text that isn't TOML, or a `workspaces` that isn't an array of tables, is refused.
        assert!(register("this is = = not toml", path, false).is_err());
        assert!(register("workspaces = 3\n", path, false).is_err());
    }
}
