//! The diagnostics bundle's redaction (ARCH-121, QA-084, AI-092; Appendix G "Diagnostics").
//!
//! [`Compiled::text`] runs these rules over every entry, in order, and then again until
//! nothing changes, at most [`MAX_PASSES`] times, so a value encoded inside another encoding
//! goes too; each replacement counts once. It does so over the text as it came, then, when
//! the text holds terminal control sequences, strips them and does so again (below), so the
//! strip never undoes what the first round caught.
//!
//! 0. **Decoding.** One level of encoding is decoded: `%5C`, `%2F`, `%3A`, `%40`, `%20`,
//!    `%2B`, `%3D` and `%25`, percent-encoded UTF-8, and the `\uXXXX` (after any run of `\`),
//!    `&#NN;` and `&#xHH;` forms of those characters and of non-ASCII ones (a backslash stays
//!    escaped as `\\`), so an encoded path, name or address is seen.
//! 1. **Names**: every Claude Code profile label and profile folder (`config_dir`) of
//!    `suite.toml`, and any other name given, as written or JSON-escaped: `<redacted>`.
//! 2. **The user-profile folder**: `%USERPROFILE%`, in its long form and its 8.3 short form,
//!    in any letter case, with any run of `\` or `/` as a separator (so escaped once or twice),
//!    with or without a `\\?\` prefix (owner decision 2026-10-22).
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
//! **Terminal control sequences** ([`strip_terminal`]): a CSI (`ESC [`, parameter and
//! intermediate bytes, a final byte, or up to a placeholder the first round left), an OSC
//! (`ESC ]` to its terminator: after a raw `ESC` a raw BEL or `ESC \`, after a written one
//! `\u0007`, `\x07` or a written `ESC \` of the same escape level, never `\a`) and, after a
//! raw `ESC`, a 2-byte sequence ([`LONE_FINALS`]; before any other byte the `ESC` goes alone).
//! The `ESC` is the raw byte or `\u001b`, `\x1b` or `\u{1b}` after any run of `\`; `\e`,
//! `\033`, `%1B` and `&#27;` are one only before a whole CSI. Nothing is put in their place,
//! so a value they split is whole again; where one stood is a word boundary.
//!
//! The known literals of rules 1, 2, 9 and 10 match in any letter case, a run of `\` or `/`
//! matching any such run (a `config_dir` without a trailing separator or a `\\?\` prefix); one
//! of [`ANYWHERE_CHARS`] characters or more goes wherever it is, inside a word too, and a
//! shorter one only as a whole word. No literal is matched inside a placeholder. Whether a
//! value starts a word is one test for every rule ([`starts_word`]), which counts an escape
//! right before it as a boundary.
//!
//! In `suite.toml` the TOML is read first ([`Redactor::toml`]): every value under a scrub-word
//! key or table, and each `[[claude_code.profiles]]` `label`; then the rules above over the
//! whole file, comments included. A `suite.toml` that doesn't parse is redacted line by line,
//! multi-line values included.

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
        let names = self.names.iter().map(|n| path_literal(n));
        for name in self.labels.iter().cloned().chain(names) {
            if name.trim().is_empty() {
                continue;
            }
            let escaped = serde_json::to_string(&name).unwrap_or_default();
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

/// The path literal `name` as it is matched: without a `\\?\` prefix (`\\?\UNC\` becoming the
/// `\\` it stands for) and without separators at its end.
fn path_literal(name: &str) -> String {
    const UNC: &str = r"\\?\UNC\";
    let name = match name.get(..UNC.len()) {
        Some(head) if head.eq_ignore_ascii_case(UNC) => format!(r"\\{}", &name[UNC.len()..]),
        _ => name.strip_prefix(r"\\?\").unwrap_or(name).to_string(),
    };
    name.trim_end_matches(['\\', '/']).to_string()
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

/// What one [`Compiled::text`] call carries from rule to rule: how many replacements were made,
/// and where a stripped terminal control sequence stood (byte offsets, ascending, moved with
/// every rule's edits). Each of those places is a word boundary ([`starts_word`]).
#[derive(Debug, Default)]
struct Cx {
    n: u32,
    bounds: Vec<usize>,
}

impl Cx {
    /// Whether a stripped sequence stood at byte `i`.
    fn bound_at(&self, i: usize) -> bool {
        !self.bounds.is_empty() && self.bounds.binary_search(&i).is_ok()
    }
}

/// A rule's output, built from its input `s`: the text it keeps and its replacements, in
/// order. [`Out::finish`] moves [`Cx::bounds`] with the edits; a boundary inside a replaced
/// value goes with it.
struct Out<'a> {
    s: &'a str,
    out: String,
    last: usize,
    edits: Vec<(usize, usize, usize)>,
}

impl<'a> Out<'a> {
    fn new(s: &'a str) -> Self {
        Out {
            s,
            out: String::with_capacity(s.len()),
            last: 0,
            edits: Vec::new(),
        }
    }

    /// `s[start..end]` (at or after the previous replacement's end) becomes `with`.
    fn replace(&mut self, start: usize, end: usize, with: &str) {
        self.out.push_str(&self.s[self.last..start]);
        self.out.push_str(with);
        self.edits.push((start, end, with.len()));
        self.last = end;
    }

    fn finish(mut self, cx: &mut Cx) -> String {
        self.out.push_str(&self.s[self.last..]);
        if !self.edits.is_empty() && !cx.bounds.is_empty() {
            let edits = &self.edits;
            cx.bounds.retain_mut(|p| {
                let mut shift = 0isize;
                for &(start, end, len) in edits {
                    if *p <= start {
                        break;
                    }
                    if *p < end {
                        return false;
                    }
                    shift += len as isize - (end - start) as isize;
                }
                *p = p.saturating_add_signed(shift);
                true
            });
        }
        self.out
    }
}

impl Compiled {
    /// [`Redactor::text`] (module docs): the rules over the text as it came; then, when it holds
    /// terminal control sequences, the rules again over it with them stripped (where each
    /// stood is a word boundary), so stripping never undoes what the first round caught.
    pub(crate) fn text(&self, s: &str) -> (String, u32) {
        let mut cx = Cx::default();
        let first = self.passes(s, &mut cx);
        let Some((stripped, bounds)) = strip_terminal(&first) else {
            return (first, cx.n);
        };
        cx.bounds = bounds;
        let second = self.passes(&stripped, &mut cx);
        (second, cx.n)
    }

    /// The decode-and-redact pass, again until nothing changes, at most [`MAX_PASSES`] times,
    /// so an encoding nested in another (each pass decodes one level) is redacted too.
    fn passes(&self, s: &str, cx: &mut Cx) -> String {
        let mut current = self.pass(s, cx);
        for _ in 1..MAX_PASSES {
            let next = self.pass(&current, cx);
            if next == current {
                break;
            }
            current = next;
        }
        current
    }

    /// One decode-and-redact pass over `s` (module docs).
    fn pass(&self, s: &str, cx: &mut Cx) -> String {
        let mut s = decode_escapes(s, cx);
        for word in &self.words {
            s = replace_literal(&s, word, REDACTED, cx);
        }
        let s = self.profile(&s, cx);
        let s = sids(&s, cx);
        let s = open_payloads(&s, cx);
        let s = deny_tokens(&s, cx);
        let s = known_tokens(&s, cx);
        let s = targets(&s, cx);
        let mut s = emails(&s, cx);
        if let Some(user) = &self.user {
            s = replace_literal(&s, user, USER, cx);
        }
        for form in &self.machines {
            s = replace_literal(&s, form, MACHINE, cx);
        }
        keys(&s, cx)
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

    fn profile(&self, s: &str, cx: &mut Cx) -> String {
        if self.profiles.is_empty() {
            return s.to_string();
        }
        let b = s.as_bytes();
        let mut out = Out::new(s);
        let mut i = 0;
        while i < b.len() {
            let candidate = b[i] == b'\\'
                || b[i] == b'/'
                || (b[i].is_ascii_alphabetic() && b.get(i + 1) == Some(&b':'));
            if candidate && let Some(end) = self.profiles.iter().find_map(|f| f.match_at(s, i)) {
                out.replace(i, end, PROFILE);
                cx.n += 1;
                i = end;
                continue;
            }
            i += 1;
        }
        out.finish(cx)
    }
}

/// How an `ESC` was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscForm {
    /// The raw byte.
    Raw,
    /// `\u001b`, `\x1b` or `\u{1b}` after a run of this many `\`: what JSON, Python's `repr`
    /// and Rust's `Debug` write.
    Written(usize),
    /// `\e` or `\033` (after a run of `\`), `%1B` or `&#27;`: spellings of source text, an
    /// `ESC` only before a whole CSI.
    Source,
}

/// An `ESC` found at some byte: where it ends, how many backslashes of the run before it stay
/// (the escapes of other levels, as [`decode_escapes`] keeps them), and how it was written.
#[derive(Debug, Clone, Copy)]
struct Esc {
    end: usize,
    keep: usize,
    form: EscForm,
}

/// The `ESC` at byte `i` of `b`, in any spelling of [`EscForm`].
fn esc_at(b: &[u8], i: usize) -> Option<Esc> {
    let source = |end: usize| Esc {
        end,
        keep: 0,
        form: EscForm::Source,
    };
    match *b.get(i)? {
        0x1b => {
            return Some(Esc {
                end: i + 1,
                keep: 0,
                form: EscForm::Raw,
            });
        }
        b'%' => {
            return b
                .get(i + 1..i + 3)
                .filter(|h| h.eq_ignore_ascii_case(b"1b"))
                .map(|_| source(i + 3));
        }
        b'&' => return b[i..].starts_with(b"&#27;").then(|| source(i + 5)),
        b'\\' => {}
        _ => return None,
    }
    let run = b[i..].iter().take_while(|c| **c == b'\\').count();
    let rest = &b[i + run..];
    let (body, form) =
        if rest.len() >= 5 && rest[0] == b'u' && rest[1..5].eq_ignore_ascii_case(b"001b") {
            (5, EscForm::Written(run))
        } else if rest.len() >= 3 && rest[0] == b'x' && rest[1..3].eq_ignore_ascii_case(b"1b") {
            (3, EscForm::Written(run))
        } else if rest.starts_with(b"u{") {
            let digits = rest[2..]
                .iter()
                .take_while(|c| c.is_ascii_hexdigit())
                .count();
            let hex = std::str::from_utf8(&rest[2..2 + digits]).unwrap_or_default();
            let ok = digits > 0
                && rest.get(2 + digits) == Some(&b'}')
                && u32::from_str_radix(hex, 16) == Ok(0x1b);
            (ok.then_some(3 + digits)?, EscForm::Written(run))
        } else if rest.starts_with(b"033") {
            (3, EscForm::Source)
        } else if rest.first() == Some(&b'e') {
            (1, EscForm::Source)
        } else {
            return None;
        };
    let keep = if run % 2 == 1 { run - 1 } else { run - 2 };
    Some(Esc {
        end: i + run + body,
        keep,
        form,
    })
}

/// The end of a string terminator `ESC \` at byte `j` that closes an OSC opened by an `ESC`
/// written `form`: after a raw `ESC`, a raw one and one `\`; after a written one, a written
/// one of the same escape level (backslash parity) and a `\` written as `\\`.
fn st_at(b: &[u8], j: usize, form: EscForm) -> Option<usize> {
    let esc = esc_at(b, j)?;
    let run = b[esc.end..].iter().take_while(|c| **c == b'\\').count();
    match (form, esc.form) {
        (EscForm::Raw, EscForm::Raw) => (run >= 1).then_some(esc.end + 1),
        (EscForm::Written(r), EscForm::Written(r2)) if r % 2 == r2 % 2 => {
            let take = run - run % 2;
            (take > 0).then_some(esc.end + take)
        }
        _ => None,
    }
}

/// The end of the terminator at byte `j` of an OSC opened by an `ESC` written `form`: after a
/// raw `ESC`, the raw BEL or a raw `ESC \`; after a written one, `\u0007` or `\x07` at the
/// `ESC`'s own escape level (backslash parity), or a written `ESC \` ([`st_at`]). `\a` is never
/// a BEL: in log text it is a backslash and a letter, as in `D:\assets`.
fn osc_end_at(b: &[u8], j: usize, form: EscForm) -> Option<usize> {
    match form {
        EscForm::Raw if b.get(j) == Some(&0x07) => return Some(j + 1),
        EscForm::Written(r) => {
            let run = b[j..].iter().take_while(|c| **c == b'\\').count();
            let rest = &b[j + run..];
            if run > 0 && run % 2 == r % 2 {
                if rest.starts_with(b"u0007") {
                    return Some(j + run + 5);
                }
                if rest.starts_with(b"x07") {
                    return Some(j + run + 3);
                }
            }
        }
        _ => {}
    }
    st_at(b, j, form)
}

/// The end of a whole CSI whose `[` is just before byte `j`: parameter bytes, intermediate
/// bytes and a final byte (0x40–0x7E). A `"` or a `\` never belongs to one, so JSON stays JSON,
/// and neither does a placeholder: a CSI whose tail a first-round redaction replaced ends where
/// the placeholder starts.
fn csi_end(s: &str, j: usize) -> Option<usize> {
    let b = s.as_bytes();
    let placeholder = |j: usize| PLACEHOLDERS.iter().any(|p| s[j..].starts_with(p));
    let mut j = j;
    while j < b.len() && (0x30..=0x3f).contains(&b[j]) && !placeholder(j) {
        j += 1;
    }
    while j < b.len() && (0x20..=0x2f).contains(&b[j]) && b[j] != b'"' && !placeholder(j) {
        j += 1;
    }
    if j < b.len() && placeholder(j) {
        return Some(j);
    }
    (j < b.len() && (0x40..=0x7e).contains(&b[j]) && b[j] != b'\\').then_some(j + 1)
}

/// The final bytes of the 2-byte sequences a raw `ESC` starts (`ESC 7`, `ESC c`, `ESC \` …).
const LONE_FINALS: &[u8] = b"78=>cDEHMNOPZ\\";

/// The end of the terminal control sequence started by `esc` in `s`, if it starts one: a CSI
/// for every spelling; an OSC to its terminator ([`osc_end_at`]) for a raw or written `ESC`;
/// for a raw `ESC`, one of [`LONE_FINALS`] after it, or else the `ESC` alone. A written or
/// source `ESC` that starts none of these is no `ESC`.
fn sequence_end(s: &str, esc: &Esc) -> Option<usize> {
    let b = s.as_bytes();
    let p = esc.end;
    let raw = esc.form == EscForm::Raw;
    match b.get(p) {
        Some(b'[') => csi_end(s, p + 1).or(raw.then_some(p)),
        Some(b']') if esc.form != EscForm::Source => {
            let mut j = p + 1;
            while j < b.len() && j - p <= MAX_OSC_BYTES && !matches!(b[j], b'"' | b'\n') {
                if let Some(end) = osc_end_at(b, j, esc.form) {
                    return Some(end);
                }
                // A run of `\` is one escape level: never looked into from its middle.
                j += b[j..].iter().take_while(|c| **c == b'\\').count().max(1);
            }
            raw.then_some(p)
        }
        Some(c) if raw && LONE_FINALS.contains(c) => Some(p + 1),
        _ => raw.then_some(p),
    }
}

/// The end of a plain SGR at byte `j` of `s`, if one starts there: an `ESC` in any spelling
/// ([`esc_at`]), `[`, parameters from `0-9 ; :` only, no intermediate bytes, then `m`. It is
/// the only sequence a first-round word steps over (never an `ESC \`, an OSC or any other
/// CSI), and it is looked for only at a byte that can start an `ESC` spelling, so a run of
/// `\` is counted once.
fn plain_sgr_at(s: &str, j: usize) -> Option<usize> {
    let b = s.as_bytes();
    let starts = match *b.get(j)? {
        0x1b | b'%' | b'&' => true,
        b'\\' => j == 0 || b[j - 1] != b'\\',
        _ => false,
    };
    if !starts {
        return None;
    }
    let esc = esc_at(b, j)?;
    if b.get(esc.end) != Some(&b'[') {
        return None;
    }
    let params = b[esc.end + 1..]
        .iter()
        .take_while(|c| c.is_ascii_digit() || matches!(c, b';' | b':'))
        .count();
    let m = esc.end + 1 + params;
    (b.get(m) == Some(&b'm')).then_some(m + 1)
}

/// `s` with every terminal control sequence removed (module docs, rule 0), and where each run
/// of them stood (byte offsets in the result, ascending); `None` when it has none. Nothing is
/// put in their place, so a value a sequence split is whole again.
fn strip_terminal(s: &str) -> Option<(String, Vec<usize>)> {
    let maybe = ["\u{1b}", "1b", "1B", "\\e", "\\033", "&#27;"];
    if !maybe.iter().any(|m| s.contains(m)) {
        return None;
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut bounds: Vec<usize> = Vec::new();
    let (mut last, mut i) = (0, 0);
    while i < b.len() {
        let Some((esc, mut end)) =
            esc_at(b, i).and_then(|esc| sequence_end(s, &esc).map(|end| (esc, end)))
        else {
            // A run of `\` is one escape level: never looked into from its middle.
            i += b[i..].iter().take_while(|c| **c == b'\\').count().max(1);
            continue;
        };
        out.push_str(&s[last..i + esc.keep]);
        // The sequences right after it go with it.
        while let Some(next) = esc_at(b, end).filter(|e| e.keep == 0)
            && let Some(next_end) = sequence_end(s, &next)
        {
            end = next_end;
        }
        if bounds.last() != Some(&out.len()) {
            bounds.push(out.len());
        }
        last = end;
        i = end;
    }
    if last == 0 {
        return None;
    }
    out.push_str(&s[last..]);
    Some((out, bounds))
}

/// `s` with one level of the encodings of rule 0 decoded (module docs); borrowed when there are
/// none. `%25` decodes to `%`, so a twice percent-encoded form is decoded by the next pass; a
/// run of percent-encoded bytes that is UTF-8 decodes to its characters; a `\uXXXX` escape
/// after any run of `\` (a JSON string inside a JSON string) decodes, the run losing the
/// backslashes of that escape level; and `&#NN;` / `&#xHH;` decode as the `\u` forms do.
fn decode_escapes(s: &str, cx: &mut Cx) -> String {
    if !s.contains('%') && !s.contains("\\u") && !s.contains("&#") {
        return s.to_string();
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
    let mut out = Out::new(s);
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(code) = hex(i + 1..i + 3)
        {
            if code == 0x25 {
                out.replace(i, i + 3, "%");
                i += 3;
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
                    out.replace(i, j, &text);
                    i = j;
                    continue;
                }
            } else if let Some(text) = decoded(code) {
                // `%5C` becomes an escaped backslash, so JSON stays JSON.
                out.replace(i, i + 3, &text);
                i += 3;
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
                out.replace(i, u + 5, &format!("{}{text}", &s[i..i + keep]));
                i = u + 5;
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
                out.replace(i, from + digits + 1, &text);
                i = from + digits + 1;
                continue;
            }
        }
        i += 1;
    }
    out.finish(cx)
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
/// escape is a boundary, so a value at the start of an escaped line is found too. So is the
/// place a stripped terminal control sequence stood ([`Cx::bounds`]), and the place a CSI's
/// final byte would go after an `ESC [` and its parameters, so a value a cut sequence runs into
/// is found before the strip. (`\"`, `\\`, `\/`, `\u{…}`, `&#NN;` and `&#xHH;` end in a
/// character that is no word character anyway.)
fn starts_word(s: &str, i: usize, word: impl Fn(char) -> bool, cx: &Cx) -> bool {
    if !s.is_char_boundary(i) {
        return false;
    }
    let Some(prev) = s[..i].chars().next_back() else {
        return true;
    };
    let b = s.as_bytes();
    !word(prev) || cx.bound_at(i) || after_escape(b, i) || after_csi_head(b, i)
}

/// Whether byte `i` of `b` comes after an `ESC [` (any spelling) and parameter bytes, where a
/// CSI's final byte would go.
fn after_csi_head(b: &[u8], i: usize) -> bool {
    let mut k = i;
    while k > 0 && (0x30..=0x3f).contains(&b[k - 1]) {
        k -= 1;
    }
    if k == i || k < 2 || b[k - 1] != b'[' {
        return false;
    }
    let e = k - 1;
    b[e - 1] == 0x1b
        || matches!(b[e - 1], b'}' | b';')
        || b[..e].ends_with(br"\033")
        || after_escape(b, e)
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
fn replace_literal(s: &str, literal: &str, with: &str, cx: &mut Cx) -> String {
    let (Some(first), Some(final_char)) = (literal.chars().next(), literal.chars().next_back())
    else {
        return s.to_string();
    };
    let anywhere = literal.chars().count() >= ANYWHERE_CHARS;
    let b = s.as_bytes();
    let mut out = Out::new(s);
    let mut i = 0;
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
            && (anywhere || !word_char(first) || starts_word(s, i, word_char, cx))
            && let Some(end) = match_literal(s, i, literal)
            && (anywhere
                || !word_char(final_char)
                || cx.bound_at(end)
                || !s[end..].chars().next().is_some_and(word_char))
        {
            out.replace(i, end, with);
            cx.n += 1;
            i = end;
            continue;
        }
        i += c.len_utf8();
    }
    out.finish(cx)
}

/// Whether `key` holds a scrub word (upper-cased).
fn holds_scrub_word(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SCRUB_WORDS.iter().any(|w| upper.contains(w))
}

/// Every Windows SID (`S-1-<n>-<n>…`, any case, starting a word) replaced by `<sid>`, whatever
/// follows it; it ends at the first `-` not followed by a digit (`-dev`, `-test-…` stay).
fn sids(s: &str, cx: &mut Cx) -> String {
    let b = s.as_bytes();
    let digits = |from: usize| {
        b[from.min(b.len())..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count()
    };
    let mut out = Out::new(s);
    let mut i = 0;
    while i + 4 <= b.len() {
        let starts = matches!(b[i], b'S' | b's')
            && b[i + 1] == b'-'
            && b[i + 2] == b'1'
            && starts_word(s, i, |c| c.is_ascii_alphanumeric(), cx);
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
                out.replace(i, j, SID);
                cx.n += 1;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out.finish(cx)
}

/// Every `open/` payload under a suite scheme (`lil`, `lil-dev`, `lil-test-…`, any case,
/// starting a word) replaced, whether it decodes or not: it is a path, a ref or a name
/// (APP-020). The scheme may be followed by `//` (`lil://open/…`), and each `/` may be escaped
/// (`open\/…`); the payload runs over `[A-Za-z0-9+/=_-]`, standard base64 included.
fn open_payloads(s: &str, cx: &mut Cx) -> String {
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
    let mut out = Out::new(s);
    let mut i = 0;
    while i + 3 <= b.len() {
        if !(b[i..i + 3].eq_ignore_ascii_case(b"lil") && starts_word(s, i, scheme_char, cx)) {
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
        out.replace(from, end, REDACTED);
        cx.n += 1;
        i = end;
    }
    out.finish(cx)
}

/// `deny/cf_<16 hex>/<32 hex>` with its token replaced; each `/` may be escaped (`\/`, with any
/// run of `\`).
fn deny_tokens(s: &str, cx: &mut Cx) -> String {
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
    let mut out = Out::new(s);
    let mut i = 0;
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
                out.replace(token, token + 32, REDACTED);
                cx.n += 1;
                i = token + 32;
                continue;
            }
        }
        i += 1;
    }
    out.finish(cx)
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
fn known_tokens(s: &str, cx: &mut Cx) -> String {
    let b = s.as_bytes();
    let run = |from: usize, ok: fn(u8) -> bool| {
        b[from.min(b.len())..]
            .iter()
            .take_while(|c| ok(**c))
            .count()
    };
    let mut out = Out::new(s);
    let mut i = 0;
    while i < b.len() {
        let could_start = matches!(b[i], b'g' | b'h' | b's' | b'A' | b'x' | b'e');
        if !could_start || !starts_word(s, i, |c| c.is_ascii_alphanumeric() || c == '_', cx) {
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
                out.replace(i, end, REDACTED);
                cx.n += 1;
                i = end;
            }
            None => i += 1,
        }
    }
    out.finish(cx)
}

/// Every Credential Manager target replaced: the suite's name or its name before the rename
/// (any case, not inside a longer word), an optional `-<channel>`, `/`, then the name up to a
/// space, quote, backslash or closing bracket, or, for a quoted target, to its quote.
fn targets(s: &str, cx: &mut Cx) -> String {
    let b = s.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-';
    let mut out = Out::new(s);
    let mut i = 0;
    while i < b.len() {
        let could_start = matches!(b[i], b'L' | b'l' | b'W' | b'w');
        if !could_start || !starts_word(s, i, |c| c.is_ascii() && word(c as u8), cx) {
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
            None => loop {
                let from = end;
                // A plain SGR is stepped over, never stopped in at its `;`.
                while let Some(after) = plain_sgr_at(s, end) {
                    end = after;
                }
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
                if end == from {
                    break;
                }
            },
        }
        if end == name {
            i += 1;
            continue;
        }
        out.replace(i, end, REDACTED);
        cx.n += 1;
        i = end;
    }
    out.finish(cx)
}

/// Every e-mail address (`[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}`, leftmost and
/// longest as a regex finds it) replaced. A local part that would start inside a JSON escape
/// (`\n…`) starts after it, so the JSON stays JSON.
fn emails(s: &str, cx: &mut Cx) -> String {
    let b = s.as_bytes();
    let local = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'%' | b'+' | b'-');
    let domain = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-');
    let mut out = Out::new(s);
    let mut i = 0;
    while let Some(off) = b.get(i..).and_then(|r| r.iter().position(|c| *c == b'@')) {
        let at = i + off;
        i = at + 1;
        let mut start = at;
        while start > out.last && local(b[start - 1]) {
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
            out.replace(start, end, REDACTED);
            cx.n += 1;
            i = end;
        }
    }
    out.finish(cx)
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
fn keys(s: &str, cx: &mut Cx) -> String {
    let b = s.as_bytes();
    let key_byte = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.');
    let spaces = |mut k: usize| {
        while matches!(b.get(k), Some(b' ' | b'\t')) {
            k += 1;
        }
        k
    };
    let mut out = Out::new(s);
    let mut i = 0;
    while i < b.len() {
        if !key_byte(b[i]) || !starts_word(s, i, |c| c.is_ascii() && key_byte(c as u8), cx) {
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
        out.replace(value_start, value_end, &with);
        cx.n += 1;
        i = value_end;
    }
    out.finish(cx)
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
    // A value that starts with plain SGRs: they go with it, and what follows them is parsed
    // as a value (quoted, braced or bare).
    let mut lead = k;
    while let Some(after) = plain_sgr_at(s, lead) {
        lead = after;
    }
    if lead > k {
        return value_at(s, lead, quote).map(|(_, end, with)| (k, end, with));
    }
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
                while j < b.len() {
                    // A plain SGR is stepped over, never stopped in at its `;`.
                    if let Some(after) = plain_sgr_at(s, j) {
                        j = after;
                    } else if stop(j) {
                        break;
                    } else {
                        j += 1;
                    }
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

    /// Terminal control sequences go, after a first redaction of the text as it came (so the
    /// strip never undoes what that caught) and before a second: CSI (`ESC [` … a final byte),
    /// OSC (`ESC ]` … BEL or `ESC \`, a written BEL only for a written `ESC` of its escape
    /// level, never `\a`) and, for a raw `ESC`, the real 2-byte sequences; nothing is put in
    /// their place.
    #[test] // spec: ARCH-121
    fn terminal_control_sequences_are_stripped_first() {
        let r = Redactor {
            labels: vec!["Night Owl".into()],
            profile_dir: Some(r"C:\Users\Fake\probeuser".into()),
            machine: Some("PROBEBOX-01".into()),
            ..Redactor::default()
        };
        let alice = Redactor {
            profile_dir: Some(r"C:\Users\Fake\alice".into()),
            names: vec![r"D:\claude\abc-dir".into()],
            ..Redactor::default()
        };
        let alexander = Redactor {
            profile_dir: Some(r"C:\Users\Fake\alexander".into()),
            ..Redactor::default()
        };
        let ghp = format!("ghp_{}", "Ab1".repeat(12));
        // An OSC whose body holds a path with `\a` in it ends at its real BEL, never at `\a`;
        // and a lone `ESC` takes no value's first character with it.
        for (redactor, text, want) in [
            (
                &alice,
                r#"{"m":"\u001b]0;C:\\Users\\Fake\\alice\\x\u0007 started"}"#.to_string(),
                r#"{"m":" started"}"#.to_string(),
            ),
            (
                &alice,
                "\u{1b}]0;C:\\Users\\Fake\\alice\\x\u{7} started".to_string(),
                " started".to_string(),
            ),
            (
                &alexander,
                "\u{1b}]0;C:\\Users\\Fake\\alexander\\run.bat\u{7}".to_string(),
                String::new(),
            ),
            (
                &alexander,
                r#"{"m":"\u001b]0;Admin: C:\\Users\\Fake\\alexander\u0007"}"#.to_string(),
                r#"{"m":""}"#.to_string(),
            ),
            (
                &alice,
                "\u{1b}]0;D:\\claude\\abc-dir\u{7}".to_string(),
                String::new(),
            ),
            (
                &alexander,
                "\u{1b}]0;C:\\Users\\Fake\\alexander".to_string(),
                "]0;%USERPROFILE%".to_string(),
            ),
            // No literal to catch the path first: `\a` is still no BEL, and a written BEL of
            // another escape level (`\\x07`, a backslash and `x07`) ends nothing.
            (&r, "\u{1b}]0;D:\\art\u{7}ok".to_string(), "ok".to_string()),
            (
                &r,
                r#"{"m":"\u001b]0;D:\\x07dir\u0007ok"}"#.to_string(),
                r#"{"m":"ok"}"#.to_string(),
            ),
            // A bare value coloured with a multi-parameter SGR (`\033[1;32m`, as custom nodes
            // print): the first round's word steps over the whole sequence, never stopping at
            // its `;`.
            (
                &r,
                "api_key=\u{1b}[1;32mhunter2secret\u{1b}[0m".to_string(),
                "api_key=<redacted>".to_string(),
            ),
            (
                &r,
                "api_key=\u{1b}[0;32mhunter2secret\u{1b}[0m".to_string(),
                "api_key=<redacted>".to_string(),
            ),
            (
                &r,
                "password=\u{1b}[38;5;208mhunter2secret\u{1b}[0m".to_string(),
                "password=<redacted>".to_string(),
            ),
            (
                &r,
                "API key: \u{1b}[1;32mhunter2secret\u{1b}[0m".to_string(),
                "API key: <redacted>".to_string(),
            ),
            (
                &r,
                "token: \u{1b}[1;32mhunter2secret\u{1b}[0m".to_string(),
                "token: <redacted>".to_string(),
            ),
            (
                &r,
                r#"{"m":"api_key=\u001b[1;32mhunter2secret\u001b[0m"}"#.to_string(),
                r#"{"m":"api_key=<redacted>"}"#.to_string(),
            ),
            (
                &r,
                r"'api_key=\x1b[1;32mhunter2secret\x1b[0m'".to_string(),
                "'api_key=<redacted>'".to_string(),
            ),
            (
                &r,
                "Authorization: Bearer \u{1b}[1;32mhunter2secret".to_string(),
                "Authorization: <redacted>".to_string(),
            ),
            (
                &r,
                "--api-key \u{1b}[1;32mhunter2secret".to_string(),
                "--api-key <redacted>".to_string(),
            ),
            (
                &r,
                format!("token=\u{1b}[1;32m{ghp}"),
                "token=<redacted>".to_string(),
            ),
            (
                &r,
                format!(r#"{{"m":"token=\u001b[1;32m{ghp}"}}"#),
                r#"{"m":"token=<redacted>"}"#.to_string(),
            ),
            (
                &r,
                "cred LILITH-dev/\u{1b}[1;32mgithub-token".to_string(),
                "cred <redacted>".to_string(),
            ),
            (&r, "\u{1b}probeuser".to_string(), "<user>".to_string()),
            (
                &r,
                "\u{1b}S-1-5-21-111-222-333-1001".to_string(),
                "<sid>".to_string(),
            ),
            (
                &r,
                format!(r#"{{"m":"\u001b{ghp}"}}"#),
                r#"{"m":"\u001b<redacted>"}"#.to_string(),
            ),
            (&r, format!("\u{1b}[32{ghp}"), "<redacted>".to_string()),
            // A raw `ESC` before an escaped quote: the word stops at the quote, which keeps its
            // backslash (the first round steps over no `ESC \`).
            (
                &r,
                "\"{\\\"m\\\":\\\"api_key=\u{1b}\\\"}\"".to_string(),
                "\"{\\\"m\\\":\\\"api_key=<redacted>\\\"}\"".to_string(),
            ),
            (
                &r,
                "cred LILITH-dev/\u{1b}\\\"x".to_string(),
                "cred <redacted>\\\"x".to_string(),
            ),
            // A value that starts with plain SGRs is parsed after them: quoted, braced or bare.
            (
                &r,
                "api_key=\u{1b}[32m\"hunter2secret\"".to_string(),
                "api_key=\"<redacted>\"".to_string(),
            ),
            (
                &r,
                "api_key=\u{1b}[1m\u{1b}[1;32m'hunter2secret x'".to_string(),
                "api_key='<redacted>'".to_string(),
            ),
            (
                &r,
                r#"{"m":"api_key=\u001b[32m\"hunter2secret x\" next"}"#.to_string(),
                r#"{"m":"api_key=\"<redacted>\" next"}"#.to_string(),
            ),
            (
                &r,
                "api_key=\u{1b}[1;32m{\"a\":\"hunter2secret\"} next".to_string(),
                "api_key=<redacted> next".to_string(),
            ),
        ] {
            let (out, _) = redactor.text(&text);
            assert_eq!(out, want, "{text}");
        }
        // Only a plain SGR (`ESC [`, `0-9 ; :`, then `m`) is stepped over: any other sequence,
        // cut or odd, ends the word where it stopped before, so the key after it keeps its rule.
        for text in [
            "api_key=\u{1b}[32 password=\"hunter2secret x\"",
            "api_key=\u{1b}[1;32 password=\"hunter2secret x\"",
            "api_key=\u{1b}[ password=\"hunter2secret x\"",
            "api_key=\u{1b}[1&password=\"hunter2secret x\"",
            "api_key=\u{1b}[1,password=\"hunter2secret x\"",
            "api_key=\u{1b}[1)password=\"hunter2secret x\"",
            "api_key=\u{1b}[1'password=\"hunter2secret x\"",
            "api_key=\u{1b}[1;password=\"hunter2secret x\"",
            "api_key=\u{1b}[1]password=\"hunter2secret x\"",
            "{api_key=\u{1b}[1}password=\"hunter2secret x\"",
            "api_key=\u{1b}[1;password={\"a\":\"hunter2secret\"}",
            "token: \u{1b}[32 password=\"hunter2secret x\"",
            "--api-key \u{1b}[32 password=\"hunter2secret x\"",
            r#"api_key=\u001b[32 password=\"hunter2secret x\""#,
            "cred LILITH-dev/\u{1b}[1;password=\"hunter2secret x\"",
        ] {
            let (out, n) = r.text(text);
            assert!(!out.contains("hunter2secret") && n >= 2, "{text} -> {out}");
        }
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
                r#"{"m":"ab and c d"}"#,
            ),
            // A written `ESC` that no CSI follows is no `ESC`.
            (
                r#"{"m":"\u001b7saved\u001b8 \u001b(Bset"}"#,
                r#"{"m":"\u001b7saved\u001b8 \u001b(Bset"}"#,
            ),
            // A raw one: the real 2-byte sequences go; before any other byte, the `ESC` alone.
            ("\u{1b}7saved\u{1b}8 \u{1b}(Bset \u{1b}cx", "saved (Bset x"),
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

    /// A colour code inside a value is removed with nothing in its place, so the value is
    /// joined again and goes (as `grep --color` highlights part of a path); where one stood is
    /// still a word boundary.
    #[test] // spec: ARCH-121
    fn a_value_split_by_a_colour_code_is_joined_again() {
        use base64::Engine as _;
        let r = Redactor {
            profile_dir: Some(r"C:\Users\Fake\probeuser".into()),
            machine: Some("PROBEBOX-01".into()),
            names: vec![r"D:\claude\owl-dir".into()],
            ..Redactor::default()
        };
        let ab = "Ab1".repeat(12);
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r"C:\Users\Fake\x\y.png");
        for (text, want) in [
            (
                r"C:\Users\Fake\probe\u001b[0muser\x".to_string(),
                r"%USERPROFILE%\x",
            ),
            (
                "C:\\Users\\Fake\\\u{1b}[01;31m\u{1b}[Kprobe\u{1b}[m\u{1b}[Kuser\\x".to_string(),
                r"%USERPROFILE%\x",
            ),
            (r"PROBE\u001b[1mBOX-01".to_string(), "<machine>"),
            (r"D:\claude\ow\u001b[0ml-dir".to_string(), "<redacted>"),
            (format!(r"ghp_Ab1\u001b[0m{ab}"), "<redacted>"),
            (
                "pass\u{1b}[0mword=hunter2secret".to_string(),
                "password=<redacted>",
            ),
            (
                format!("lil:op\u{1b}[0men/{payload}"),
                "lil:open/<redacted>",
            ),
            // Where one stood still starts a word.
            ("x\u{1b}[1mS-1-5-21-1-2-3-4 y".to_string(), "x<sid> y"),
        ] {
            assert_eq!(r.text(&text).0, want, "{text}");
        }
    }

    /// `\e`, `\033`, `%1B` and `&#27;` are an `ESC` only with a whole CSI after them; then the
    /// sequence goes and what follows starts a word. Elsewhere (a path, `\e7`) they stay.
    #[test] // spec: ARCH-121
    fn source_spellings_of_esc_go_only_before_a_csi() {
        use base64::Engine as _;
        let r = Redactor {
            profile_dir: Some(r"C:\Users\Fake\probe".into()),
            machine: Some("PCBOX".into()),
            ..Redactor::default()
        };
        let ghp = format!("ghp_{}", "Ab1".repeat(12));
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r"C:\Users\Fake\x\y.png");
        for (text, want) in [
            (format!(r"\e[32m{ghp}"), "<redacted>".to_string()),
            (format!(r"\033[32m{ghp}"), "<redacted>".to_string()),
            (format!("%1B[1m{ghp}"), "<redacted>".to_string()),
            (
                "%1B[1mS-1-5-21-111-222-333-1001".to_string(),
                "<sid>".to_string(),
            ),
            (
                "&#27;[1mS-1-5-21-111-222-333-1001".to_string(),
                "<sid>".to_string(),
            ),
            (
                format!("%1B[0mlil:open/{payload}"),
                "lil:open/<redacted>".to_string(),
            ),
            (r"\e[1mprobe".to_string(), "<user>".to_string()),
            (r"\033[1mprobe".to_string(), "<user>".to_string()),
            ("%1B[1mprobe".to_string(), "<user>".to_string()),
            ("&#27;[1mprobe".to_string(), "<user>".to_string()),
            (r"\e[1mPCBOX".to_string(), "<machine>".to_string()),
            // No CSI after them: untouched.
            (
                r"D:\eve\x and \e7 and \033x and %1Bz".to_string(),
                r"D:\eve\x and \e7 and \033x and %1Bz".to_string(),
            ),
        ] {
            assert_eq!(r.text(&text).0, want, "{text}");
        }
        let eve = Redactor {
            profile_dir: Some(r"C:\Users\Fake\eve".into()),
            ..Redactor::default()
        };
        assert_eq!(eve.text(r"C:\Users\Fake\eve\x").0, r"%USERPROFILE%\x");
    }

    /// A path literal matches without its trailing separator or its `\\?\` (`\\?\UNC\`) prefix.
    #[test] // spec: ARCH-121
    fn path_literals_match_without_a_trailing_separator_or_a_verbatim_prefix() {
        for (name, text) in [
            (r"D:\claude\owl-dir\", r"open D:\claude\owl-dir now"),
            (r"D:/claude/owl-dir//", r"open D:\\claude\\owl-dir now"),
            (r"\\?\D:\claude\owl-dir", r"open D:/claude/owl-dir/x now"),
            (r"\\?\UNC\srv\share\owl", r"open \\srv\share\owl\x now"),
        ] {
            let r = Redactor {
                names: vec![name.into()],
                ..Redactor::default()
            };
            let (out, n) = r.text(text);
            assert!(
                out.starts_with("open <redacted>") && n == 1,
                "{name}: {text} -> {out}"
            );
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
