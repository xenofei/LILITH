//! The diagnostics bundle's redaction (ARCH-121, QA-084, AI-092; Appendix G "Diagnostics").
//!
//! [`Compiled::text`] runs these rules over every entry, in order, and then again until
//! nothing changes, at most [`MAX_PASSES`] times, so a value encoded inside another encoding
//! goes too; each replacement counts once:
//!
//! 0. **Terminal control sequences and decoding.** First every terminal control sequence is
//!    removed: CSI (`ESC [`, parameters, a final byte), OSC (`ESC ]` to a BEL or `ESC \`) and
//!    any other `ESC` with its intermediate and final bytes, the `ESC` raw or written `\u001b`,
//!    `\x1b` or `\u{1b}` after any run of `\`; a run of them between two word characters leaves
//!    a space. Then one level of encoding is decoded: `%5C`, `%2F`, `%3A`, `%40`, `%20`, `%2B`,
//!    `%3D` and `%25`, percent-encoded UTF-8, and the `\uXXXX` (after any run of `\`), `&#NN;`
//!    and `&#xHH;` forms of those characters and of non-ASCII ones (a backslash stays escaped
//!    as `\`), so an encoded path, name or address is seen.
//! 1. **Names**: every Claude Code profile label and profile folder (`config_dir`) of
//!    `suite.toml`, and any other name given, as written or JSON-escaped: `<redacted>`.
//! 2. **The user-profile folder**: `%USERPROFILE%`, in its long form and its 8.3 short form,
//!    in any letter case, with any run of `\` or `/` as a separator (so escaped once or twice),
//!    with or without a `\?\` prefix (owner decision 2026-10-22).
//! 3. **SIDs** (`S-1-<n>-<n>…`, any case), whatever follows them: `<sid>`.
//! 4. **`<scheme>:open/<payload>`** under a suite scheme (also `lil://open/`, and with `/`
//!    escaped), whether the payload decodes or not: `open/<redacted>`.
//! 5. **Deny URL tokens** (`deny/<confirmation id>/<token>`, each `/` escaped or not): the
//!    token.
//! 6. **Well-known secret formats**, with no key before them: GitHub tokens (`gh[pousr]_…`,
//!    `github_pat_…`), Hugging Face (`hf_…`), Anthropic and OpenAI keys (`sk-ant-…`, `sk-…`),
//!    age secret keys (`AGE-SECRET-KEY-1…`), Slack (`xox[abpr]-…`), AWS key ids (`AKIA…`) and
//!    JWT-shaped tokens (`eyJ….….…`).
//! 7. **Credential Manager targets**: [`names::APP_NAME`] or [`names::LEGACY_APP_NAME`], an
//!    optional `-<channel>`, then `/<name>`, in any letter case; a quoted target to its quote.
//! 8. **E-mail addresses** (`[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}`).
//! 9. **The account name** (the profile folder's last part, 4 characters or more): `<user>`.
//! 10. **The computer name** (its NetBIOS and DNS names, and their machine-id spellings):
//!     `<machine>`.
//! 11. **Values under scrub-word keys** ([`SCRUB_WORDS`], upper-cased). A key is a run of
//!     letters, digits, `_`, `-` and `.`, optionally quoted (with any escaping), followed by
//!     `=` or `:` (never `::`, a Rust path, and never a `.rs` file's line number). Its value is
//!     the quoted string (with any escaping), a TOML multi-line string (`'''…'''` or
//!     `"""…"""`, escaped any number of times) to its closer or, never closed, to the end of
//!     its line, the bracketed JSON, a `Debug` call such as `Some("…")` or `Secret("…")` to its
//!     closing parenthesis, or the word after it (two words after `Bearer`, `Basic`, `Digest`
//!     or `Token`). A flag holding a scrub word (`--api-key`) takes the next argument as its
//!     value, space-separated or the next element of an array.
//!
//! The known literals of rules 1, 2, 9 and 10 match in any letter case, a run of `\` or `/`
//! matching any such run; one of [`ANYWHERE_CHARS`] characters or more goes wherever it is,
//! inside a word too, and a shorter one only as a whole word. No literal is matched inside a
//! placeholder. Whether a value starts a word is one test for every rule ([`starts_word`]),
//! which counts an escape right before it as a boundary.
//!
//! In `suite.toml` the TOML is read first ([`Redactor::toml`]): every value under a scrub-word
//! key or table, and each `[[claude_code.profiles]]` `label`; then the rules above over the
//! whole file, comments included. A `suite.toml` that doesn't parse is redacted line by line,
//! multi-line values included.

use std::borrow::Cow;
use std::path::Path;

use lil_platform::names;

/// How many decode-and-redact passes [`Redactor::text`] makes at most.
pub const MAX_PASSES: usize = 3;
/// A known literal this long or longer (in characters) goes wherever it is, inside a word too;
/// a shorter one only as a whole word.
pub const ANYWHERE_CHARS: usize = 6;
/// The longest OSC body (`ESC ]` … BEL or `ESC \`) that is looked through for its end.
const MAX_OSC_BYTES: usize = 4096;
/// What a redacted value becomes.
pub const REDACTED: &str = "<redacted>";
/// What the user-profile folder becomes.
pub const PROFILE: &str = "%USERPROFILE%";
/// What the computer name becomes.
pub const MACHINE: &str = "<machine>";
/// What a Windows SID becomes.
pub const SID: &str = "<sid>";
/// What the account name becomes.
pub const USER: &str = "<user>";
/// The placeholders, which no literal is ever matched inside.
const PLACEHOLDERS: [&str; 5] = [PROFILE, REDACTED, USER, MACHINE, SID];
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

/// What the bundle redacts beyond the fixed rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redactor {
    /// The Claude Code profile labels of `suite.toml` (`[[claude_code.profiles]] {id, label,
    /// config_dir}`, AI-088).
    pub labels: Vec<String>,
    /// The user-profile folder.
    pub profile_dir: Option<String>,
    /// The computer name (`COMPUTERNAME`).
    pub machine: Option<String>,
    /// Other names of the computer (its DNS host name).
    pub machine_names: Vec<String>,
    /// Other names redacted as the labels are (each profile's `config_dir`).
    pub names: Vec<String>,
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
        self.compile_with(|long| {
            lil_platform::fs::short_name(Path::new(long))
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
    }

    /// [`Redactor::compile`] with the 8.3 short name of the profile folder from `short`.
    pub(crate) fn compile_with(&self, short: impl Fn(&str) -> Option<String>) -> Compiled {
        let mut profiles: Vec<ProfileForm> = Vec::new();
        let mut user = None;
        if let Some(long) = self.profile_dir.as_deref().filter(|p| !p.trim().is_empty()) {
            let long = long.trim_start_matches(r"\\?\");
            profiles.extend(ProfileForm::parse(long));
            if let Some(form) = short(long).as_deref().and_then(ProfileForm::parse)
                && !profiles.contains(&form)
            {
                profiles.push(form);
            }
            user = profiles
                .first()
                .and_then(|f| f.parts.last())
                .filter(|p| p.chars().count() >= 4)
                .cloned();
        }
        // The more specific form first.
        profiles.sort_by_key(|f| std::cmp::Reverse(f.len()));
        let mut words: Vec<String> = Vec::new();
        for name in self.labels.iter().chain(&self.names) {
            if name.trim().is_empty() {
                continue;
            }
            let escaped = serde_json::to_string(name).unwrap_or_default();
            let escaped = escaped
                .strip_prefix('"')
                .and_then(|e| e.strip_suffix('"'))
                .unwrap_or_default()
                .to_string();
            for form in [name.clone(), escaped] {
                if !form.is_empty() && !words.iter().any(|w| same_text(w, &form)) {
                    words.push(form);
                }
            }
        }
        words.sort_by_key(|w| std::cmp::Reverse(w.len()));
        let mut machines: Vec<String> = Vec::new();
        for name in self
            .machine
            .iter()
            .chain(&self.machine_names)
            .filter(|m| !m.is_empty())
        {
            for form in [name.to_lowercase(), lil_platform::machine_id_from(name)] {
                if !machines.contains(&form) {
                    machines.push(form);
                }
            }
        }
        machines.sort_by_key(|m| std::cmp::Reverse(m.len()));
        Compiled {
            profiles,
            words,
            user,
            machines,
        }
    }
}

/// Whether `a` and `b` are the same text in any letter case.
fn same_text(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

/// A [`Redactor`] with its forms worked out, for redacting many lines.
#[derive(Debug, Clone)]
pub(crate) struct Compiled {
    profiles: Vec<ProfileForm>,
    /// The labels and other names, as written and JSON-escaped, longest first.
    words: Vec<String>,
    /// The account name (the profile folder's last part).
    user: Option<String>,
    machines: Vec<String>,
}

impl Compiled {
    /// [`Redactor::text`] (module docs): the decode-and-redact pass, again until nothing
    /// changes, at most [`MAX_PASSES`] times, so an encoding nested in another (each pass
    /// decodes one level) is redacted too.
    pub(crate) fn text(&self, s: &str) -> (String, u32) {
        let mut n = 0;
        let mut current = self.pass(s, &mut n);
        for _ in 1..MAX_PASSES {
            let next = self.pass(&current, &mut n);
            if next == current {
                break;
            }
            current = next;
        }
        (current, n)
    }

    /// One decode-and-redact pass over `s` (module docs).
    fn pass(&self, s: &str, n: &mut u32) -> String {
        let s = strip_terminal(s);
        let s = decode_escapes(&s);
        let mut s = s.into_owned();
        for word in &self.words {
            s = replace_literal(&s, word, REDACTED, n);
        }
        let s = self.profile(&s, n);
        let s = sids(&s, n);
        let s = open_payloads(&s, n);
        let s = deny_tokens(&s, n);
        let s = known_tokens(&s, n);
        let s = targets(&s, n);
        let mut s = emails(&s, n);
        if let Some(user) = &self.user {
            s = replace_literal(&s, user, USER, n);
        }
        for form in &self.machines {
            s = replace_literal(&s, form, MACHINE, n);
        }
        keys(&s, n)
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
}

/// An `ESC` at byte `i` of `b`: the raw byte, or `\u001b`, `\x1b` or `\u{1b}` after any run of
/// `\` (a JSON string, or one inside another). Its end, how many of the run's backslashes
/// stay (the escapes of other levels, as [`decode_escapes`] keeps them), and whether it was
/// the raw byte.
fn esc_at(b: &[u8], i: usize) -> Option<(usize, usize, bool)> {
    match *b.get(i)? {
        0x1b => return Some((i + 1, 0, true)),
        b'\\' => {}
        _ => return None,
    }
    let run = b[i..].iter().take_while(|c| **c == b'\\').count();
    let rest = &b[i + run..];
    let body = if rest.len() >= 5 && rest[0] == b'u' && rest[1..5].eq_ignore_ascii_case(b"001b") {
        5
    } else if rest.len() >= 3 && rest[0] == b'x' && rest[1..3].eq_ignore_ascii_case(b"1b") {
        3
    } else if rest.starts_with(b"u{") {
        let digits = rest[2..]
            .iter()
            .take_while(|c| c.is_ascii_hexdigit())
            .count();
        let hex = std::str::from_utf8(&rest[2..2 + digits]).unwrap_or_default();
        (digits > 0
            && rest.get(2 + digits) == Some(&b'}')
            && u32::from_str_radix(hex, 16) == Ok(0x1b))
        .then_some(3 + digits)?
    } else {
        return None;
    };
    let keep = if run % 2 == 1 { run - 1 } else { run - 2 };
    Some((i + run + body, keep, false))
}

/// The end of the string terminator `ESC \` at byte `j`, if one is there: after a raw `ESC`
/// one `\`, after a written one a `\` written as `\\` (any even run).
fn st_at(b: &[u8], j: usize) -> Option<usize> {
    let (end, _, raw) = esc_at(b, j)?;
    let run = b[end..].iter().take_while(|c| **c == b'\\').count();
    let take = if raw { run.min(1) } else { run - run % 2 };
    (take > 0).then_some(end + take)
}

/// The end of a BEL at byte `j`: the raw byte, or `\u0007`, `\x07` or `\a` after a run of `\`.
fn bel_at(b: &[u8], j: usize) -> Option<usize> {
    if b.get(j) == Some(&0x07) {
        return Some(j + 1);
    }
    let run = b.get(j..)?.iter().take_while(|c| **c == b'\\').count();
    let rest = &b[j + run..];
    if run == 0 {
        None
    } else if rest.starts_with(b"u0007") {
        Some(j + run + 5)
    } else if rest.starts_with(b"x07") {
        Some(j + run + 3)
    } else if rest.starts_with(b"a") {
        Some(j + run + 1)
    } else {
        None
    }
}

/// The end of the terminal control sequence whose `ESC` ([`esc_at`]) ends at `p`: a lone
/// `ESC \`, CSI `[` (parameters, intermediates, a final byte), OSC `]` to its BEL or `ESC \`,
/// or another `ESC` with its intermediates and final byte; just the `ESC` when what follows is
/// none of these. A `"` or a `\` never belongs to one, so JSON stays JSON.
fn sequence_end(b: &[u8], start: usize, p: usize) -> usize {
    let at = |j: usize| b.get(j).copied();
    let in_range = |c: Option<u8>, lo: u8, hi: u8| c.is_some_and(|c| (lo..=hi).contains(&c));
    if let Some(end) = st_at(b, start) {
        return end;
    }
    match at(p) {
        Some(b'[') => {
            let mut j = p + 1;
            while in_range(at(j), 0x30, 0x3f) {
                j += 1;
            }
            while in_range(at(j), 0x20, 0x2f) && at(j) != Some(b'"') {
                j += 1;
            }
            if in_range(at(j), 0x40, 0x7e) && at(j) != Some(b'\\') {
                j + 1
            } else {
                p + 1
            }
        }
        Some(b']') => {
            let mut j = p + 1;
            while j < b.len() && j - p <= MAX_OSC_BYTES && !matches!(b[j], b'"' | b'\n') {
                if let Some(end) = bel_at(b, j).or_else(|| st_at(b, j)) {
                    return end;
                }
                j += 1;
            }
            p + 1
        }
        _ => {
            let mut j = p;
            while in_range(at(j), 0x20, 0x2f) && at(j) != Some(b'"') {
                j += 1;
            }
            if in_range(at(j), 0x30, 0x7e) && at(j) != Some(b'\\') {
                j + 1
            } else {
                p
            }
        }
    }
}

/// `s` with every terminal control sequence removed (module docs, rule 0); borrowed when it
/// has none. A run of them between two word characters leaves a space, so a value right after
/// one still starts a word.
fn strip_terminal(s: &str) -> Cow<'_, str> {
    if !s.contains('\u{1b}') && !s.contains("1b") && !s.contains("1B") {
        return Cow::Borrowed(s);
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i < b.len() {
        let Some((p, keep, _)) = esc_at(b, i) else {
            i += 1;
            continue;
        };
        out.push_str(&s[last..i + keep]);
        let mut end = sequence_end(b, i, p);
        // The sequences right after it go with it.
        while let Some((p, 0, _)) = esc_at(b, end) {
            end = sequence_end(b, end, p);
        }
        let joins = |c: Option<char>| c.is_some_and(word_char);
        if joins(out.chars().next_back()) && joins(s[end..].chars().next()) {
            out.push(' ');
        }
        last = end;
        i = end;
    }
    out.push_str(&s[last..]);
    Cow::Owned(out)
}

/// `s` with one level of the encodings of rule 0 decoded (module docs); borrowed when there are
/// none. `%25` decodes to `%`, so a twice percent-encoded form is decoded by the next pass; a
/// run of percent-encoded bytes that is UTF-8 decodes to its characters; a `\uXXXX` escape
/// after any run of `\` (a JSON string inside a JSON string) decodes, the run losing the
/// backslashes of that escape level; and `&#NN;` / `&#xHH;` decode as the `\u` forms do.
fn decode_escapes(s: &str) -> Cow<'_, str> {
    if !s.contains('%') && !s.contains("\\u") && !s.contains("&#") {
        return Cow::Borrowed(s);
    }
    let b = s.as_bytes();
    let hex = |range: std::ops::Range<usize>| {
        b.get(range.clone())
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
            .and_then(|_| u32::from_str_radix(&s[range], 16).ok())
    };
    // What a code point decodes to, if it is one rule 0 decodes.
    let decoded = |code: u32| match code {
        0x5c => Some(r"\\".to_string()),
        0x2f | 0x3a | 0x40 | 0x20 | 0x2b | 0x3d => char::from_u32(code).map(String::from),
        0xa0.. => char::from_u32(code).map(String::from),
        _ => None,
    };
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i < b.len() {
        if b[i] == b'%'
            && let Some(code) = hex(i + 1..i + 3)
        {
            if code == 0x25 {
                out.push_str(&s[last..i]);
                out.push('%');
                i += 3;
                last = i;
                continue;
            }
            if code >= 0x80 {
                // A run of percent-encoded bytes: decoded when it is UTF-8.
                let mut bytes = Vec::new();
                let mut j = i;
                while b.get(j) == Some(&b'%')
                    && let Some(byte) = hex(j + 1..j + 3).filter(|c| *c >= 0x80)
                {
                    bytes.push(byte as u8);
                    j += 3;
                }
                if let Ok(text) = String::from_utf8(bytes) {
                    out.push_str(&s[last..i]);
                    out.push_str(&text);
                    i = j;
                    last = i;
                    continue;
                }
            } else if let Some(text) = decoded(code) {
                // `%5C` becomes an escaped backslash, so JSON stays JSON.
                out.push_str(&s[last..i]);
                out.push_str(&text);
                i += 3;
                last = i;
                continue;
            }
        }
        if b[i] == b'\\' {
            let run = b[i..].iter().take_while(|c| **c == b'\\').count();
            let u = i + run;
            if b.get(u) == Some(&b'u')
                && let Some(text) = hex(u + 1..u + 5).and_then(decoded)
            {
                // The escape's own backslashes go: one at its level, the others pairs.
                let keep = if run % 2 == 1 { run - 1 } else { run - 2 };
                out.push_str(&s[last..i]);
                out.push_str(&s[i..i + keep]);
                out.push_str(&text);
                i = u + 5;
                last = i;
                continue;
            }
            i += run;
            continue;
        }
        if b[i] == b'&' && b.get(i + 1) == Some(&b'#') {
            let (digits, radix, from) = match b.get(i + 2) {
                Some(b'x' | b'X') => (
                    b[i + 3..]
                        .iter()
                        .take_while(|c| c.is_ascii_hexdigit())
                        .count(),
                    16,
                    i + 3,
                ),
                _ => (
                    b[i + 2..].iter().take_while(|c| c.is_ascii_digit()).count(),
                    10,
                    i + 2,
                ),
            };
            if (1..=6).contains(&digits)
                && b.get(from + digits) == Some(&b';')
                && let Some(text) = u32::from_str_radix(&s[from..from + digits], radix)
                    .ok()
                    .and_then(decoded)
            {
                out.push_str(&s[last..i]);
                out.push_str(&text);
                i = from + digits + 1;
                last = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&s[last..]);
    Cow::Owned(out)
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
    /// case; and, for a form shorter than [`ANYWHERE_CHARS`], then a place where a name can't
    /// go on ([`name_ends_at`]). A separator is any run of `\` or `/`, so a path escaped once
    /// (JSON) or twice (`Debug` text inside a JSON line) matches too, and so does the prefix in
    /// any of those spellings.
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
        (self.len() >= ANYWHERE_CHARS || name_ends_at(s, j)).then_some(j)
    }
}

/// Whether a folder or file name can't go on at byte `j` of `s`: the end, a separator, or a
/// character that isn't a letter, a digit or `_` (a `.`, `-` or `~` only when no letter or
/// digit follows it): the profile folder's name followed by `2` is another folder.
fn name_ends_at(s: &str, j: usize) -> bool {
    let mut rest = s[j..].chars();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    match rest.next() {
        None => true,
        Some(c) if word(c) => false,
        Some('.' | '-' | '~') => !rest.next().is_some_and(word),
        Some(_) => true,
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

/// A letter, a digit or `_`: what a whole word can't touch.
fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether a value may start at byte `i` of `s`, the one "not inside a word" test every rule
/// uses: at the text's start, after a character that isn't a word character (as `word`
/// says), or right after an escape: `\n`, `\r`, `\t`, `\b`, `\f`, `\v`, `\a`, `\e` or `\0`,
/// `\xXX`, `\uXXXX` or `\UXXXXXXXX` (after any run of `\`), or `%XX`, decoded or not. An
/// escape is a boundary, so a value at the start of an escaped line is found too. (`\"`,
/// `\\`, `\/`, `\u{…}`, `&#NN;` and `&#xHH;` end in a character that is no word character
/// anyway, and a terminal control sequence is stripped first, leaving a space between two
/// word characters.)
fn starts_word(s: &str, i: usize, word: impl Fn(char) -> bool) -> bool {
    if !s.is_char_boundary(i) {
        return false;
    }
    let Some(prev) = s[..i].chars().next_back() else {
        return true;
    };
    !word(prev) || after_escape(s.as_bytes(), i)
}

/// Whether byte `i` of `b` comes right after one of [`starts_word`]'s escapes.
fn after_escape(b: &[u8], i: usize) -> bool {
    let after_backslash = |at: usize| at > 0 && b[at - 1] == b'\\';
    // `letter` then `digits` hex digits, ending at `i`, after a `\`.
    let hex_escape = |letter: u8, digits: usize| {
        i > digits
            && b[i - digits - 1] == letter
            && b[i - digits..i].iter().all(u8::is_ascii_hexdigit)
            && after_backslash(i - digits - 1)
    };
    (i >= 2
        && matches!(
            b[i - 1],
            b'n' | b'r' | b't' | b'b' | b'f' | b'v' | b'a' | b'e' | b'0'
        )
        && after_backslash(i - 1))
        || hex_escape(b'x', 2)
        || hex_escape(b'u', 4)
        || hex_escape(b'U', 8)
        || (i >= 3 && b[i - 3] == b'%' && b[i - 2..i].iter().all(u8::is_ascii_hexdigit))
}

/// The end of the known literal `literal` in `s` from byte `j`, compared after normalising: in
/// any letter case, and with a run of `\` or `/` in it matching any run of `\` or `/`.
fn match_literal(s: &str, j: usize, literal: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut j = j;
    let mut want = literal.chars().peekable();
    while let Some(c) = want.next() {
        if matches!(c, '\\' | '/') {
            while want.next_if(|c| matches!(c, '\\' | '/')).is_some() {}
            j += separator_at(b, j)?;
            continue;
        }
        let got = s.get(j..)?.chars().next()?;
        if !got.to_lowercase().eq(c.to_lowercase()) {
            return None;
        }
        j += got.len_utf8();
    }
    Some(j)
}

/// Every known literal `literal` ([`match_literal`]) replaced by `with`, never inside a
/// placeholder: wherever it is when it is [`ANYWHERE_CHARS`] characters or more, else as a
/// whole word (where it starts or ends with a letter, digit or `_`, it starts a word,
/// [`starts_word`], and the character after it isn't one).
fn replace_literal(s: &str, literal: &str, with: &str, n: &mut u32) -> String {
    let (Some(first), Some(final_char)) = (literal.chars().next(), literal.chars().next_back())
    else {
        return s.to_string();
    };
    let anywhere = literal.chars().count() >= ANYWHERE_CHARS;
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i < s.len() {
        if matches!(b[i], b'%' | b'<')
            && let Some(p) = PLACEHOLDERS.iter().find(|p| s[i..].starts_with(**p))
        {
            i += p.len();
            continue;
        }
        let c = s[i..].chars().next().unwrap_or_default();
        let first_ok = if matches!(first, '\\' | '/') {
            matches!(c, '\\' | '/')
        } else {
            c.to_lowercase().eq(first.to_lowercase())
        };
        if first_ok
            && (anywhere || !word_char(first) || starts_word(s, i, word_char))
            && let Some(end) = match_literal(s, i, literal)
            && (anywhere
                || !word_char(final_char)
                || !s[end..].chars().next().is_some_and(word_char))
        {
            out.push_str(&s[last..i]);
            out.push_str(with);
            *n += 1;
            last = end;
            i = end;
            continue;
        }
        i += c.len_utf8();
    }
    out.push_str(&s[last..]);
    out
}

/// Whether `key` holds a scrub word (upper-cased).
fn holds_scrub_word(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SCRUB_WORDS.iter().any(|w| upper.contains(w))
}

/// Every Windows SID (`S-1-<n>-<n>…`, any case, starting a word) replaced by `<sid>`, whatever
/// follows it; it ends at the first `-` not followed by a digit (`-dev`, `-test-…` stay).
fn sids(s: &str, n: &mut u32) -> String {
    let b = s.as_bytes();
    let digits = |from: usize| {
        b[from.min(b.len())..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count()
    };
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i + 4 <= b.len() {
        let starts = matches!(b[i], b'S' | b's')
            && b[i + 1] == b'-'
            && b[i + 2] == b'1'
            && starts_word(s, i, |c| c.is_ascii_alphanumeric());
        if starts {
            let mut j = i + 3;
            let mut groups = 0;
            while b.get(j) == Some(&b'-') {
                let d = digits(j + 1);
                if d == 0 {
                    break;
                }
                j += 1 + d;
                groups += 1;
            }
            if groups >= 2 {
                out.push_str(&s[last..i]);
                out.push_str(SID);
                *n += 1;
                last = j;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&s[last..]);
    out
}

/// Every `open/` payload under a suite scheme (`lil`, `lil-dev`, `lil-test-…`, any case,
/// starting a word) replaced, whether it decodes or not: it is a path, a ref or a name
/// (APP-020). The scheme may be followed by `//` (`lil://open/…`), and each `/` may be escaped
/// (`open\/…`); the payload runs over `[A-Za-z0-9+/=_-]`, standard base64 included.
fn open_payloads(s: &str, n: &mut u32) -> String {
    let b = s.as_bytes();
    let scheme_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    // A `/`, escaped with any run of `\` or not: its length.
    let slash = |at: usize| {
        let run = b[at.min(b.len())..]
            .iter()
            .take_while(|c| **c == b'\\')
            .count();
        (b.get(at + run) == Some(&b'/')).then_some(run + 1)
    };
    let payload_byte =
        |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'=' | b'_' | b'-');
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i + 3 <= b.len() {
        if !(b[i..i + 3].eq_ignore_ascii_case(b"lil") && starts_word(s, i, scheme_char)) {
            i += 1;
            continue;
        }
        let mut j = i + 3;
        if b.get(j) == Some(&b'-') {
            j += 1;
            while b
                .get(j)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-' || *c == b'_')
            {
                j += 1;
            }
        }
        if b.get(j) != Some(&b':') {
            i += 1;
            continue;
        }
        j += 1;
        // `lil://open/…`
        for _ in 0..2 {
            if let Some(len) = slash(j) {
                j += len;
            }
        }
        let verb = b
            .get(j..j + 4)
            .is_some_and(|v| v.eq_ignore_ascii_case(b"open"));
        let Some(sep) = verb.then(|| slash(j + 4)).flatten() else {
            i += 1;
            continue;
        };
        let from = j + 4 + sep;
        let mut end = from;
        loop {
            if end < b.len() && payload_byte(b[end]) {
                end += 1;
            } else if let Some(len) = slash(end).filter(|_| b.get(end) == Some(&b'\\')) {
                // A standard-base64 `/` escaped in JSON.
                end += len;
            } else {
                break;
            }
        }
        if end == from {
            i = from;
            continue;
        }
        out.push_str(&s[last..from]);
        out.push_str(REDACTED);
        *n += 1;
        last = end;
        i = end;
    }
    out.push_str(&s[last..]);
    out
}

/// `deny/cf_<16 hex>/<32 hex>` with its token replaced; each `/` may be escaped (`\/`, with any
/// run of `\`).
fn deny_tokens(s: &str, n: &mut u32) -> String {
    let b = s.as_bytes();
    let hex = |range: std::ops::Range<usize>| {
        b.get(range)
            .is_some_and(|r| r.iter().all(u8::is_ascii_hexdigit))
    };
    // A `/`, escaped with any run of `\` or not: its length.
    let slash = |at: usize| {
        let run = b[at.min(b.len())..]
            .iter()
            .take_while(|c| **c == b'\\')
            .count();
        (b.get(at + run) == Some(&b'/')).then_some(run + 1)
    };
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i + 4 <= b.len() {
        let id = b[i..i + 4]
            .eq_ignore_ascii_case(b"deny")
            .then(|| slash(i + 4))
            .flatten()
            .map(|len| i + 4 + len)
            .filter(|cf| {
                b.get(*cf..cf + 3)
                    .is_some_and(|h| h.eq_ignore_ascii_case(b"cf_"))
            })
            .map(|cf| cf + 3);
        if let Some(id) = id
            && hex(id..id + 16)
            && let Some(len) = slash(id + 16)
        {
            let token = id + 16 + len;
            if hex(token..token + 32) {
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

/// A well-known secret format: its prefix, the bytes its body may hold, and the body's
/// shortest length.
struct TokenFormat {
    prefix: &'static str,
    body: fn(u8) -> bool,
    min: usize,
}

fn alnum(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}

fn alnum_under(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn alnum_dash_under(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_'
}

fn upper_digit(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}

/// Rule 6's formats (module docs); the longer prefix of two that overlap first.
const TOKEN_FORMATS: &[TokenFormat] = &[
    TokenFormat {
        prefix: "github_pat_",
        body: alnum_under,
        min: 20,
    },
    TokenFormat {
        prefix: "ghp_",
        body: alnum,
        min: 20,
    },
    TokenFormat {
        prefix: "gho_",
        body: alnum,
        min: 20,
    },
    TokenFormat {
        prefix: "ghu_",
        body: alnum,
        min: 20,
    },
    TokenFormat {
        prefix: "ghs_",
        body: alnum,
        min: 20,
    },
    TokenFormat {
        prefix: "ghr_",
        body: alnum,
        min: 20,
    },
    TokenFormat {
        prefix: "hf_",
        body: alnum,
        min: 20,
    },
    TokenFormat {
        prefix: "sk-ant-",
        body: alnum_dash_under,
        min: 16,
    },
    TokenFormat {
        prefix: "sk-",
        body: alnum_dash_under,
        min: 16,
    },
    TokenFormat {
        prefix: "AGE-SECRET-KEY-1",
        body: upper_digit,
        min: 50,
    },
    TokenFormat {
        prefix: "xoxa-",
        body: alnum_dash_under,
        min: 10,
    },
    TokenFormat {
        prefix: "xoxb-",
        body: alnum_dash_under,
        min: 10,
    },
    TokenFormat {
        prefix: "xoxp-",
        body: alnum_dash_under,
        min: 10,
    },
    TokenFormat {
        prefix: "xoxr-",
        body: alnum_dash_under,
        min: 10,
    },
    TokenFormat {
        prefix: "AKIA",
        body: upper_digit,
        min: 16,
    },
];

/// Every well-known secret (rule 6) replaced, wherever it is.
fn known_tokens(s: &str, n: &mut u32) -> String {
    let b = s.as_bytes();
    let run = |from: usize, ok: fn(u8) -> bool| {
        b[from.min(b.len())..]
            .iter()
            .take_while(|c| ok(**c))
            .count()
    };
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i < b.len() {
        let could_start = matches!(b[i], b'g' | b'h' | b's' | b'A' | b'x' | b'e');
        if !could_start || !starts_word(s, i, |c| c.is_ascii_alphanumeric() || c == '_') {
            i += 1;
            continue;
        }
        let mut end = TOKEN_FORMATS.iter().find_map(|f| {
            let p = f.prefix.as_bytes();
            let body = i + p.len();
            let len = run(body, f.body);
            (b.get(i..body) == Some(p) && len >= f.min).then_some(body + len)
        });
        // A JWT: three base64url parts, the first a JSON header (`eyJ`).
        if end.is_none() && b[i..].starts_with(b"eyJ") {
            let one = run(i, alnum_dash_under);
            let two = (b.get(i + one) == Some(&b'.')).then(|| run(i + one + 1, alnum_dash_under));
            let three = two.filter(|t| *t >= 8).and_then(|t| {
                let at = i + one + 1 + t;
                (b.get(at) == Some(&b'.')).then(|| (at, run(at + 1, alnum_dash_under)))
            });
            if one >= 10
                && let Some((at, len)) = three
                && len >= 8
            {
                end = Some(at + 1 + len);
            }
        }
        match end {
            Some(end) => {
                out.push_str(&s[last..i]);
                out.push_str(REDACTED);
                *n += 1;
                last = end;
                i = end;
            }
            None => i += 1,
        }
    }
    out.push_str(&s[last..]);
    out
}

/// Every Credential Manager target replaced: the suite's name or its name before the rename
/// (any case, not inside a longer word), an optional `-<channel>`, `/`, then the name up to a
/// space, quote, backslash or closing bracket, or, for a quoted target, to its quote.
fn targets(s: &str, n: &mut u32) -> String {
    let b = s.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-';
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i < b.len() {
        let could_start = matches!(b[i], b'L' | b'l' | b'W' | b'w');
        if !could_start || !starts_word(s, i, |c| c.is_ascii() && word(c as u8)) {
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
        let quote = i
            .checked_sub(1)
            .map(|q| b[q])
            .filter(|q| matches!(q, b'"' | b'\''));
        let mut end = name;
        match quote {
            Some(q) => {
                while b.get(end).is_some_and(|c| *c != q && *c != b'\n') {
                    end += 1;
                }
                // An escaped quote (`\"`) ends it too: its backslashes stay.
                if b.get(end) == Some(&q) {
                    while end > name && b[end - 1] == b'\\' {
                        end -= 1;
                    }
                }
            }
            None => {
                while b.get(end).is_some_and(|c| {
                    !(c.is_ascii_whitespace()
                        || matches!(
                            c,
                            b'"' | b'\''
                                | b'\\'
                                | b','
                                | b';'
                                | b')'
                                | b']'
                                | b'}'
                                | b'<'
                                | b'>'
                                | b'`'
                        ))
                }) {
                    end += 1;
                }
            }
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
    /// `\"key\"` inside a JSON string, with this many backslashes (`\\\"` twice escaped).
    Escaped(usize),
}

/// The odd run of backslashes followed by `"` at `k`, if one is there: its length.
fn escaped_quote_at(b: &[u8], k: usize) -> Option<usize> {
    let run = b.get(k..)?.iter().take_while(|c| **c == b'\\').count();
    (run % 2 == 1 && b.get(k + run) == Some(&b'"')).then_some(run)
}

/// Every value under a scrub-word key replaced (rule 11, module docs).
fn keys(s: &str, n: &mut u32) -> String {
    let b = s.as_bytes();
    let key_byte = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.');
    let spaces = |mut k: usize| {
        while matches!(b.get(k), Some(b' ' | b'\t')) {
            k += 1;
        }
        k
    };
    let mut out = String::with_capacity(s.len());
    let (mut last, mut i) = (0, 0);
    while i < b.len() {
        if !key_byte(b[i]) || !starts_word(s, i, |c| c.is_ascii() && key_byte(c as u8)) {
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
        let quote = if let Some(run) = escaped_quote_at(b, k) {
            k += run + 1;
            Quote::Escaped(run)
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
        let after_key = k;
        k = spaces(k);
        let separator = match b.get(k) {
            Some(b'=') => true,
            Some(b':') => b.get(k + 1) != Some(&b':') && (k == 0 || b[k - 1] != b':'),
            _ => false,
        };
        let value = if separator {
            value_at(s, spaces(k + 1), quote)
        } else if key.starts_with('-') {
            flag_value(s, after_key, quote)
        } else {
            None
        };
        let Some((value_start, value_end, with)) = value else {
            continue;
        };
        out.push_str(&s[last..value_start]);
        out.push_str(&with);
        *n += 1;
        last = value_end;
        i = value_end;
    }
    out.push_str(&s[last..]);
    out
}

/// The argument after a scrub-word flag (`--api-key`) that ends at `k`: the next element of an
/// array (`"--token", "…"`) or the next space-separated word. Its start, its end and what
/// replaces it.
fn flag_value(s: &str, k: usize, quote: Quote) -> Option<(usize, usize, String)> {
    let b = s.as_bytes();
    let mut j = k;
    while matches!(b.get(j), Some(b' ' | b'\t')) {
        j += 1;
    }
    if quote != Quote::None {
        // In an array: a comma, then the next quoted element.
        if b.get(j) != Some(&b',') {
            return None;
        }
        j += 1;
        while matches!(b.get(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        let quoted = matches!(b.get(j), Some(b'"' | b'\'')) || escaped_quote_at(b, j).is_some();
        return quoted.then(|| value_at(s, j, quote)).flatten();
    }
    // Space-separated, and not another flag.
    if j == k || b.get(j).is_none_or(|c| *c == b'-') {
        return None;
    }
    value_at(s, j, Quote::None)
}

/// The value starting at byte `k`: its start, its end and what replaces it; `None` for no
/// value or one that is `<redacted>` already.
fn value_at(s: &str, k: usize, quote: Quote) -> Option<(usize, usize, String)> {
    let b = s.as_bytes();
    let bare = |inner: &str| match quote {
        Quote::Double => format!("\"{inner}\""),
        Quote::Escaped(run) => {
            let slashes = "\\".repeat(run);
            format!("{slashes}\"{inner}{slashes}\"")
        }
        Quote::None | Quote::Single => inner.to_string(),
    };
    // A TOML multi-line string (`'''…'''`, `"""…"""`, or `"""` escaped any number of times):
    // to its closing delimiter, or, never closed, to the end of its line.
    let escaped_triple = escaped_quote_at(b, k).and_then(|run| {
        let one = format!("{}\"", "\\".repeat(run));
        s[k..].starts_with(&one.repeat(3)).then(|| one.repeat(3))
    });
    let triple = ["'''", "\"\"\""]
        .iter()
        .find(|d| s[k..].starts_with(**d))
        .map(|d| (*d).to_string())
        .or(escaped_triple);
    if let Some(delim) = triple {
        let inner = k + delim.len();
        let (close, end) = match s[inner..].find(&delim) {
            Some(p) => (inner + p, inner + p + delim.len()),
            None => {
                let line_end = s[inner..].find('\n').map_or(s.len(), |p| inner + p);
                (line_end, line_end)
            }
        };
        return (s[inner..close] != *REDACTED)
            .then(|| (k, end, format!("{delim}{REDACTED}{delim}")));
    }
    if let Some(run) = escaped_quote_at(b, k) {
        // A string escaped `run` times: to the same run before a `"`.
        let inner = k + run + 1;
        let closing = format!("{}\"", "\\".repeat(run));
        let close = s[inner..].find(&closing).map_or(s.len(), |p| inner + p);
        let end = (close + closing.len()).min(s.len());
        let slashes = "\\".repeat(run);
        return (s[inner..close] != *REDACTED)
            .then(|| (k, end, format!("{slashes}\"{REDACTED}{slashes}\"")));
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
                .then(|| (k, end, format!("{quote}{REDACTED}{quote}")))
        }
        b'{' | b'[' => Some((k, balanced_end(b, k), bare(REDACTED))),
        _ => {
            // `Some("…")`, `Secret("…")`: a `Debug` call, to its closing parenthesis.
            let ident = b[k..]
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_')
                .count();
            if ident > 0 && b.get(k + ident) == Some(&b'(') {
                return Some((k, balanced_end(b, k + ident), bare(REDACTED)));
            }
            let stop = |j: usize| {
                b[j].is_ascii_whitespace()
                    || matches!(
                        b[j],
                        b',' | b';' | b'}' | b']' | b')' | b'"' | b'\'' | b'&' | b'<' | b'>'
                    )
                    || (b[j] == b'\\' && escaped_quote_at(b, j).is_some())
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
            Some((k, end, bare(REDACTED)))
        }
    }
}

/// The end (one past the closing bracket) of the `{…}`, `[…]` or `(…)` opening at `k`, strings
/// and escapes skipped; the text's end when it never closes.
fn balanced_end(b: &[u8], k: usize) -> usize {
    let (mut depth, mut j, mut in_string) = (0usize, k, false);
    while j < b.len() {
        match b[j] {
            b'\\' => j += 1,
            b'"' => in_string = !in_string,
            b'{' | b'[' | b'(' if !in_string => depth += 1,
            b'}' | b']' | b')' if !in_string => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return (j + 1).min(b.len());
                }
            }
            _ => {}
        }
        j += 1;
    }
    b.len()
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

/// Where a multi-line value opened on a `key = value` line ends: the closing `"""` or `'''` of
/// a multi-line string, or the `]` of an array that doesn't close on its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    String(&'static str),
    Array,
}

/// The multi-line value `value` (the text after `=`) opens, if it does.
fn opens(value: &str) -> Option<Open> {
    let v = value.trim();
    for delim in ["\"\"\"", "'''"] {
        if let Some(rest) = v.strip_prefix(delim) {
            return (!rest.contains(delim)).then_some(Open::String(delim));
        }
    }
    let depth = v.bytes().fold(0i32, |d, c| match c {
        b'[' => d + 1,
        b']' => d - 1,
        _ => d,
    });
    (v.starts_with('[') && depth > 0).then_some(Open::Array)
}

/// A `suite.toml` that doesn't parse, line by line: a `key = value` line whose key holds a
/// scrub word, that sits in a table whose name holds one, or that is a profile's `label`, has
/// its value replaced, and the following lines of a multi-line value are left out.
fn scrub_lines(doc: &str, n: &mut u32) -> String {
    let mut out = String::with_capacity(doc.len());
    let (mut scrubbed, mut profiles) = (false, false);
    let mut skipping: Option<Open> = None;
    for line in doc.split_inclusive('\n') {
        if let Some(open) = skipping {
            let closed = match open {
                Open::String(delim) => line.contains(delim),
                Open::Array => line.contains(']'),
            };
            if closed {
                skipping = None;
            }
            continue;
        }
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
                skipping = opens(value);
                continue;
            }
        }
        out.push_str(line);
    }
    out
}

/// Each `[[claude_code.profiles]]` `key` value of the `suite.toml` text `doc` (`label`,
/// `config_dir`); read line by line when the TOML doesn't parse.
pub(crate) fn profile_values(doc: &str, key: &str) -> Vec<String> {
    if let Ok(parsed) = doc.parse::<toml_edit::DocumentMut>() {
        let Some(profiles) = parsed.get("claude_code").and_then(|c| c.get("profiles")) else {
            return Vec::new();
        };
        let value = |t: &dyn toml_edit::TableLike| {
            t.get(key)
                .and_then(toml_edit::Item::as_str)
                .map(str::to_string)
        };
        return match profiles {
            toml_edit::Item::ArrayOfTables(a) => a.iter().filter_map(|t| value(t)).collect(),
            toml_edit::Item::Value(toml_edit::Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_inline_table().and_then(|t| value(t)))
                .collect(),
            _ => Vec::new(),
        };
    }
    let mut values = Vec::new();
    let mut profiles = false;
    for line in doc.lines() {
        if let Some(name) = header(line) {
            profiles = name == "claude_code.profiles";
        } else if profiles
            && let Some((k, value)) = line.split_once('=')
            && k.trim().trim_matches('"') == key
        {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                values.push(value.to_string());
            }
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

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
            (r"probe\u0040mail.example", "<redacted>"),
            ("probe%40mail.example", "<redacted>"),
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
            ("LILITH-dev%2Fgithub", "<redacted>"),
            (
                r#"{"t":"LILITH-dev/my token name"}"#,
                r#"{"t":"<redacted>"}"#,
            ),
            (
                r#"{"m":"saw \"LILITH-dev/a b\" here"}"#,
                r#"{"m":"saw \"<redacted>\" here"}"#,
            ),
        ] {
            assert_eq!(r.text(text).0, want, "{text}");
        }
    }

    /// The profile folder is a literal of 6 characters or more: it goes wherever it is, even
    /// where a longer folder name goes on after it (over-redaction is safe).
    #[test] // spec: ARCH-121
    fn profile_forms_parse_and_match_anywhere() {
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
        for (text, want) in [
            (
                r"D:\x and c:/users/fake/üNÏ name/y",
                r"D:\x and %USERPROFILE%/y",
            ),
            (r"C:\Users\Fake\Ünï Name2\x", r"%USERPROFILE%2\x"),
            (r"C:\Users\Fake\Ünï Name.old", r"%USERPROFILE%.old"),
            (r"in C:\Users\Fake\Ünï Name.", r"in %USERPROFILE%."),
            (r"C%3A%5CUsers%5CFake%5CÜnï%20Name%5Cx", r"%USERPROFILE%\\x"),
            (
                r#"{"p":"C:\\Users\\Fake\\\u00dcn\u00ef Name"}"#,
                r#"{"p":"%USERPROFILE%"}"#,
            ),
        ] {
            assert_eq!(r.text(text).0, want, "{text}");
        }
    }

    /// The 8.3 short form matches wherever it comes from, whether or not this volume makes
    /// 8.3 names.
    #[test] // spec: ARCH-121
    fn the_short_form_matches_as_the_long_one_does() {
        let r = Redactor {
            profile_dir: Some(r"C:\Users\Fake\Long Profile Name".into()),
            ..Redactor::default()
        };
        let compiled = r.compile_with(|_| Some(r"C:\Users\Fake\LONGPR~1".to_string()));
        for text in [r"C:\Users\Fake\LONGPR~1\x", r"c:/users/fake/longpr~1/x"] {
            let (out, n) = compiled.text(text);
            assert_eq!(n, 1, "{text} -> {out}");
            assert!(out.starts_with("%USERPROFILE%"), "{out}");
        }
        // A literal of 6 characters or more goes even inside a longer name.
        assert_eq!(
            compiled.text(r"C:\Users\Fake\LONGPR~12"),
            ("%USERPROFILE%2".to_string(), 1)
        );
    }

    #[test] // spec: ARCH-121
    fn sids_go_and_their_neighbours_stay() {
        let r = Redactor::default();
        for (text, want) in [
            (
                r"\\.\pipe\lilith-core-S-1-5-21-1111111111-2222222222-3333333333-1001-dev",
                r"\\.\pipe\lilith-core-<sid>-dev",
            ),
            (
                r"lilith-core-S-1-5-21-1000-2000-3000-1001-test-fea52dff",
                "lilith-core-<sid>-test-fea52dff",
            ),
            ("D:P(A;;GA;;;S-1-5-21-1-2-3-1001)", "D:P(A;;GA;;;<sid>)"),
            ("run as s-1-5-18.", "run as <sid>."),
            ("XS-1-5-18 and S-1 and S-1-5", "XS-1-5-18 and S-1 and S-1-5"),
        ] {
            assert_eq!(r.text(text).0, want, "{text}");
        }
    }

    #[test] // spec: ARCH-121
    fn open_payloads_and_known_tokens_go_anywhere() {
        let r = Redactor::default();
        // `C:\Users\Fake\me.png`, base64url.
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r"C:\Users\Fake\me.png");
        let (out, n) = r.text(&format!("launch lil-dev:open/{payload} now"));
        assert_eq!((out.as_str(), n), ("launch lil-dev:open/<redacted> now", 1));
        assert_eq!(r.text("see http://x/open/abc").1, 0, "only a lil scheme");
        let tokens = [
            format!("ghp_{}", "A1".repeat(18)),
            format!("gho_{}", "b2".repeat(18)),
            format!("github_pat_11{}", "C3_".repeat(10)),
            format!("hf_{}", "d4".repeat(15)),
            format!("sk-ant-api03-{}", "e5-".repeat(10)),
            format!("sk-proj-{}", "F6".repeat(12)),
            format!("AGE-SECRET-KEY-1{}", "G7".repeat(30)),
            format!(
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.{}",
                "h8".repeat(12)
            ),
            format!("AKIA{}", "J9".repeat(8)),
        ];
        for token in &tokens {
            for text in [
                format!("push failed with {token}."),
                format!(r#"{{"m":"{token}"}}"#),
                format!("github = \"{token}\"\n"),
            ] {
                let (out, n) = r.text(&text);
                assert!(!out.contains(token.as_str()), "{text} -> {out}");
                assert!(n >= 1, "{text}");
            }
        }
        assert_eq!(r.text("desk-top and task-list-of-many-words").1, 0);
    }

    #[test] // spec: ARCH-121
    fn scrub_word_values_go_in_every_shape() {
        let r = Redactor::default();
        for (text, secret) in [
            (
                r#"{"m":"cfg { api_key: Some(\"sk-PROBE-A\") }"}"#,
                "PROBE-A",
            ),
            (r#"{"m":"token: Secret(\"PROBE-B\")"}"#, "PROBE-B"),
            (r#"{"m":"password: \\\"PROBE-X\\\""}"#, "PROBE-X"),
            ("run --api-key PROBE-G now", "PROBE-G"),
            (r#"["--password", "PROBE-H"]"#, "PROBE-H"),
            (r#"{"args":["--token","PROBE-I"]}"#, "PROBE-I"),
            (r#"{"m":"args [\"--token\", \"PROBE-J\"]"}"#, "PROBE-J"),
            (
                "args = [\"--listen\", \"--api-key\", \"PROBE-TOML-B\"]",
                "PROBE-TOML-B",
            ),
        ] {
            let (out, n) = r.text(text);
            assert!(!out.contains(secret), "{text} -> {out}");
            assert!(n >= 1, "{text}");
            if text.starts_with('{') || text.starts_with('[') {
                serde_json::from_str::<serde_json::Value>(&out)
                    .unwrap_or_else(|e| panic!("{e}: {text} -> {out}"));
            }
        }
        assert_eq!(r.text("--token --verbose").1, 0, "a flag is no value");
    }

    /// A known literal of 6 characters or more goes wherever it is, inside a word and inside
    /// text with no spaces; a shorter one only as a whole word; a placeholder is never
    /// redacted again.
    #[test] // spec: ARCH-121
    fn known_literals_go_anywhere_and_short_ones_as_whole_words() {
        let r = Redactor {
            labels: vec!["Night Owl".into(), "Work".into()],
            profile_dir: Some(r"C:\Users\Fake\probeuser".into()),
            machine: Some("PROBEBOX-01".into()),
            machine_names: vec!["probebox-01-long-dns-name".into(), "PC07".into()],
            names: vec![r"D:\claude\night-owl-x".into()],
        };
        for (text, want) in [
            (
                r"night owl and Workspace and Work; probeuser on PROBEBOX-01\probeuser via probebox-01-long-dns-name.lan, D:\claude\night-owl-x",
                r"<redacted> and Workspace and <redacted>; <user> on <machine>\<user> via <machine>.lan, <redacted>",
            ),
            (
                "xprobeuserx and NightOwl and xNight Owlx",
                "x<user>x and NightOwl and x<redacted>x",
            ),
            (
                "用户probeuser登录 ユーザーPROBEBOX-01のPC",
                "用户<user>登录 ユーザー<machine>のPC",
            ),
            (
                "PC07 and PC07x and xPC07 and Workers",
                "<machine> and PC07x and xPC07 and Workers",
            ),
        ] {
            assert_eq!(r.text(text).0, want, "{text}");
        }
        // The test channel's account name is `profile`: it goes inside words, never inside a
        // placeholder.
        let r = Redactor {
            profile_dir: Some(r"C:\Users\Fake\profile".into()),
            ..Redactor::default()
        };
        assert_eq!(
            r.text(r"C:\Users\Fake\profile\x and hardware_profile, <redacted>")
                .0,
            r"%USERPROFILE%\x and hardware_<user>, <redacted>"
        );
    }

    /// `config_dir`, the labels and the profile folder match after normalising: `/` and `\`
    /// are the same, a run of them is one, and letter case is ignored.
    #[test] // spec: ARCH-121
    fn paths_and_labels_match_after_normalising() {
        let r = Redactor {
            labels: vec![r"Team/Night Owl".into()],
            names: vec![r"D:\claude\owl-dir".into()],
            ..Redactor::default()
        };
        for (text, secret) in [
            // `Debug` inside JSON, as `tracing::warn!(dir = ?path)` writes it.
            (
                r#"{"m":"cfg { dir: \"D:\\\\claude\\\\owl-dir\" }"}"#,
                "owl-dir",
            ),
            ("D:/claude/owl-dir/x", "owl-dir"),
            (r"d:\CLAUDE\\Owl-Dir", "owl-dir"),
            (r"D:\\\\claude//owl-dir", "owl-dir"),
            (r"team\night owl", "night owl"),
            (r#"{"m":"TEAM\\\\Night Owl"}"#, "night owl"),
        ] {
            let (out, n) = r.text(text);
            assert!(!out.to_lowercase().contains(secret), "{text} -> {out}");
            assert!(n >= 1, "{text} -> {out}");
            if text.starts_with('{') {
                serde_json::from_str::<serde_json::Value>(&out)
                    .unwrap_or_else(|e| panic!("{e}: {text} -> {out}"));
            }
        }
    }

    #[test] // spec: ARCH-121
    fn a_broken_suite_toml_loses_multi_line_values() {
        let r = Redactor::default();
        let doc = "x = = broken\napi_key = \"\"\"\nline-one-SECRET-1\nline-two-SECRET-2\n\"\"\"\nkept = 1\n[auth]\nlist = [\n  \"SECRET-3\",\n]\nother = 2\n";
        let (out, _) = r.toml(doc);
        for secret in ["SECRET-1", "SECRET-2", "SECRET-3"] {
            assert!(!out.contains(secret), "{out}");
        }
        assert!(out.contains("kept = 1"), "{out}");
    }

    /// An escape right before a value is a word boundary for every rule (one helper,
    /// [`starts_word`]): the `\n` kind, `\xXX`, `\uXXXX`, `\UXXXXXXXX`, `%XX` (decoded or not),
    /// `&#NN;` / `&#xHH;`, and a terminal control sequence, which is stripped, before each kind
    /// of value that needs a boundary (short literals, SIDs, tokens, targets and keys).
    #[test] // spec: ARCH-121
    fn an_escape_before_a_value_is_a_boundary_for_every_rule() {
        let r = Redactor {
            labels: vec!["Owl".into()],
            profile_dir: Some(r"C:\Users\Fake\abby".into()),
            machine: Some("PC07".into()),
            machine_names: Vec::new(),
            names: vec![r"D:\claude\owl-dir".into()],
        };
        let ghp = format!("ghp_{}", "A1".repeat(18));
        let values = [
            ("abby", "<user>"),
            ("PC07", "<machine>"),
            ("Owl", "<redacted>"),
            (r"D:\\claude\\owl-dir", "<redacted>"),
            ("S-1-5-21-1-2-3-1001", "<sid>"),
            (ghp.as_str(), "<redacted>"),
            ("LILITH-dev/github/agent-token", "<redacted>"),
            ("api_key=hunter2secret", "<redacted>"),
        ];
        // Each as it stands in a JSON string.
        let escapes = [
            r"\n",
            r"\t",
            r"\r",
            r"\u000a",
            r"\\n",
            r"\\0",
            r"\\v",
            r"\\e",
            r"\\a",
            r"\\x0a",
            r"\\U0000000A",
            "%0A",
            "%22",
            "%2C",
            "%3D",
            "&#10;",
            "&#x0a;",
            r"\u001b[32m",
            r"\u001b[0;1m",
            r"\\x1b[1m",
            r"\\u{1b}[0m",
            r"\\u001b[33m",
            r"\u001b]0;title\u0007",
        ];
        for (value, placeholder) in values {
            for escape in escapes {
                let text = format!(r#"{{"m":"first line{escape}{value} rest"}}"#);
                let (out, n) = r.text(&text);
                assert!(
                    !out.to_lowercase().contains(&value.to_lowercase()),
                    "{text} -> {out}"
                );
                assert!(n >= 1 && out.contains(placeholder), "{text} -> {out}");
                serde_json::from_str::<serde_json::Value>(&out)
                    .unwrap_or_else(|e| panic!("{e}: {text} -> {out}"));
            }
            // The raw escape byte, as a terminal writes it.
            let text = format!("first line\u{1b}[32m{value}\u{1b}[0m rest");
            let (out, _) = r.text(&text);
            assert!(
                out.contains(placeholder) && !out.contains('\u{1b}'),
                "{out}"
            );
        }
        // Still not inside a word.
        assert_eq!(r.text("xabby and abbys and PC07x and Owls").1, 0);
    }

    /// Terminal control sequences go before the rules, in every spelling of their `ESC`: CSI
    /// (`ESC [` … a final byte), OSC (`ESC ]` … BEL or `ESC \`) and the other `ESC x` pairs;
    /// between two letters they leave a space.
    #[test] // spec: ARCH-121
    fn terminal_control_sequences_are_stripped_first() {
        let r = Redactor {
            labels: vec!["Night Owl".into()],
            profile_dir: Some(r"C:\Users\Fake\probeuser".into()),
            machine: Some("PROBEBOX-01".into()),
            ..Redactor::default()
        };
        for (text, want) in [
            (
                r#"{"m":"\u001b[3mtoken\u001b[0m\u001b[2m=\u001b[0mhunter2secret"}"#,
                r#"{"m":"token=<redacted>"}"#,
            ),
            (
                r#"{"m":"\u001b[1mS-1-5-21-111-222-333-1001\u001b[0m"}"#,
                r#"{"m":"<sid>"}"#,
            ),
            (
                r#"{"m":"\u001b[32mprobeuser@PROBEBOX-01\u001b[0m:~$ ls"}"#,
                r#"{"m":"<user>@<machine>:~$ ls"}"#,
            ),
            (
                r#"{"m":"\u001b[1mNight Owl\u001b[0m"}"#,
                r#"{"m":"<redacted>"}"#,
            ),
            (
                r#"{"m":"\u001b[0mLILITH-dev/github-token"}"#,
                r#"{"m":"<redacted>"}"#,
            ),
            (
                r#"{"m":"see \u001b]8;;file:///C:/Users/Fake/probeuser/x.png\u001b\\here\u001b]8;;\u001b\\ now"}"#,
                r#"{"m":"see here now"}"#,
            ),
            (
                r#"{"m":"\u001b]0;C:\\Users\\Fake\\probeuser\u0007title"}"#,
                r#"{"m":"title"}"#,
            ),
            (
                r#"{"m":"a\u001b[1mb and c \u001b[1md"}"#,
                r#"{"m":"a b and c d"}"#,
            ),
            (
                r#"{"m":"\u001b7saved\u001b8 \u001b(Bset"}"#,
                r#"{"m":"saved set"}"#,
            ),
            (
                r#"{"m":"\\u{1b}[32mok\\x1b[0m \\u001b[1mfine"}"#,
                r#"{"m":"ok fine"}"#,
            ),
            ("\u{1b}[31mred\u{1b}[0m text", "red text"),
        ] {
            let (out, _) = r.text(text);
            assert_eq!(out, want, "{text}");
            if text.starts_with('{') {
                serde_json::from_str::<serde_json::Value>(&out)
                    .unwrap_or_else(|e| panic!("{e}: {text} -> {out}"));
            }
        }
    }

    /// A Deny token with escaped `/`, and an `open/` payload holding the percent escapes
    /// base64 uses (`%2B`, `%2F`, `%3D`), go as the plain forms do; so does a key's value after
    /// `%3D`.
    #[test] // spec: ARCH-121, APP-020
    fn escaped_deny_tokens_and_percent_encoded_payloads_go() {
        let r = Redactor::default();
        let token = "c0ffee".repeat(5) + "ab";
        for (text, secret) in [
            (
                format!(r#"{{"u":"lil-dev:deny\/cf_0123456789abcdef\/{token}"}}"#),
                token.as_str(),
            ),
            (
                format!(r#"{{"u":"lil-dev:deny\\\/cf_0123456789abcdef\\\/{token}"}}"#),
                token.as_str(),
            ),
            (
                "lil:open/QzpcVXNlcnNc%2BcHJvYmV1c2VyXFBpY3R1cmVz".to_string(),
                "cHJvYmV1",
            ),
            ("lil:open/QzpcVXNl%2fcHJv%3D%3D x".to_string(), "cHJv"),
            ("api_key%3Dhunter2secret".to_string(), "hunter2secret"),
            (
                "next=%2Fcb%3Ftoken%3Dhunter2secret".to_string(),
                "hunter2secret",
            ),
        ] {
            let (out, n) = r.text(&text);
            assert!(!out.contains(secret), "{text} -> {out}");
            assert!(n >= 1, "{text}");
            if text.starts_with('{') {
                serde_json::from_str::<serde_json::Value>(&out)
                    .unwrap_or_else(|e| panic!("{e}: {text} -> {out}"));
            }
        }
    }

    #[test] // spec: ARCH-121
    fn a_sid_goes_whatever_follows_it() {
        let r = Redactor::default();
        for (text, want) in [
            ("O:S-1-5-21-1-2-3-1001G:SY", "O:<sid>G:SY"),
            ("S-1-5-21-1-2-3-1001x", "<sid>x"),
            ("pipe-S-1-5-18-dev", "pipe-<sid>-dev"),
        ] {
            assert_eq!(r.text(text).0, want, "{text}");
        }
    }

    #[test] // spec: ARCH-121, APP-020
    fn an_open_payload_goes_whether_it_decodes_or_not() {
        let r = Redactor::default();
        for (text, want) in [
            // Truncated to 1 mod 4: it no longer decodes.
            (
                "lil:open/QzpcVXNlcnNccHJvYmV1c2VyXFBpY3R1cm",
                "lil:open/<redacted>",
            ),
            ("lil:open/QzpcVXNl…", "lil:open/<redacted>…"),
            ("lil://open/QzpcVXNl", "lil://open/<redacted>"),
            (
                r#"{"u":"lil-dev:open\/QzpcVXNl"}"#,
                r#"{"u":"lil-dev:open\/<redacted>"}"#,
            ),
            ("LIL-TEST-x1:OPEN/ab+cd/ef==", "LIL-TEST-x1:OPEN/<redacted>"),
            (
                r#"{"u":"lil:open/ab+c\/d=="}"#,
                r#"{"u":"lil:open/<redacted>"}"#,
            ),
            (
                "http://x/open/abc and file:open/x",
                "http://x/open/abc and file:open/x",
            ),
        ] {
            assert_eq!(r.text(text).0, want, "{text}");
        }
    }

    /// Each pass decodes one more level, up to [`MAX_PASSES`]: a value encoded twice goes.
    #[test] // spec: ARCH-121
    fn a_value_encoded_two_levels_deep_goes() {
        let r = Redactor {
            profile_dir: Some(r"C:\Users\Fake\probeuser".into()),
            ..Redactor::default()
        };
        for (text, secret) in [
            ("probe%2540mail.example", "probe"),
            (
                r"C%253A%255CUsers%255CFake%255Cprobeuser%255Cx",
                "probeuser",
            ),
            ("J%C3%BCrgen probe%2540mail.example", "probe"),
            (r#"{"a":"{\"b\":\"probe\\u0040mail.example\"}"}"#, "probe"),
            ("probe&#64;mail.example", "probe"),
            (r"lil:open%252FQzpcVXNl", "QzpcVXNl"),
        ] {
            let (out, n) = r.text(text);
            assert!(!out.contains(secret), "{text} -> {out}");
            assert!(n >= 1, "{text}");
        }
        assert!(r.text("J%C3%BCrgen").0.contains("Jürgen"));
    }

    #[test] // spec: ARCH-121
    fn a_toml_multi_line_string_in_log_text_goes_whole() {
        let r = Redactor::default();
        for (text, secret) in [
            ("api_key = '''PRB-8''' and on", "PRB-8"),
            ("api_key = \"\"\"PRB-9\nnext\"\"\" and on", "PRB-9"),
            (r#"{"m":"api_key = \"\"\"PRB-10\"\"\" end"}"#, "PRB-10"),
            ("api_key = '''PRB-11 never closed", "PRB-11"),
        ] {
            let (out, _) = r.text(text);
            assert!(!out.contains(secret), "{text} -> {out}");
        }
        assert!(
            r.text("api_key = '''PRB-8''' and on")
                .0
                .ends_with(" and on")
        );
        // Never closed: only to the end of its own line, never the rest of the entry.
        let entry = "{\n  \"prompt\": \"api_key = '''PRB-12\",\n  \"untrusted\": true\n}";
        let (out, _) = r.text(entry);
        assert!(!out.contains("PRB-12"), "{out}");
        assert!(out.ends_with("\n  \"untrusted\": true\n}"), "{out}");
        let (out, _) = r.text("api_key = \"\"\"PRB-13\nkept = 2");
        assert_eq!(out, "api_key = \"\"\"<redacted>\"\"\"\nkept = 2");
    }
}
