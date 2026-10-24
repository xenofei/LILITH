//! A tiny argument parser: `<command> [positional...] [--flag value | --switch]...`, and
//! everything after a bare `--` kept verbatim. Pure, so it is unit-tested.

use std::collections::BTreeMap;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub command: Option<String>,
    pub positional: Vec<String>,
    /// `--name value`; a flag with no value (end of input, or followed by another `--flag`)
    /// is stored with an empty value.
    pub flags: BTreeMap<String, String>,
    /// Everything after a bare `--`, untouched.
    pub rest: Vec<String>,
}

impl Args {
    pub fn parse<I: IntoIterator<Item = String>>(items: I) -> Args {
        let mut out = Args::default();
        let mut it = items.into_iter().peekable();
        while let Some(item) = it.next() {
            if item == "--" {
                out.rest.extend(it.by_ref());
                break;
            }
            if let Some(name) = item.strip_prefix("--") {
                let value = match it.peek() {
                    Some(next) if !next.starts_with("--") => it.next().unwrap_or_default(),
                    _ => String::new(),
                };
                out.flags.insert(name.to_string(), value);
            } else if out.command.is_none() {
                out.command = Some(item);
            } else {
                out.positional.push(item);
            }
        }
        out
    }

    pub fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.flags.get(name).map(String::as_str)
    }

    pub fn get_or<'a>(&'a self, name: &str, default: &'a str) -> &'a str {
        self.get(name).filter(|v| !v.is_empty()).unwrap_or(default)
    }

    /// A numeric flag; a missing or unparsable value gives `default`.
    pub fn num(&self, name: &str, default: u64) -> u64 {
        self.get(name)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &[&str]) -> Args {
        Args::parse(s.iter().map(|s| s.to_string()))
    }

    #[test]
    fn command_flags_switches_and_positionals() {
        let a = parse(&[
            "toast",
            "x",
            "--tag",
            "t1",
            "--suppress",
            "--out",
            "f.json",
            "y",
        ]);
        assert_eq!(a.command.as_deref(), Some("toast"));
        assert_eq!(a.positional, vec!["x", "y"]);
        assert_eq!(a.get("tag"), Some("t1"));
        assert!(a.has("suppress"));
        assert_eq!(a.get("suppress"), Some(""));
        assert_eq!(a.get("out"), Some("f.json"));
    }

    #[test]
    fn a_trailing_switch_has_an_empty_value() {
        let a = parse(&["p", "--blocker"]);
        assert!(a.has("blocker"));
        assert_eq!(a.get_or("blocker", "d"), "d");
    }

    #[test]
    fn everything_after_double_dash_is_kept_verbatim() {
        let a = parse(&["spawn", "--", "C:\\x y\\a.exe", "--out", "f", "--"]);
        assert_eq!(a.command.as_deref(), Some("spawn"));
        assert!(a.flags.is_empty());
        assert_eq!(a.rest, vec!["C:\\x y\\a.exe", "--out", "f", "--"]);
    }

    #[test]
    fn numbers_fall_back_to_the_default() {
        let a = parse(&["c", "--secs", "20", "--bad", "x"]);
        assert_eq!(a.num("secs", 1), 20);
        assert_eq!(a.num("bad", 7), 7);
        assert_eq!(a.num("missing", 3), 3);
    }
}
