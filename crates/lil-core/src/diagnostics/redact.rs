//! The diagnostics bundle's redaction (ARCH-121, QA-084, AI-092; Appendix G "Diagnostics").
//!
//! In every entry these become `<redacted>`: the value under a key holding one of Appendix F's
//! scrub words ([`SCRUB_WORDS`], compared upper-cased), every Claude Code profile label of
//! `suite.toml` (as written and JSON-escaped), every e-mail address
//! (`[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}`), every Credential Manager target
//! ([`names::APP_NAME`] or [`names::LEGACY_APP_NAME`], an optional `-<channel>`, then
//! `/<name>`, in any letter case) and a Deny URL's token (`deny/<confirmation id>/<token>`). The user-profile folder becomes
//! `%USERPROFILE%` in its long form and its 8.3 short form, in any letter case, with `\` or
//! `/` separators, with a `\\?\` prefix and JSON-escaped with doubled backslashes; the
//! computer name becomes `<machine>` in any letter case and in its machine-id spelling, as a
//! whole word (owner decision 2026-10-22). Each replacement counts once.
//!
//! In text (the logs, line by line, and the JSON entries) a key is a run of letters, digits,
//! `_`, `-` and `.`, optionally quoted (`"key"`, `'key'`, or `\"key\"` inside a JSON string),
//! followed by `=` or `:` (never `::`, a Rust path, and never a `.rs` file's line number); its
//! value is the quoted string, the bracketed JSON, or the word after it (two words after
//! `Bearer`, `Basic`, `Digest` or `Token`). In `suite.toml` the TOML is read
//! ([`Redactor::toml`]): every value under a scrub-word key or table, and each
//! `[[claude_code.profiles]]` `label`, then the text rules over the whole file, comments
//! included. A `suite.toml` that doesn't parse is redacted line by line.

use std::path::Path;

use lil_platform::names;

/// What a redacted value becomes.
pub const REDACTED: &str = "<redacted>";
/// What the user-profile folder becomes.
pub const PROFILE: &str = "%USERPROFILE%";
/// What the computer name becomes.
pub const MACHINE: &str = "<machine>";
/// Appendix F's scrub words: a key holding one (upper-cased) has its value redacted.
pub const SCRUB_WORDS: [&str; 7] = [
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "AUTH",
];

/// What the bundle redacts beyond the fixed rules: the Claude Code profile labels of
/// `suite.toml` (`[[claude_code.profiles]] {id, label, config_dir}`, AI-088), the user-profile
/// folder and the computer name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redactor {
    pub labels: Vec<String>,
    pub profile_dir: Option<String>,
    pub machine: Option<String>,
}

impl Redactor {
    /// `s` redacted, and how many replacements were made.
    pub fn text(&self, s: &str) -> (String, u32) {
        self.compile().text(s)
    }

    /// The `suite.toml` text `doc` redacted, and how many replacements were made.
    pub fn toml(&self, doc: &str) -> (String, u32) {
        self.compile().toml(doc)
    }

    /// The forms to look for, worked out once (the 8.3 short name is asked of Windows here).
    pub(crate) fn compile(&self) -> Compiled {
        let mut profiles: Vec<ProfileForm> = Vec::new();
        if let Some(long) = self.profile_dir.as_deref().filter(|p| !p.trim().is_empty()) {
            let long = long.trim_start_matches(r"\\?\");
            profiles.extend(ProfileForm::parse(long));
            if let Ok(short) = lil_platform::fs::short_name(Path::new(long))
                && let Some(form) = ProfileForm::parse(&short.to_string_lossy())
                && !profiles.contains(&form)
            {
                profiles.push(form);
            }
        }
        // The more specific form first.
        profiles.sort_by_key(|f| std::cmp::Reverse(f.len()));
        let mut labels: Vec<String> = Vec::new();
        for label in self.labels.iter().filter(|l| !l.is_empty()) {
            let escaped = serde_json::to_string(label).unwrap_or_default();
            let escaped = escaped
                .strip_prefix('"')
                .and_then(|e| e.strip_suffix('"'))
                .unwrap_or_default()
                .to_string();
            for form in [label.clone(), escaped] {
                if !form.is_empty() && !labels.contains(&form) {
                    labels.push(form);
                }
            }
        }
        labels.sort_by_key(|l| std::cmp::Reverse(l.len()));
        let mut machines: Vec<String> = Vec::new();
        if let Some(name) = self.machine.as_deref().filter(|m| !m.is_empty()) {
            for form in [
                name.to_ascii_lowercase(),
                lil_platform::machine_id_from(name),
            ] {
                if !machines.contains(&form) {
                    machines.push(form);
                }
            }
        }
        machines.sort_by_key(|m| std::cmp::Reverse(m.len()));
        Compiled {
            profiles,
            labels,
            machines,
        }
    }
}

/// A [`Redactor`] with its forms worked out, for redacting many lines.
#[derive(Debug, Clone)]
pub(crate) struct Compiled {
    profiles: Vec<ProfileForm>,
    labels: Vec<String>,
    machines: Vec<String>,
}

impl Compiled {
    /// [`Redactor::text`].
    pub(crate) fn text(&self, s: &str) -> (String, u32) {
        let mut n = 0;
        let s = self.profile(s, &mut n);
        let s = deny_tokens(&s, &mut n);
        let s = targets(&s, &mut n);
        let s = self.labels(&s, &mut n);
        let s = emails(&s, &mut n);
        let s = self.machine(&s, &mut n);
        let s = keys(&s, &mut n);
        (s, n)
    }

    /// [`Redactor::toml`].
    pub(crate) fn toml(&self, doc: &str) -> (String, u32) {
        let mut n = 0;
        let structured = match doc.parse::<toml_edit::DocumentMut>() {
            Ok(mut parsed) => {
                scrub_table(parsed.as_table_mut(), &mut n);
                scrub_labels(&mut parsed, &mut n);
                parsed.to_string()
            }
            Err(_) => scrub_lines(doc, &mut n),
        };
        let (text, m) = self.text(&structured);
        (text, n + m)
    }

    fn profile(&self, s: &str, n: &mut u32) -> String {
        if self.profiles.is_empty() {
            return s.to_string();
        }
        let b = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let (mut last, mut i) = (0, 0);
        while i < b.len() {
            let candidate = b[i] == b'\\'
                || b[i] == b'/'
                || (b[i].is_ascii_alphabetic() && b.get(i + 1) == Some(&b':'));
            if candidate && let Some(end) = self.profiles.iter().find_map(|f| f.match_at(s, i)) {
                out.push_str(&s[last..i]);
                out.push_str(PROFILE);
                *n += 1;
                last = end;
                i = end;
                continue;
            }
            i += 1;
        }
        out.push_str(&s[last..]);
        out
    }

    fn labels(&self, s: &str, n: &mut u32) -> String {
        let mut s = s.to_string();
        for label in &self.labels {
            let found = s.matches(label.as_str()).count();
            if found > 0 {
                s = s.replace(label.as_str(), REDACTED);
                *n += u32::try_from(found).unwrap_or(u32::MAX);
            }
        }
        s
    }

    fn machine(&self, s: &str, n: &mut u32) -> String {
        let mut s = s.to_string();
        for form in &self.machines {
            s = replace_words(&s, form, MACHINE, n);
        }
        s
    }
}

/// The user-profile folder in one spelling: its drive letter (lower-cased) and its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileForm {
    drive: Option<u8>,
    parts: Vec<String>,
}

impl ProfileForm {
    /// `path` (`C:\Users\<user>`, `/` or `\` separators) as a form; `None` with no part.
    fn parse(path: &str) -> Option<ProfileForm> {
        let path = path.replace('/', "\\");
        let mut segments = path.split('\\').filter(|s| !s.is_empty()).peekable();
        let drive = match segments.peek() {
            Some(first) if first.len() == 2 && first.as_bytes()[1] == b':' => {
                let letter = first.as_bytes()[0].to_ascii_lowercase();
                segments.next();
                letter.is_ascii_alphabetic().then_some(letter)
            }
            _ => None,
        };
        let parts: Vec<String> = segments.map(str::to_string).collect();
        (!parts.is_empty()).then_some(ProfileForm { drive, parts })
    }

    fn len(&self) -> usize {
        self.parts.iter().map(String::len).sum::<usize>() + self.parts.len()
    }

    /// The end of this form in `s` from byte `i`, if it is there: an optional `\\?\` prefix,
    /// the drive (or, without a prefix, none), then each part after a separator, in any letter
    /// case. A separator is any run of `\` or `/`, so a path escaped once (JSON) or twice
    /// (`Debug` text inside a JSON line) matches too, and so does the prefix in any of those
    /// spellings.
    fn match_at(&self, s: &str, i: usize) -> Option<usize> {
        let b = s.as_bytes();
        let mut j = i;
        let mut prefixed = false;
        if let Some(lead) = separator_at(b, j)
            && b.get(j + lead) == Some(&b'?')
            && let Some(tail) = separator_at(b, j + lead + 1)
        {
            j += lead + 1 + tail;
            prefixed = true;
        }
        if let Some(drive) = self.drive {
            if b.get(j).map(u8::to_ascii_lowercase) == Some(drive) && b.get(j + 1) == Some(&b':') {
                j += 2;
            } else if prefixed {
                return None;
            }
        }
        for part in &self.parts {
            j += separator_at(b, j)?;
            j = match_ignoring_case(s, j, part)?;
        }
        Some(j)
    }
}

/// The length of the path separator at `j`: a run of `\` or `/` (one, or more when escaped).
fn separator_at(b: &[u8], j: usize) -> Option<usize> {
    let run = b
        .get(j..)?
        .iter()
        .take_while(|c| matches!(c, b'\\' | b'/'))
        .count();
    (run > 0).then_some(run)
}

/// The end of `word` in `s` from byte `j`, compared ignoring letter case.
fn match_ignoring_case(s: &str, j: usize, word: &str) -> Option<usize> {
    let rest = s.get(j..)?;
    let mut text = rest.char_indices();
    for want in word.chars() {
        let (_, got) = text.next()?;
        if !got.to_lowercase().eq(want.to_lowercase()) {
            return None;
        }
    }
    Some(j + text.next().map_or(rest.len(), |(k, _)| k))
}

/// Every whole-word `word` (ASCII letter case ignored; the neighbours aren't ASCII letters or
/// digits) replaced by `with`.
fn replace_words(s: &str, word: &str, with: &str, n: &mut u32) -> String {
    let (b, w) = (s.as_bytes(), word.as_bytes());
    if w.is_empty() || b.len() < w.len() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i + w.len() <= b.len() {
        let before_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let after_ok = b
            .get(i + w.len())
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        if before_ok && after_ok && b[i..i + w.len()].eq_ignore_ascii_case(w) {
            out.push_str(&s[last..i]);
            out.push_str(with);
            *n += 1;
            i += w.len();
            last = i;
            continue;
        }
        i += 1;
    }
    out.push_str(&s[last..]);
    out
}

/// Whether `key` holds a scrub word (upper-cased).
fn holds_scrub_word(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SCRUB_WORDS.iter().any(|w| upper.contains(w))
}

/// `deny/cf_<16 hex>/<32 hex>` with its token replaced.
fn deny_tokens(s: &str, n: &mut u32) -> String {
    const HEAD: &str = "deny/cf_";
    let b = s.as_bytes();
    let hex = |range: std::ops::Range<usize>| {
        b.get(range)
            .is_some_and(|r| r.iter().all(u8::is_ascii_hexdigit))
    };
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i + HEAD.len() <= b.len() {
        if b[i..i + HEAD.len()].eq_ignore_ascii_case(HEAD.as_bytes()) {
            let id = i + HEAD.len();
            let token = id + 17;
            if hex(id..id + 16) && b.get(id + 16) == Some(&b'/') && hex(token..token + 32) {
                out.push_str(&s[last..token]);
                out.push_str(REDACTED);
                *n += 1;
                last = token + 32;
                i = last;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&s[last..]);
    out
}

/// Every Credential Manager target replaced: the suite's name or its name before the rename
/// (any case, not inside a longer word), an optional `-<channel>`, `/`, then the name up to a
/// space, quote, backslash or closing bracket.
fn targets(s: &str, n: &mut u32) -> String {
    let b = s.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-';
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i < b.len() {
        if i > 0 && word(b[i - 1]) {
            i += 1;
            continue;
        }
        let head = [names::APP_NAME, names::LEGACY_APP_NAME]
            .iter()
            .find_map(|h| {
                let end = i + h.len();
                b.get(i..end)
                    .is_some_and(|w| w.eq_ignore_ascii_case(h.as_bytes()))
                    .then_some(end)
            });
        let Some(mut j) = head else {
            i += 1;
            continue;
        };
        if b.get(j) == Some(&b'-') {
            let start = j + 1;
            j = start;
            while b.get(j).is_some_and(|c| word(*c)) {
                j += 1;
            }
            if j == start {
                i += 1;
                continue;
            }
        }
        if b.get(j) != Some(&b'/') {
            i += 1;
            continue;
        }
        let name = j + 1;
        let mut end = name;
        while b.get(end).is_some_and(|c| {
            !(c.is_ascii_whitespace()
                || matches!(
                    c,
                    b'"' | b'\'' | b'\\' | b',' | b';' | b')' | b']' | b'}' | b'<' | b'>' | b'`'
                ))
        }) {
            end += 1;
        }
        if end == name {
            i += 1;
            continue;
        }
        out.push_str(&s[last..i]);
        out.push_str(REDACTED);
        *n += 1;
        last = end;
        i = end;
    }
    out.push_str(&s[last..]);
    out
}

/// Every e-mail address (`[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}`, leftmost and
/// longest as a regex finds it) replaced. A local part that would start inside a JSON escape
/// (`\n…`) starts after it, so the JSON stays JSON.
fn emails(s: &str, n: &mut u32) -> String {
    let b = s.as_bytes();
    let local = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'%' | b'+' | b'-');
    let domain = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-');
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while let Some(off) = b.get(i..).and_then(|r| r.iter().position(|c| *c == b'@')) {
        let at = i + off;
        i = at + 1;
        let mut start = at;
        while start > last && local(b[start - 1]) {
            start -= 1;
        }
        if start > 0 && b[start - 1] == b'\\' {
            let backslashes = b[..start].iter().rev().take_while(|c| **c == b'\\').count();
            if backslashes % 2 == 1 {
                start += 1;
            }
        }
        if start >= at {
            continue;
        }
        let mut stop = at + 1;
        while stop < b.len() && domain(b[stop]) {
            stop += 1;
        }
        // The rightmost `.` with at least two letters after it, and a domain part before it.
        let mut end = None;
        let mut dot = stop;
        while dot > at + 2 {
            dot -= 1;
            if b[dot] == b'.' {
                let letters = b[dot + 1..stop]
                    .iter()
                    .take_while(|c| c.is_ascii_alphabetic())
                    .count();
                if letters >= 2 {
                    end = Some(dot + 1 + letters);
                    break;
                }
            }
        }
        if let Some(end) = end {
            out.push_str(&s[last..start]);
            out.push_str(REDACTED);
            *n += 1;
            last = end;
            i = end;
        }
    }
    out.push_str(&s[last..]);
    out
}

/// How the key before a value was quoted: what a bare value's replacement is wrapped in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Double,
    Single,
    /// `\"key\"` inside a JSON string.
    Escaped,
}

/// Every value under a scrub-word key replaced (module docs).
fn keys(s: &str, n: &mut u32) -> String {
    let b = s.as_bytes();
    let key_byte = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.');
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i < b.len() {
        if !key_byte(b[i]) || (i > 0 && key_byte(b[i - 1])) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && key_byte(b[i]) {
            i += 1;
        }
        let key = &s[start..i];
        if !holds_scrub_word(key) || key.to_ascii_lowercase().ends_with(".rs") {
            continue;
        }
        let mut k = i;
        let quote = if s[k..].starts_with("\\\"") {
            k += 2;
            Quote::Escaped
        } else {
            match b.get(k) {
                Some(b'"') => {
                    k += 1;
                    Quote::Double
                }
                Some(b'\'') => {
                    k += 1;
                    Quote::Single
                }
                _ => Quote::None,
            }
        };
        while matches!(b.get(k), Some(b' ' | b'\t')) {
            k += 1;
        }
        let separator = match b.get(k) {
            Some(b'=') => true,
            Some(b':') => b.get(k + 1) != Some(&b':') && (k == 0 || b[k - 1] != b':'),
            _ => false,
        };
        if !separator {
            continue;
        }
        k += 1;
        while matches!(b.get(k), Some(b' ' | b'\t')) {
            k += 1;
        }
        let Some((value_end, with)) = value_at(s, k, quote) else {
            continue;
        };
        out.push_str(&s[last..k]);
        out.push_str(&with);
        *n += 1;
        last = value_end;
        i = value_end;
    }
    out.push_str(&s[last..]);
    out
}

/// The value starting at byte `k` and what replaces it; `None` for no value or one that is
/// `<redacted>` already.
fn value_at(s: &str, k: usize, quote: Quote) -> Option<(usize, String)> {
    let b = s.as_bytes();
    let bare = |inner: &str| match quote {
        Quote::Double => format!("\"{inner}\""),
        Quote::Escaped => format!("\\\"{inner}\\\""),
        Quote::None | Quote::Single => inner.to_string(),
    };
    if s[k..].starts_with("\\\"") {
        let inner = k + 2;
        let close = s[inner..].find("\\\"").map_or(s.len(), |p| inner + p);
        let end = (close + 2).min(s.len());
        return (s[inner..close] != *REDACTED).then(|| (end, format!("\\\"{REDACTED}\\\"")));
    }
    match *b.get(k)? {
        open @ (b'"' | b'\'') => {
            let mut j = k + 1;
            while j < b.len() && b[j] != open {
                j += if open == b'"' && b[j] == b'\\' { 2 } else { 1 };
            }
            let close = j.min(b.len());
            let end = (close + 1).min(b.len());
            let quote = char::from(open);
            (s.get(k + 1..close) != Some(REDACTED))
                .then(|| (end, format!("{quote}{REDACTED}{quote}")))
        }
        b'{' | b'[' => {
            let (mut depth, mut j, mut in_string) = (0usize, k, false);
            while j < b.len() {
                match b[j] {
                    b'\\' => j += 1,
                    b'"' => in_string = !in_string,
                    b'{' | b'[' if !in_string => depth += 1,
                    b'}' | b']' if !in_string => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            Some(((j + 1).min(b.len()), bare(REDACTED)))
        }
        _ => {
            let stop = |j: usize| {
                b[j].is_ascii_whitespace()
                    || matches!(
                        b[j],
                        b',' | b';' | b'}' | b']' | b')' | b'"' | b'\'' | b'&' | b'<' | b'>'
                    )
                    || (b[j] == b'\\' && b.get(j + 1) == Some(&b'"'))
            };
            let run_end = |from: usize| {
                let mut j = from;
                while j < b.len() && !stop(j) {
                    j += 1;
                }
                j
            };
            let mut end = run_end(k);
            if end == k {
                return None;
            }
            let word = &s[k..end];
            if ["bearer", "basic", "digest", "token"]
                .iter()
                .any(|w| word.eq_ignore_ascii_case(w))
                && b.get(end) == Some(&b' ')
            {
                let next = run_end(end + 1);
                if next > end + 1 {
                    end = next;
                }
            }
            Some((end, bare(REDACTED)))
        }
    }
}

/// Every value under a scrub-word key of `table`, at any depth, replaced.
fn scrub_table(table: &mut toml_edit::Table, n: &mut u32) {
    for (key, item) in table.iter_mut() {
        if holds_scrub_word(key.get()) {
            match item {
                toml_edit::Item::Value(v) => replace_value(v, n),
                toml_edit::Item::None => {}
                _ => {
                    *item = toml_edit::value(REDACTED);
                    *n += 1;
                }
            }
            continue;
        }
        match item {
            toml_edit::Item::Table(t) => scrub_table(t, n),
            toml_edit::Item::ArrayOfTables(a) => {
                for t in a.iter_mut() {
                    scrub_table(t, n);
                }
            }
            toml_edit::Item::Value(v) => scrub_value(v, n),
            toml_edit::Item::None => {}
        }
    }
}

/// [`scrub_table`] inside an inline table or array.
fn scrub_value(value: &mut toml_edit::Value, n: &mut u32) {
    match value {
        toml_edit::Value::InlineTable(t) => {
            for (key, v) in t.iter_mut() {
                if holds_scrub_word(key.get()) {
                    replace_value(v, n);
                } else {
                    scrub_value(v, n);
                }
            }
        }
        toml_edit::Value::Array(a) => {
            for v in a.iter_mut() {
                scrub_value(v, n);
            }
        }
        _ => {}
    }
}

/// `value` becomes `"<redacted>"`, keeping its spacing.
fn replace_value(value: &mut toml_edit::Value, n: &mut u32) {
    if value.as_str() == Some(REDACTED) {
        return;
    }
    let decor = value.decor().clone();
    *value = toml_edit::Value::from(REDACTED);
    *value.decor_mut() = decor;
    *n += 1;
}

/// Each `[[claude_code.profiles]]` `label` (or an inline array's) replaced.
fn scrub_labels(doc: &mut toml_edit::DocumentMut, n: &mut u32) {
    let Some(profiles) = doc
        .get_mut("claude_code")
        .and_then(|c| c.get_mut("profiles"))
    else {
        return;
    };
    match profiles {
        toml_edit::Item::ArrayOfTables(a) => {
            for t in a.iter_mut() {
                if let Some(toml_edit::Item::Value(v)) = t.get_mut("label") {
                    replace_value(v, n);
                }
            }
        }
        toml_edit::Item::Value(toml_edit::Value::Array(a)) => {
            for v in a.iter_mut() {
                if let Some(t) = v.as_inline_table_mut()
                    && let Some(label) = t.get_mut("label")
                {
                    replace_value(label, n);
                }
            }
        }
        _ => {}
    }
}

/// The header of a `[table]` or `[[array]]` line, if `line` is one.
fn header(line: &str) -> Option<String> {
    let t = line.trim();
    t.starts_with('[').then(|| {
        t.trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|p| p.trim().trim_matches('"').trim_matches('\''))
            .collect::<Vec<_>>()
            .join(".")
    })
}

/// A `suite.toml` that doesn't parse, line by line: a `key = value` line whose key holds a
/// scrub word, that sits in a table whose name holds one, or that is a profile's `label`, has
/// its value replaced.
fn scrub_lines(doc: &str, n: &mut u32) -> String {
    let mut out = String::with_capacity(doc.len());
    let (mut scrubbed, mut profiles) = (false, false);
    for line in doc.split_inclusive('\n') {
        if let Some(name) = header(line) {
            scrubbed = holds_scrub_word(&name);
            profiles = name == "claude_code.profiles";
            out.push_str(line);
            continue;
        }
        if let Some(eq) = line.find('=') {
            let key = line[..eq].trim().trim_matches('"').trim_matches('\'');
            let value = line[eq + 1..].trim();
            let already = value.trim_matches('"').trim_matches('\'') == REDACTED;
            if !already
                && !line.trim_start().starts_with('#')
                && (scrubbed || holds_scrub_word(key) || (profiles && key == "label"))
            {
                out.push_str(&line[..=eq]);
                out.push_str(" \"");
                out.push_str(REDACTED);
                out.push('"');
                if line.ends_with('\n') {
                    out.push('\n');
                }
                *n += 1;
                continue;
            }
        }
        out.push_str(line);
    }
    out
}

/// The Claude Code profile labels of the `suite.toml` text `doc` (`[[claude_code.profiles]]`
/// `label`s); read line by line when the TOML doesn't parse.
pub(crate) fn profile_labels(doc: &str) -> Vec<String> {
    if let Ok(parsed) = doc.parse::<toml_edit::DocumentMut>() {
        let Some(profiles) = parsed.get("claude_code").and_then(|c| c.get("profiles")) else {
            return Vec::new();
        };
        let label = |t: &dyn toml_edit::TableLike| {
            t.get("label")
                .and_then(toml_edit::Item::as_str)
                .map(str::to_string)
        };
        return match profiles {
            toml_edit::Item::ArrayOfTables(a) => a.iter().filter_map(|t| label(t)).collect(),
            toml_edit::Item::Value(toml_edit::Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_inline_table().and_then(|t| label(t)))
                .collect(),
            _ => Vec::new(),
        };
    }
    let mut labels = Vec::new();
    let mut profiles = false;
    for line in doc.lines() {
        if let Some(name) = header(line) {
            profiles = name == "claude_code.profiles";
        } else if profiles
            && let Some((key, value)) = line.split_once('=')
            && key.trim().trim_matches('"') == "label"
        {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                labels.push(value.to_string());
            }
        }
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-121
    fn emails_match_as_the_appendix_g_pattern_does() {
        let r = Redactor::default();
        for (text, want) in [
            ("a@b.test", "<redacted>"),
            ("a@b.test.x", "<redacted>.x"),
            ("x@y@z.test", "x@<redacted>"),
            ("no at-sign here", "no at-sign here"),
            ("a@b", "a@b"),
            ("react@18.3.1", "react@18.3.1"),
            (
                r#"{"m":"line\nme@x.example"}"#,
                r#"{"m":"line\n<redacted>"}"#,
            ),
        ] {
            assert_eq!(r.text(text).0, want, "{text}");
        }
    }

    #[test] // spec: ARCH-121
    fn a_target_needs_its_slash_and_stands_alone() {
        let r = Redactor::default();
        for (text, want) in [
            (r"C:\x\LILITH-dev\logs", r"C:\x\LILITH-dev\logs"),
            ("LILITH/a", "<redacted>"),
            ("MyLILITH/a", "MyLILITH/a"),
            ("LILITH-/a", "LILITH-/a"),
            ("LILITH/", "LILITH/"),
        ] {
            assert_eq!(r.text(text).0, want, "{text}");
        }
    }

    #[test] // spec: ARCH-121
    fn profile_forms_parse_with_and_without_a_drive() {
        assert_eq!(
            ProfileForm::parse(r"C:\Users\Fake"),
            Some(ProfileForm {
                drive: Some(b'c'),
                parts: vec!["Users".into(), "Fake".into()]
            })
        );
        assert_eq!(ProfileForm::parse(r"C:\"), None);
        let r = Redactor {
            profile_dir: Some(r"C:\Users\Fake\Ünï Name".into()),
            ..Redactor::default()
        };
        assert_eq!(
            r.text(r"D:\x and c:/users/fake/üNÏ name/y").0,
            r"D:\x and %USERPROFILE%/y"
        );
    }
}
