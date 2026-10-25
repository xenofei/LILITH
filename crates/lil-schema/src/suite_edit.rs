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

/// A change of `[caps]` (`app/caps.set`, CORE-008): each field `None` leaves its key as it is,
/// `Some(None)` removes it and `Some(Some(amount))` sets it (a decimal string in the display
/// currency, written as a TOML string).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapsEdit {
    pub monthly: Option<Option<String>>,
    pub per_call_max: Option<Option<String>>,
    /// `[caps.provider_monthly]`, per provider id.
    pub provider_monthly: Vec<(String, Option<String>)>,
    /// The display currency is `USD`: a legacy `monthly_usd`, `per_call_max_usd` or
    /// `[caps.provider_monthly_usd]` value whose new key is absent is moved to the new key
    /// first, so removing the legacy keys never drops a cap.
    pub carry_legacy: bool,
}

/// The legacy keys of the workspace format's `[caps]`, with the key each became (Appendix F).
const LEGACY_CAPS: [(&str, &str); 3] = [
    ("monthly_usd", "monthly"),
    ("per_call_max_usd", "per_call_max"),
    ("provider_monthly_usd", "provider_monthly"),
];

/// Applies `edit` to `[caps]` (Appendix F "Caps and money in `suite.toml`"): the new keys are
/// written, the legacy `_usd` keys removed (carried to the new keys first with
/// [`CapsEdit::carry_legacy`]), and comments, key order and every other key and table are
/// kept. Returns the new text; when nothing changes it is `text` itself, byte for byte. A
/// leading UTF-8 byte-order mark is kept.
///
/// # Errors
/// The text isn't TOML, or `caps` or `caps.provider_monthly` is not a table.
pub fn set_caps(text: &str, edit: &CapsEdit) -> Result<String, String> {
    let (bom, body) = match text.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let mut doc: DocumentMut = body
        .parse()
        .map_err(|e| format!("suite.toml is not valid TOML: {e}"))?;
    let before = doc.to_string();
    if doc.get("caps").is_none() {
        doc.insert("caps", Item::Table(implicit()));
    }
    let caps = doc
        .get_mut("caps")
        .and_then(Item::as_table_like_mut)
        .ok_or("suite.toml: `caps` is not a table")?;
    if edit.carry_legacy {
        for (legacy, new) in LEGACY_CAPS {
            let Some(old) = caps.get(legacy).cloned() else {
                continue;
            };
            if new != "provider_monthly" {
                if caps.get(new).is_none() {
                    caps.insert(new, old);
                }
                continue;
            }
            let Some(old) = old.as_table_like() else {
                return Err("suite.toml: `caps.provider_monthly_usd` is not a table".into());
            };
            let carried: Vec<(String, Item)> = old
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect();
            let target = provider_table(caps)?;
            for (provider, value) in carried {
                if target.get(&provider).is_none() {
                    target.insert(&provider, value);
                }
            }
        }
    }
    for (legacy, _) in LEGACY_CAPS {
        caps.remove(legacy);
    }
    for (key, change) in [
        ("monthly", &edit.monthly),
        ("per_call_max", &edit.per_call_max),
    ] {
        match change {
            None => {}
            Some(None) => {
                caps.remove(key);
            }
            Some(Some(amount)) => set_value(caps, key, amount),
        }
    }
    if !edit.provider_monthly.is_empty() {
        let target = provider_table(caps)?;
        for (provider, change) in &edit.provider_monthly {
            match change {
                None => {
                    target.remove(provider);
                }
                Some(amount) => set_value(target, provider, amount),
            }
        }
    }
    let after = doc.to_string();
    Ok(if after == before {
        text.to_string()
    } else {
        let mut out = after;
        if bom {
            out.insert(0, BOM);
        }
        out
    })
}

/// `[caps.provider_monthly]`, made when it is missing.
fn provider_table(
    caps: &mut dyn toml_edit::TableLike,
) -> Result<&mut dyn toml_edit::TableLike, String> {
    if caps.get("provider_monthly").is_none() {
        caps.insert("provider_monthly", Item::Table(implicit()));
    }
    caps.get_mut("provider_monthly")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| "suite.toml: `caps.provider_monthly` is not a table".to_string())
}

/// A new table that is written only once it holds a key.
fn implicit() -> Table {
    let mut t = Table::new();
    t.set_implicit(true);
    t
}

/// Sets `key` to the string `amount`, keeping the decor (comments) of a value it replaces.
fn set_value(table: &mut dyn toml_edit::TableLike, key: &str, amount: &str) {
    let mut new = value(amount);
    if let (Some(old), Some(v)) = (table.get(key).and_then(Item::as_value), new.as_value_mut()) {
        *v.decor_mut() = old.decor().clone();
    }
    table.insert(key, new);
}

/// A change of the desktop settings (`app/desktop.set`; Appendix G "Notifications"): each
/// `Some` is written, each `None` left as it is. `notifications` holds `(category, on)` pairs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopEdit {
    pub notifications: Vec<(String, bool)>,
    pub quiet_while_fullscreen: Option<bool>,
    pub fullscreen_autopause: Option<bool>,
    pub keep_awake_gpu: Option<bool>,
}

/// Applies `edit`: the categories and `quiet_while_fullscreen` under `[notifications]`,
/// `fullscreen_autopause` under `[jobs]` and `keep_awake_gpu` under `[power]`, each a TOML
/// boolean; a missing table is added. Comments, key order and every other key and table are
/// kept. Returns the new text; when nothing changes it is `text` itself, byte for byte. A
/// leading UTF-8 byte-order mark is kept.
///
/// # Errors
/// The text isn't TOML, or `notifications`, `jobs` or `power` isn't a table.
pub fn set_desktop(text: &str, edit: &DesktopEdit) -> Result<String, String> {
    let (bom, body) = match text.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let mut doc: DocumentMut = body
        .parse()
        .map_err(|e| format!("suite.toml is not valid TOML: {e}"))?;
    let before = doc.to_string();
    let mut changes: Vec<(&str, &str, bool)> = edit
        .notifications
        .iter()
        .map(|(category, on)| ("notifications", category.as_str(), *on))
        .collect();
    for (table, key, value) in [
        (
            "notifications",
            "quiet_while_fullscreen",
            edit.quiet_while_fullscreen,
        ),
        ("jobs", "fullscreen_autopause", edit.fullscreen_autopause),
        ("power", "keep_awake_gpu", edit.keep_awake_gpu),
    ] {
        if let Some(value) = value {
            changes.push((table, key, value));
        }
    }
    for (table, key, on) in changes {
        if doc.get(table).is_none() {
            doc.insert(table, Item::Table(implicit()));
        }
        let t = doc
            .get_mut(table)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("suite.toml: `{table}` is not a table"))?;
        if t.get(key).and_then(Item::as_bool) == Some(on) {
            continue;
        }
        let mut new = value(on);
        match t.get_mut(key) {
            // In place, so the key keeps the comment above it and the value its own.
            Some(item) => {
                if let (Some(old), Some(v)) = (item.as_value(), new.as_value_mut()) {
                    *v.decor_mut() = old.decor().clone();
                }
                *item = new;
            }
            None => {
                t.insert(key, new);
            }
        }
    }
    let after = doc.to_string();
    Ok(if after == before {
        text.to_string()
    } else {
        let mut out = after;
        if bom {
            out.insert(0, BOM);
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: APP-146, CORE-007
    fn set_desktop_writes_booleans_and_keeps_comments_and_other_keys() {
        let text = "# my suite, by hand\ndefault_workspace = 'C:\\w'\n\n\
                    [notifications]\n# off while I stream\njob_finished = true # was on\n\
                    job_progress = true\n\n[jobs]\npreempt_agent_batches = true\n";
        let edit = DesktopEdit {
            notifications: vec![("job_finished".into(), false), ("cap_alert".into(), false)],
            quiet_while_fullscreen: None,
            fullscreen_autopause: Some(true),
            keep_awake_gpu: Some(false),
        };
        let out = set_desktop(text, &edit).unwrap();
        assert!(out.starts_with("# my suite, by hand\n"), "{out}");
        assert!(out.contains("# off while I stream\n"), "{out}");
        assert!(out.contains("job_finished = false # was on\n"), "{out}");
        assert!(
            out.contains("job_progress = true\n"),
            "unknown keys stay: {out}"
        );
        assert!(out.contains("preempt_agent_batches = true\n"), "{out}");
        assert!(out.contains("[power]\nkeep_awake_gpu = false\n"), "{out}");
        let suite = crate::SuiteToml::parse(&out).unwrap();
        let desktop = suite.desktop();
        assert!(desktop.fullscreen_autopause && !desktop.keep_awake_gpu);
        assert!(desktop.quiet_while_fullscreen, "left as it was");
        let on = |c: &str| {
            desktop
                .notifications
                .iter()
                .find(|(k, _)| *k == c)
                .unwrap()
                .1
        };
        assert!(!on("job_finished") && !on("cap_alert") && on("job_failed"));
        assert!(suite.preempt_agent_batches());
        // A no-op is the text itself; a BOM is kept.
        assert_eq!(set_desktop(&out, &edit).unwrap(), out);
        assert_eq!(set_desktop(text, &DesktopEdit::default()).unwrap(), text);
        let bom = format!("\u{feff}{text}");
        let out = set_desktop(&bom, &edit).unwrap();
        assert!(out.starts_with("\u{feff}# my suite"), "{out:?}");
        // An empty file gets the tables.
        let fresh = set_desktop("", &edit).unwrap();
        assert!(
            crate::SuiteToml::parse(&fresh)
                .unwrap()
                .desktop()
                .fullscreen_autopause
        );
        // A table that isn't one, or text that isn't TOML, is refused.
        assert!(set_desktop("power = 3\n", &edit).is_err());
        assert!(set_desktop("this = = not toml", &edit).is_err());
    }

    #[test] // spec: CORE-008, CORE-085
    fn set_caps_writes_new_keys_drops_legacy_ones_and_keeps_comments() {
        let text = "# my caps, set by hand\n[money]\ndisplay_currency = \"USD\"\n\
                    [caps]\nmonthly_usd = 50 # the old key\nper_call_max_usd = \"2\"\n\
                    [caps.provider_monthly_usd]\nfakeprov = \"3\"\n";
        let edit = CapsEdit {
            monthly: Some(Some("5".into())),
            carry_legacy: true,
            ..CapsEdit::default()
        };
        let out = set_caps(text, &edit).unwrap();
        assert!(out.starts_with("# my caps, set by hand\n"), "{out}");
        let suite = crate::SuiteToml::parse(&out).unwrap();
        let caps = &suite.other["caps"];
        assert_eq!(caps["monthly"].as_str(), Some("5"), "{out}");
        // Carried, not lost: the legacy per-call and provider caps keep their values.
        assert_eq!(caps["per_call_max"].as_str(), Some("2"), "{out}");
        assert_eq!(
            caps["provider_monthly"]["fakeprov"].as_str(),
            Some("3"),
            "{out}"
        );
        for legacy in ["monthly_usd", "per_call_max_usd", "provider_monthly_usd"] {
            assert!(caps.get(legacy).is_none(), "{legacy} stayed: {out}");
        }
        // Clearing and the provider table; a no-op returns the text itself.
        let cleared = set_caps(
            &out,
            &CapsEdit {
                per_call_max: Some(None),
                provider_monthly: vec![
                    ("fakeprov".into(), None),
                    ("other".into(), Some("1".into())),
                ],
                ..CapsEdit::default()
            },
        )
        .unwrap();
        let suite = crate::SuiteToml::parse(&cleared).unwrap();
        let caps = &suite.other["caps"];
        assert!(caps.get("per_call_max").is_none(), "{cleared}");
        assert!(
            caps["provider_monthly"].get("fakeprov").is_none(),
            "{cleared}"
        );
        assert_eq!(caps["provider_monthly"]["other"].as_str(), Some("1"));
        assert_eq!(set_caps(&cleared, &CapsEdit::default()).unwrap(), cleared);
        // Without carry_legacy (another display currency) the legacy keys simply go.
        let dropped = set_caps("[caps]\nmonthly_usd = 9\n", &CapsEdit::default()).unwrap();
        assert!(!dropped.contains("monthly"), "{dropped}");
        assert!(set_caps("caps = 3\n", &CapsEdit::default()).is_err());
    }

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
