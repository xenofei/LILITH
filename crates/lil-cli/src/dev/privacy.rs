//! Privacy scan: nothing personal in repository files, paths or commits (QA-086, rulings R30,
//! R32, R33, R34, R35 and R37).
//!
//! The suite's repository is public, so every tracked file, every path and every commit is
//! public. `lil dev privacy-scan` looks for three things:
//!
//! 1. **User-profile paths.** A single-letter drive (`X:`, also after `\\?\` or `file:///`),
//!    the `/x/` form, or a UNC admin share (`\\host\x$`), then `Users` (any case), then one or
//!    more separators, then a non-empty name segment (the run of characters up to the next
//!    quote, backtick, separator or whitespace). The segment is fine when it is an allowlisted
//!    placeholder (ruling R30: `%USERPROFILE%`, `<user>`, `<name>`, `Fake`, `runneradmin`,
//!    `Public`, `Default`, compared without regard to case and ignoring trailing punctuation) or
//!    holds no letter or digit (`*`, `...`). URL paths (`/v1/users/me`) have no drive and don't
//!    match; a `/x/` form inside a web URL (`https://host/v/users/me`) is ignored too, but not
//!    inside a `file://` URL.
//! 2. **E-mail addresses.** Every `<local>@<domain>` address (non-ASCII local parts included)
//!    except GitHub no-reply addresses (`*@users.noreply.github.com`, `noreply@github.com`),
//!    `noreply@anthropic.com`, the `git@github.com` remote, the reserved test domains
//!    (`*.invalid`, `*.example`, `*.test`, `example.com`, `example.org`, `example.net` and their
//!    subdomains), URL userinfo (`scheme://user[:password]@host`, unless another `@` follows the
//!    host) and the scp-style remote form `user@github.com:path`. High-DPI asset names
//!    (`<name>@2x.png`) are file names, not addresses. Commit and tag identities are stricter
//!    (see [`Mode::Commits`]).
//! 3. **The private denylist**: case-insensitive literal strings, one per line (blank lines
//!    and `#` lines are ignored). Unicode format characters (zero-width spaces and the like) are
//!    dropped from both sides before matching. It comes from the first of these that is there
//!    ([`load_denylist`]): the [`names::DENYLIST_ENV`] environment variable (CI, from an Actions
//!    secret) and `%USERPROFILE%\<home>\privacy-denylist.txt`. The variable counts only when it
//!    is set and not blank; the file counts when it exists.
//!    The first source found is the denylist: one with text but no entries (only comments) is
//!    refused, not skipped. A missing or empty denylist is a refusal, never an empty pass.
//!
//! Before paths and addresses are matched, `%5C`, `%2F`, `%40` and `%3A` are decoded, so a
//! percent-encoded path or address is still seen.
//!
//! **What is printed.** A finding names its place (`file:line`, a file name, a commit's or tag's
//! message line or identity, a pushed ref name) and what was found. A denylist finding names
//! only the entry's number, counted over the entries (blank and `#` lines don't count): never
//! the entry, the matched text or the line. A path finding shows the match masked (the first
//! character of the name, then `***`); an e-mail finding shows only the first character of the
//! local part, then `***@***`, never the domain. File and ref names are printed with every
//! finding in them replaced by `***`, so a name can't repeat what it leaks. A commit or tag
//! header is named by its key only when that is a standard key git writes (`gpgsig header`);
//! any other is `header #N`, by position, since a key can hold anything (R37). Nothing from a
//! binary file's bytes is ever printed.
//!
//! **Modes** ([`Mode`]): `--range` scans what a set of commits adds (every commit's lines,
//! paths, message and identities), `--tree` scans every tracked file at one commit, and
//! `--commits` checks identities only. Each blob is judged by its own bytes, never by
//! `.gitattributes`, and nothing is decided from a prefix (ruling R34): every blob is read as
//! (lossy) UTF-8 text, line by line; a blob with a NUL byte anywhere, or one that starts with a
//! UTF-16 byte-order mark, is also read as UTF-16LE and UTF-16BE from byte 0 and from byte 1,
//! and searched byte by byte for denylist entries (ASCII-case-folded, as UTF-8 and as UTF-16LE)
//! and profile paths (in its printable ASCII and UTF-16LE runs). In a blob with a NUL, every
//! reading reports only printable, delimited addresses of 8 characters or more (R35), since
//! random bytes look like short addresses surprisingly often; such an address may end at a
//! delimiter or at `.`, `!`, `?`, `/` or `-` (R37). Findings are deduplicated per line, kind and
//! raw match (a hash of the matched text, never the masked text, so two addresses that mask
//! alike stay two findings). `--tree` and `--range` pick whole-blob reading with the same test
//! (a NUL anywhere, or a byte-order mark); in `--range` other blobs are read by their added
//! lines. A deleted file adds nothing. Symlink targets are scanned as text; a submodule entry
//! has only its path (and `.gitmodules`, a normal file). Denylist entries shorter than 5
//! characters get a warning (by number).
//!
//! **Why every commit, not only the net diff.** History is public as well: a line added in one
//! commit and removed in the next is still published by the push. `--range` therefore reads each
//! commit's own patch against its first parent (`git log -p --diff-merges=first-parent`), which
//! covers every line the net `base...head` diff adds (merge resolutions included) and also the
//! lines that were added and then removed again. Revisions reach git on stdin, never as
//! arguments.
//!
//! **Commit and tag objects** are read raw (`git cat-file --batch`), so every header is seen,
//! and nothing is skipped except a well-formed object id or type (rulings R35 and R37):
//! `author`, `committer` and `tagger` get the identity rules on the e-mail (plus the path and
//! denylist rules, since a no-reply address can still hold a path) and the line rules on the
//! rest of the line (the name, anything after `>`, continuation lines); a `mergetag` header is
//! scanned as the tag it embeds; `gpgsig`, a malformed `tree`, `parent`, `object` or `type`,
//! `encoding` and unknown headers get the line rules on the whole header, key included; every
//! message line is read, signature blocks included. A message with a NUL (stored as UTF-16)
//! also gets the UTF-16 readings.
//!
//! **Pushes** ([`scan_push`]; in CI, `--ref-name`): the pushed ref's name (path, e-mail and
//! denylist rules), the
//! annotated tag it points at (following tags of tags), and its commits. For a ref new to the
//! remote, only what the push destination currently advertises (`git ls-remote <push URL>`, see
//! [`remote_tips`]) is excluded; stale remote-tracking refs are never trusted.
//!
//! **Fail closed.** Any git error, an unknown revision or unreadable output is an error (exit 2),
//! never a pass.

use super::guard::{escape_controls, normalize_path, resolve_commit, unquote_path};
use anyhow::{Context, Result, bail};
use lil_platform::names;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The refusal when no denylist can be found.
pub const MISSING_DENYLIST: &str = "privacy denylist not found; see docs/dev/merge-guard.md";

/// The refusal when the denylist has no entries.
pub const EMPTY_DENYLIST: &str =
    "privacy denylist has no entries, so the scan can't be trusted; see docs/dev/merge-guard.md";

/// Placeholder profile names that may appear in a profile path (ruling R30), lowercase.
const PLACEHOLDERS: [&str; 7] = [
    "%userprofile%",
    "<user>",
    "<name>",
    "fake",
    "runneradmin",
    "public",
    "default",
];

// ---------------------------------------------------------------------------------------------
// The denylist.
// ---------------------------------------------------------------------------------------------

/// The private denylist: lowercase literal entries.
#[derive(Clone, PartialEq, Eq)]
pub struct Denylist {
    entries: Vec<String>,
}

/// Entries shorter than this many characters get a warning ([`Denylist::warnings`]).
pub const SHORT_ENTRY: usize = 5;

// Never print the entries, not even in a debug dump.
impl fmt::Debug for Denylist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Denylist({} entries)", self.entries.len())
    }
}

/// Unicode format characters (general category Cf: zero-width spaces and joiners, bidi marks,
/// soft hyphens, tag characters, ...). They are invisible, so they are dropped before denylist
/// matching: `a<U+200B>b` must still match `ab`.
fn is_format_char(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// Lowercases character by character, dropping format characters ([`is_format_char`]), so
/// offsets can be mapped back ([`lower_with_map`]).
fn lower(s: &str) -> String {
    s.chars()
        .filter(|c| !is_format_char(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// [`lower`], plus, for every byte of the result, the byte offset in `s` of the character it
/// came from (and one extra entry, `s.len()`).
fn lower_with_map(s: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(s.len());
    let mut map = Vec::with_capacity(s.len() + 1);
    for (i, c) in s.char_indices() {
        if is_format_char(c) {
            continue;
        }
        for l in c.to_lowercase() {
            let before = out.len();
            out.push(l);
            map.extend(std::iter::repeat_n(i, out.len() - before));
        }
    }
    map.push(s.len());
    (out, map)
}

impl Denylist {
    /// One entry per line; surrounding whitespace is trimmed; blank lines and lines starting
    /// with `#` are skipped. No entries is an error.
    pub fn parse(text: &str) -> Result<Denylist, String> {
        let entries: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(lower)
            .filter(|e| !e.is_empty())
            .collect();
        if entries.is_empty() {
            return Err(EMPTY_DENYLIST.to_string());
        }
        Ok(Denylist { entries })
    }

    /// One warning per entry shorter than [`SHORT_ENTRY`] characters. The entry itself is never
    /// shown; it still applies.
    pub fn warnings(&self) -> Vec<String> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.chars().count() < SHORT_ENTRY)
            .map(|(i, _)| {
                format!(
                    "privacy denylist entry {} is shorter than {SHORT_ENTRY} characters and may match random binary data or signature text",
                    i + 1
                )
            })
            .collect()
    }

    /// The 1-based numbers of the entries found in `text`.
    fn hits(&self, text: &str) -> Vec<usize> {
        let low = lower(text);
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| low.contains(e.as_str()))
            .map(|(i, _)| i + 1)
            .collect()
    }

    /// Entries as lowercase UTF-8 and UTF-16LE bytes, for searching binary blobs.
    fn byte_needles(&self) -> Vec<(usize, Vec<u8>, Vec<u8>)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let utf16: Vec<u8> = e.encode_utf16().flat_map(u16::to_le_bytes).collect();
                (i + 1, e.as_bytes().to_vec(), utf16)
            })
            .collect()
    }

    /// Byte ranges of `text` that match any entry.
    fn ranges(&self, text: &str) -> Vec<(usize, usize)> {
        let (low, map) = lower_with_map(text);
        let mut out = Vec::new();
        for e in &self.entries {
            for (a, m) in low.match_indices(e.as_str()) {
                let last = map[a + m.len() - 1];
                let end = last + text[last..].chars().next().map_or(0, char::len_utf8);
                out.push((map[a], end));
            }
        }
        out
    }
}

/// Loads the denylist from the first of these that is there (ruling RN4): `env` (the
/// [`names::DENYLIST_ENV`] variable), else the first of `files` that exists. The variable is
/// there only when it is set and not blank after trimming; an unset or blank one is skipped. A
/// value that isn't UTF-8 is there, and an error. The source found first is the denylist even
/// when it has no entries (a variable with only comments, an empty or comment-only file), so
/// that is refused rather than skipped. Nothing found, or no entries, is an error.
pub fn load_denylist(env: Option<OsString>, files: &[&Path]) -> Result<Denylist, String> {
    if let Some(v) = env {
        let text = v
            .into_string()
            .map_err(|_| format!("{} isn't valid UTF-8", names::DENYLIST_ENV))?;
        if !text.trim().is_empty() {
            return Denylist::parse(&text);
        }
    }
    for file in files {
        match std::fs::read(file) {
            Ok(bytes) => {
                let text = String::from_utf8(bytes)
                    .map_err(|_| "the privacy denylist isn't valid UTF-8".to_string())?;
                return Denylist::parse(&text);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("couldn't read the privacy denylist: {}", e.kind())),
        }
    }
    Err(MISSING_DENYLIST.to_string())
}

/// The denylist file under `profile`: `<home>\privacy-denylist.txt`.
fn denylist_file_in(profile: &Path) -> PathBuf {
    profile.join(names::HOME_DIR).join("privacy-denylist.txt")
}

/// The real denylist: the environment variable, then the file under `%USERPROFILE%` when that
/// is set.
pub fn real_denylist() -> Result<Denylist, String> {
    let file = std::env::var_os("USERPROFILE")
        .filter(|p| !p.is_empty())
        .map(|p| denylist_file_in(Path::new(&p)));
    let files: Vec<&Path> = file.iter().map(PathBuf::as_path).collect();
    load_denylist(std::env::var_os(names::DENYLIST_ENV), &files)
}

// ---------------------------------------------------------------------------------------------
// Detection in one line of text.
// ---------------------------------------------------------------------------------------------

/// `line` with `%5C`, `%2F`, `%40` and `%3A` (any case) decoded, so an encoded path or address
/// is still seen. Nothing else is decoded.
fn percent_decoded(line: &str) -> std::borrow::Cow<'_, str> {
    if !line.contains('%') {
        return std::borrow::Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let code = rest.get(i + 1..i + 3).map(str::to_ascii_uppercase);
        let decoded = match code.as_deref() {
            Some("5C") => Some('\\'),
            Some("2F") => Some('/'),
            Some("40") => Some('@'),
            Some("3A") => Some(':'),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[i + 3..];
            }
            None => {
                out.push('%');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

/// Walks a line forward and keeps track of the token it is in (the text after the last
/// character matching `delim`) and of the first `://` in that token. The questions "which
/// token is this match in" and "does that token hold a URL scheme" then cost amortised O(1)
/// per match, never a rescan back to the start of the line, so a line with thousands of
/// matches stays linear (ruling R37, M5). Positions passed to [`TokenCursor::advance_to`] must
/// be character boundaries and must not decrease.
struct TokenCursor<'a> {
    line: &'a str,
    delim: fn(char) -> bool,
    /// Every byte before `pos` has been walked.
    pos: usize,
    /// The start of the current token (always a character boundary, however wide the
    /// delimiter before it is).
    start: usize,
    /// The first `://` in the current token, if one starts before `pos`, and whether the
    /// text from the token's start to it is a well-formed URL scheme.
    scheme: Option<(usize, bool)>,
    /// The first `/`, `@`, `?` or `#` at least 3 bytes after `scheme`, if one was walked.
    stop: Option<usize>,
}

impl<'a> TokenCursor<'a> {
    fn new(line: &'a str, delim: fn(char) -> bool) -> TokenCursor<'a> {
        TokenCursor {
            line,
            delim,
            pos: 0,
            start: 0,
            scheme: None,
            stop: None,
        }
    }

    fn advance_to(&mut self, to: usize) {
        while self.pos < to {
            let Some(c) = self.line[self.pos..].chars().next() else {
                return;
            };
            let i = self.pos;
            self.pos += c.len_utf8();
            if (self.delim)(c) {
                self.start = self.pos;
                self.scheme = None;
                self.stop = None;
                continue;
            }
            match self.scheme {
                None if self.line[i..].starts_with("://") => {
                    let scheme = &self.line[self.start..i];
                    let ok = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                        && scheme
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
                    self.scheme = Some((i, ok));
                }
                Some((s, _))
                    if self.stop.is_none() && i >= s + 3 && matches!(c, '/' | '@' | '?' | '#') =>
                {
                    self.stop = Some(i);
                }
                _ => {}
            }
        }
    }

    /// The first `://` wholly inside the current token before `end` (the position walked to).
    fn scheme_before(&self, end: usize) -> Option<(usize, bool)> {
        self.scheme.filter(|(s, _)| s + 3 <= end)
    }
}

fn is_sep(c: char) -> bool {
    c == '\\' || c == '/'
}

/// Characters that end a profile-name segment (control characters and U+FFFD too, so a NUL or
/// an undecodable byte in a binary ends a name).
fn ends_segment(c: char) -> bool {
    is_sep(c) || c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '`' | '\u{FFFD}')
}

/// A profile path in a line: `start..seg_start` is the drive, `Users` and separators, and
/// `seg_start..seg_end` the name segment.
#[derive(Debug, Clone, Copy)]
struct PathHit {
    start: usize,
    seg_start: usize,
    seg_end: usize,
}

fn is_placeholder(seg: &str) -> bool {
    let trimmed = seg.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
    if !trimmed.chars().any(char::is_alphanumeric) {
        return true;
    }
    let low = lower(trimmed);
    PLACEHOLDERS.contains(&low.as_str())
}

/// What ends a token when deciding whether a `/x/` form sits inside a web URL.
fn ends_path_token(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | '<')
}

/// The start of the drive (`X:` or `/x`) that `Users` at byte `users_at` belongs to, or `None`.
/// `tokens` walks `line` with [`ends_path_token`].
fn drive_before(line: &str, users_at: usize, tokens: &mut TokenCursor<'_>) -> Option<usize> {
    let b = line.as_bytes();
    // At least one separator between the drive and `Users`.
    let mut k = users_at;
    while k > 0 && (b[k - 1] == b'\\' || b[k - 1] == b'/') {
        k -= 1;
    }
    if k == users_at || k == 0 {
        return None;
    }
    // `X:` with nothing alphanumeric right before the letter.
    if b[k - 1] == b':' && k >= 2 && b[k - 2].is_ascii_alphabetic() {
        let letter = k - 2;
        if letter == 0 || !b[letter - 1].is_ascii_alphanumeric() {
            return Some(letter);
        }
        return None;
    }
    // The UNC admin share `\\host\x$\Users`.
    if b[k - 1] == b'$'
        && k >= 3
        && b[k - 2].is_ascii_alphabetic()
        && (b[k - 3] == b'\\' || b[k - 3] == b'/')
    {
        return Some(k - 3);
    }
    // `/x/Users`: a single letter between two slashes, not inside a web URL (`file://` URLs
    // are paths, so they count).
    if b[k - 1].is_ascii_alphabetic() && k >= 2 && b[k - 2] == b'/' && b[k] == b'/' {
        let slash = k - 2;
        tokens.advance_to(slash);
        if let Some((pos, _)) = tokens.scheme_before(slash) {
            let is_file = pos >= tokens.start + 4 && b[pos - 4..pos].eq_ignore_ascii_case(b"file");
            if !is_file {
                return None;
            }
        }
        return Some(slash);
    }
    None
}

fn profile_paths(line: &str) -> Vec<PathHit> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut tokens = TokenCursor::new(line, ends_path_token);
    let mut i = 0;
    while i + 5 <= b.len() {
        if !b[i..i + 5].eq_ignore_ascii_case(b"users") {
            i += 1;
            continue;
        }
        let users_at = i;
        i += 5;
        let Some(start) = drive_before(line, users_at, &mut tokens) else {
            continue;
        };
        // One or more separators after `Users`, then the segment.
        let mut j = users_at + 5;
        while j < b.len() && (b[j] == b'\\' || b[j] == b'/') {
            j += 1;
        }
        if j == users_at + 5 {
            continue;
        }
        let seg_end = line[j..].find(ends_segment).map_or(line.len(), |n| j + n);
        if seg_end == j || is_placeholder(&line[j..seg_end]) {
            continue;
        }
        out.push(PathHit {
            start,
            seg_start: j,
            seg_end,
        });
        i = seg_end;
    }
    out
}

/// An e-mail address in a line: `local_start..at` is the local part, `at + 1..end` the domain.
#[derive(Debug, Clone, Copy)]
struct MailHit {
    local_start: usize,
    at: usize,
    end: usize,
}

/// Bytes of a local part: ASCII letters, digits, `._%+-`, and any non-ASCII character (every
/// byte of a multi-byte UTF-8 character is 0x80 or above, so whole characters are taken).
fn is_local_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'%' | b'+' | b'-') || c >= 0x80
}

/// `2x`, `3x`, ...: the scale suffix of a high-DPI asset name (`icon@2x.png`).
fn is_scale_suffix(label: &str) -> bool {
    label.len() >= 2
        && label[..label.len() - 1].bytes().all(|b| b.is_ascii_digit())
        && label.ends_with(['x', 'X'])
}

/// Every syntactically plausible `<local>@<domain>.<tld>` in `line`, allowed or not.
fn all_emails(line: &str) -> Vec<MailHit> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    for (at, _) in line.match_indices('@') {
        // Local parts are at most 64 characters (RFC 5321); the cap keeps a long run of
        // undecodable bytes from making this quadratic.
        let mut s = at;
        while s > 0 && at - s < 256 && is_local_char(b[s - 1]) {
            s -= 1;
        }
        while !line.is_char_boundary(s) {
            s += 1;
        }
        while s < at && b[s] == b'.' {
            s += 1;
        }
        if s == at {
            continue;
        }
        let mut e = at + 1;
        while e < b.len()
            && e - at <= 255
            && (b[e].is_ascii_alphanumeric() || b[e] == b'.' || b[e] == b'-')
        {
            e += 1;
        }
        while e > at + 1 && (b[e - 1] == b'.' || b[e - 1] == b'-') {
            e -= 1;
        }
        // A hyphen glued to the end (`<local>@<domain>.com-x`): the address ends at the hyphen
        // when the letters before it make a top-level domain (ruling R37, M2).
        if let Some(d) = line[at + 1..e].rfind('.').map(|d| at + 1 + d)
            && let Some(h) = line[d + 1..e].find('-')
            && line[d + 1..d + 1 + h]
                .bytes()
                .all(|c| c.is_ascii_alphabetic())
        {
            e = d + 1 + h;
        }
        let domain = &line[at + 1..e];
        let labels: Vec<&str> = domain.split('.').collect();
        let tld = labels.last().copied().unwrap_or("");
        if labels.len() < 2
            || labels.iter().any(|l| l.is_empty())
            || tld.len() < 2
            || !tld.chars().all(|c| c.is_ascii_alphabetic())
            || (labels.len() == 2 && is_scale_suffix(labels[0]))
        {
            continue;
        }
        out.push(MailHit {
            local_start: s,
            at,
            end: e,
        });
    }
    out
}

fn is_test_domain(domain: &str) -> bool {
    let reserved_tld = ["invalid", "example", "test"]
        .iter()
        .any(|t| domain == *t || domain.ends_with(&format!(".{t}")));
    let reserved = ["example.com", "example.org", "example.net"]
        .iter()
        .any(|d| domain == *d || domain.ends_with(&format!(".{d}")));
    reserved_tld || reserved
}

/// `true` when the address is one of the GitHub no-reply forms commits may carry.
fn is_github_no_reply(addr: &str) -> bool {
    let a = addr.to_ascii_lowercase();
    a == "noreply@github.com"
        || a.strip_suffix("@users.noreply.github.com")
            .is_some_and(|l| !l.is_empty() && !l.contains('@'))
}

/// What ends a token when deciding whether an address is URL userinfo.
fn ends_mail_token(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | '<' | '[')
}

/// `true` when `local@domain` in its line may stay in the repository. `tokens` walks `line`
/// with [`ends_mail_token`]; addresses must be asked about in order.
fn email_allowed(line: &str, m: MailHit, tokens: &mut TokenCursor<'_>) -> bool {
    // `scheme://<user>@<domain>@host`: the real userinfo hides an address; never allowed.
    if line.as_bytes().get(m.end) == Some(&b'@') {
        return false;
    }
    let local = line[m.local_start..m.at].to_ascii_lowercase();
    let domain = line[m.at + 1..m.end].to_ascii_lowercase();
    if domain == "users.noreply.github.com"
        || is_test_domain(&domain)
        || matches!(
            (local.as_str(), domain.as_str()),
            ("noreply", "github.com") | ("noreply", "anthropic.com") | ("git", "github.com")
        )
    {
        return true;
    }
    // URL userinfo: `scheme://user[:password]@host`. The token before the local part must hold
    // a `://` with a well-formed scheme before it and no `/`, `@`, `?` or `#` after it.
    tokens.advance_to(m.local_start);
    if let Some((_, scheme_ok)) = tokens.scheme_before(m.local_start)
        && scheme_ok
        && tokens.stop.is_none()
    {
        return true;
    }
    // The scp-style remote `user@github.com:owner/repo` (and `user@github.com/...`).
    domain == "github.com" && matches!(line.as_bytes().get(m.end), Some(b':' | b'/'))
}

fn emails(line: &str) -> Vec<MailHit> {
    let mut tokens = TokenCursor::new(line, ends_mail_token);
    all_emails(line)
        .into_iter()
        .filter(|m| !email_allowed(line, *m, &mut tokens))
        .collect()
}

/// The first character of `s` (escaped), then `***`.
fn first_then_stars(s: &str) -> String {
    let first: String = s.chars().take(1).collect();
    format!("{}***", escape_controls(&first))
}

fn mask_path(line: &str, h: PathHit) -> String {
    let more = if h.seg_end < line.len() { "\\…" } else { "" };
    format!(
        "{}{}{more}",
        escape_controls(&line[h.start..h.seg_start]),
        first_then_stars(&line[h.seg_start..h.seg_end])
    )
}

/// The local part's first character, then `***@***`: the domain is never shown.
fn mask_address(addr: &str) -> String {
    match addr.rsplit_once('@') {
        Some((local, _)) => format!("{}@***", first_then_stars(local)),
        None => first_then_stars(addr),
    }
}

fn mask_email(line: &str, m: MailHit) -> String {
    mask_address(&line[m.local_start..m.end])
}

/// What a finding is. The strings are already masked.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum What {
    /// A user-profile path (masked).
    ProfilePath(String),
    /// An e-mail address (masked).
    Email(String),
    /// A denylist entry, by its 1-based number.
    Denylist(usize),
    /// A commit author e-mail that isn't a GitHub no-reply address (masked).
    AuthorEmail(String),
    /// A commit committer e-mail that isn't a GitHub no-reply address (masked).
    CommitterEmail(String),
    /// A `Co-Authored-By:` or `Signed-off-by:` e-mail that isn't allowed (masked).
    TrailerEmail(String),
    /// An annotated tag's tagger e-mail that isn't a GitHub no-reply address (masked).
    TaggerEmail(String),
    /// A user-profile path inside a binary file (never shown).
    BinaryProfilePath,
}

impl fmt::Display for What {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            What::ProfilePath(m) => write!(f, "a user-profile path ({m}); write %USERPROFILE%"),
            What::Email(m) => write!(f, "an e-mail address ({m})"),
            What::Denylist(n) => write!(f, "matches private denylist entry {n}"),
            What::AuthorEmail(m) => write!(
                f,
                "author e-mail {m} isn't a GitHub no-reply address (set user.email to your <id>+<login>@users.noreply.github.com address)"
            ),
            What::CommitterEmail(m) => write!(
                f,
                "committer e-mail {m} isn't a GitHub no-reply address (set user.email to your <id>+<login>@users.noreply.github.com address)"
            ),
            What::TrailerEmail(m) => write!(
                f,
                "trailer e-mail {m} isn't noreply@anthropic.com or a GitHub no-reply address"
            ),
            What::TaggerEmail(m) => write!(
                f,
                "tagger e-mail {m} isn't a GitHub no-reply address (set user.email to your <id>+<login>@users.noreply.github.com address)"
            ),
            What::BinaryProfilePath => write!(
                f,
                "a user-profile path in binary content (not shown); write %USERPROFILE%"
            ),
        }
    }
}

/// One finding: where, and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Already escaped and redacted.
    pub place: String,
    pub what: What,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.place, self.what)
    }
}

/// Profile paths, e-mail addresses and denylist entries in one line (ordered by kind).
pub fn scan_line(line: &str, deny: &Denylist) -> Vec<What> {
    scan_line_with(line, deny, true)
}

fn scan_line_with(line: &str, deny: &Denylist, with_emails: bool) -> Vec<What> {
    let rule = if with_emails {
        EmailRule::Normal
    } else {
        EmailRule::Off
    };
    scan_line_rule(line, deny, rule)
}

/// Which addresses a line reading reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmailRule {
    /// None (a trailer line, judged by the trailer rule instead).
    Off,
    /// Every address that isn't allowed.
    Normal,
    /// Only addresses whose local part is printable (no control character, no U+FFFD, no
    /// non-ASCII whitespace; ruling R35), that are at least [`MIN_BINARY_ADDRESS`] characters
    /// long, that start at a delimiter ([`delimits`]) or the line's start, and that end at one
    /// ([`ends_address`], which also takes `.`, `!`, `?`, `/` and `-`) or the line's end. For
    /// readings of a blob with a NUL, where random bytes would otherwise look like addresses.
    Printable,
}

/// The shortest address reported from a blob with a NUL. Shorter printable matches turn up in
/// random bytes (see the fix-round-3 report).
const MIN_BINARY_ADDRESS: usize = 8;

/// A character that can stand right before or after an address embedded in binary data: a
/// control character (a NUL terminator, a length byte), whitespace, or punctuation that
/// brackets or separates text.
fn delimits(c: char) -> bool {
    c.is_control() || c.is_whitespace() || "<>()[]{}\"'`,;:=|".contains(c)
}

/// A character that can stand right after an address embedded in binary data: anything that
/// [`delimits`], or sentence and path punctuation (`.`, `!`, `?`, `/`, `-`; ruling R37, M2).
/// The domain never ends in `.` or `-` (they are trimmed), so these always follow it.
fn ends_address(c: char) -> bool {
    delimits(c) || matches!(c, '.' | '!' | '?' | '/' | '-')
}

/// A character a real address's local part can hold, seen in binary data.
fn printable_local_char(c: char) -> bool {
    c.is_ascii_graphic()
        || (!c.is_ascii() && !c.is_control() && !c.is_whitespace() && c != '\u{FFFD}')
}

fn scan_line_rule(line: &str, deny: &Denylist, rule: EmailRule) -> Vec<What> {
    scan_line_hits(line, deny, rule)
        .into_iter()
        .map(|h| h.what)
        .collect()
}

/// A hash of the raw text a finding matched. Findings are deduplicated by it (with the line and
/// kind), never by the masked text, so two different addresses that mask alike stay two
/// findings (ruling R37, M3). The raw text itself is never kept or printed.
fn raw_key(kind: u8, raw: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    kind.hash(&mut h);
    raw.hash(&mut h);
    h.finish()
}

/// One match in a line: what it is (masked) and [`raw_key`] of what it matched.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LineHit {
    what: What,
    raw: u64,
}

/// [`scan_line_rule`], with the raw identity of every match.
fn scan_line_hits(line: &str, deny: &Denylist, rule: EmailRule) -> Vec<LineHit> {
    let decoded = percent_decoded(line);
    let line = decoded.as_ref();
    let mut out: Vec<LineHit> = profile_paths(line)
        .into_iter()
        .map(|h| LineHit {
            what: What::ProfilePath(mask_path(line, h)),
            raw: raw_key(0, &line[h.start..h.seg_end]),
        })
        .collect();
    if rule != EmailRule::Off {
        out.extend(
            emails(line)
                .into_iter()
                .filter(|m| {
                    rule == EmailRule::Normal
                        || (line[m.local_start..m.at].chars().all(printable_local_char)
                            && line[m.local_start..m.end].chars().count() >= MIN_BINARY_ADDRESS
                            && line[..m.local_start]
                                .chars()
                                .next_back()
                                .is_none_or(delimits)
                            && line[m.end..].chars().next().is_none_or(ends_address))
                })
                .map(|m| LineHit {
                    what: What::Email(mask_email(line, m)),
                    raw: raw_key(1, &line[m.local_start..m.end]),
                }),
        );
    }
    out.extend(deny.hits(line).into_iter().map(|n| LineHit {
        what: What::Denylist(n),
        raw: raw_key(2, &n.to_string()),
    }));
    out
}

/// `text` (a path) escaped for printing, with every finding in it replaced by `***`.
fn redact(text: &str, deny: &Denylist) -> String {
    let decoded = percent_decoded(text);
    let text = decoded.as_ref();
    let mut hidden = vec![false; text.len()];
    let mut hide = |a: usize, b: usize| hidden[a..b].iter_mut().for_each(|h| *h = true);
    for (a, b) in deny.ranges(text) {
        hide(a, b);
    }
    for h in profile_paths(text) {
        hide(h.seg_start, h.seg_end);
    }
    for m in emails(text) {
        hide(m.local_start, m.end);
    }
    let mut out = String::new();
    let mut in_hidden = false;
    for (i, c) in text.char_indices() {
        if hidden[i] {
            if !in_hidden {
                out.push_str("***");
            }
            in_hidden = true;
        } else {
            in_hidden = false;
            out.push_str(&escape_controls(&c.to_string()));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Git.
// ---------------------------------------------------------------------------------------------

fn git_command(repo: &Path, args: &[&str]) -> Command {
    let mut cmd = super::root::git_command();
    cmd.arg("-C")
        .arg(repo)
        .args(["-c", "core.quotepath=off"])
        .args(args);
    cmd
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = git_command(repo, args)
        .output()
        .context("could not run git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// [`git`], with `input` written to git's stdin (from a thread, so a large output can't
/// deadlock against it).
fn git_stdin(repo: &Path, args: &[&str], input: String) -> Result<Vec<u8>> {
    let mut child = git_command(repo, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run git")?;
    let mut stdin = child.stdin.take().context("git: no stdin")?;
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().context("could not run git")?;
    let _ = writer.join();
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// `true` when `sha` names a commit (or a tag that peels to one) in `repo`.
pub fn has_commit(repo: &Path, sha: &str) -> bool {
    !sha.starts_with('-')
        && super::root::git_command()
            .arg("-C")
            .arg(repo)
            .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// Every object id the remote currently advertises (`git ls-remote <remote>`), tags and their
/// peeled commits included. `remote` should be the push URL git passed to the hook, so that a
/// `pushurl` or `pushInsteadOf` is honoured. A failure is an error: the caller refuses rather
/// than guess. Names or URLs starting with `-`, and the `ext::` transport (which runs a
/// command), are refused.
pub fn remote_tips(repo: &Path, remote: &str) -> Result<Vec<String>> {
    if remote.is_empty() || remote.starts_with('-') {
        bail!("no usable remote name or URL to ask what the remote has");
    }
    if remote.to_ascii_lowercase().starts_with("ext::") {
        bail!("the ext:: transport runs a command, so it isn't asked");
    }
    let out = git(repo, &["ls-remote", remote])?;
    let mut shas: Vec<String> = String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|s| is_sha(s))
        .map(str::to_string)
        .collect();
    shas.sort();
    shas.dedup();
    Ok(shas)
}

/// Decodes `bytes` as UTF-16 (lossily; a byte-order mark becomes a format character, which the
/// denylist ignores).
fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| {
            if little_endian {
                u16::from_le_bytes(*p)
            } else {
                u16::from_be_bytes(*p)
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// The printable runs of a blob, read both as ASCII and as UTF-16LE in both alignments (at
/// least 4 characters each).
fn printable_runs(bytes: &[u8]) -> Vec<String> {
    let printable = |b: u8| (0x20..0x7F).contains(&b);
    let mut out = Vec::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if cur.len() >= 4 {
            out.push(std::mem::take(cur));
        }
        cur.clear();
    };
    let mut cur = String::new();
    for &b in bytes {
        if printable(b) {
            cur.push(b as char);
        } else {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    for start in [0, 1] {
        for pair in bytes[start.min(bytes.len())..].as_chunks::<2>().0 {
            if printable(pair[0]) && pair[1] == 0 {
                cur.push(pair[0] as char);
            } else {
                flush(&mut cur, &mut out);
            }
        }
        flush(&mut cur, &mut out);
    }
    out
}

/// The byte search of a blob with a NUL, without any of its bytes in the result: the denylist
/// entries (ASCII-folded blob against each entry's UTF-8 and UTF-16LE bytes) and user-profile
/// paths (in the blob's printable ASCII and UTF-16LE runs, with the usual placeholder rules).
fn binary_findings(bytes: &[u8], deny: &Denylist) -> Vec<What> {
    let folded = bytes.to_ascii_lowercase();
    let mut out = Vec::new();
    if printable_runs(bytes)
        .iter()
        .any(|run| !profile_paths(&percent_decoded(run)).is_empty())
    {
        out.push(What::BinaryProfilePath);
    }
    for (n, utf8, utf16) in deny.byte_needles() {
        if contains_bytes(&folded, &utf8) || contains_bytes(&folded, &utf16) {
            out.push(What::Denylist(n));
        }
    }
    out
}

/// One finding in a whole blob: the line (in the reading named by `reading`), or none for the
/// byte search.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BlobHit {
    line: Option<usize>,
    reading: Option<&'static str>,
    what: What,
}

impl BlobHit {
    /// `path:3`, `path:3 (UTF-16LE, commit abc)`, `path (binary, commit abc)`. `suffix` starts
    /// with `, ` or is empty.
    fn place(&self, shown_path: &str, suffix: &str) -> String {
        match (self.line, self.reading) {
            (Some(n), None) if suffix.is_empty() => format!("{shown_path}:{n}"),
            (Some(n), None) => format!("{shown_path}:{n} ({})", &suffix[2..]),
            (Some(n), Some(r)) => format!("{shown_path}:{n} ({r}{suffix})"),
            (None, _) => format!("{shown_path} (binary{suffix})"),
        }
    }
}

/// The UTF-16 readings of bytes that may be UTF-16: little- and big-endian, each from byte 0
/// and from byte 1.
const UTF16_READINGS: [(&str, bool, usize); 4] = [
    ("UTF-16LE", true, 0),
    ("UTF-16LE", true, 1),
    ("UTF-16BE", false, 0),
    ("UTF-16BE", false, 1),
];

/// `true` for a blob that is read whole, in every reading ([`blob_findings`]): one with a NUL
/// byte anywhere, or one that starts with a UTF-16 byte-order mark (`FF FE` or `FE FF`; UTF-16
/// text of CJK or Cyrillic characters has no NUL). `--tree` and `--range` both decide with this
/// one test (ruling R37, I2), so a BOM file is decoded in range mode as well.
fn read_whole(bytes: &[u8]) -> bool {
    bytes.contains(&0) || bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF])
}

/// Every finding in a whole blob (rulings R34, R35 and R37; nothing is decided from a prefix):
///
/// - every blob is read as (lossy) UTF-8 text, line by line;
/// - a blob that [`read_whole`] picks (a NUL byte anywhere, or a UTF-16 byte-order mark) is also
///   read as UTF-16LE and UTF-16BE, each from byte 0 and from byte 1, line by line, and gets
///   the binary byte search ([`binary_findings`]);
/// - in a blob with a NUL, every reading reports only printable addresses
///   ([`EmailRule::Printable`]): random bytes look like addresses surprisingly often.
///
/// Findings are deduplicated by what they matched ([`raw_key`]), never by the masked text: one
/// per line, kind and raw match in the UTF-8 reading; one per kind and raw match across the four
/// UTF-16 readings (they overlap, and the first, with its line, is kept); and the byte search
/// only adds what neither reading found.
fn blob_findings(bytes: &[u8], deny: &Denylist) -> Vec<BlobHit> {
    let rule = if bytes.contains(&0) {
        EmailRule::Printable
    } else {
        EmailRule::Normal
    };
    let mut out: Vec<BlobHit> = Vec::new();
    let mut seen: HashSet<(usize, LineHit)> = HashSet::new();
    let text = String::from_utf8_lossy(bytes);
    for (n, line) in text.split('\n').enumerate() {
        for hit in scan_line_hits(line, deny, rule) {
            let what = hit.what.clone();
            if seen.insert((n + 1, hit)) {
                out.push(BlobHit {
                    line: Some(n + 1),
                    reading: None,
                    what,
                });
            }
        }
    }
    if !read_whole(bytes) {
        return out;
    }
    let mut wide_seen: HashSet<LineHit> = HashSet::new();
    for (reading, le, start) in UTF16_READINGS {
        let decoded = decode_utf16(&bytes[start.min(bytes.len())..], le);
        for (n, line) in decoded.split('\n').enumerate() {
            for hit in scan_line_hits(line, deny, rule) {
                let what = hit.what.clone();
                if wide_seen.insert(hit) {
                    out.push(BlobHit {
                        line: Some(n + 1),
                        reading: Some(reading),
                        what,
                    });
                }
            }
        }
    }
    let found: HashSet<What> = out.iter().map(|h| h.what.clone()).collect();
    let any_path = found.iter().any(|w| matches!(w, What::ProfilePath(_)));
    for what in binary_findings(bytes, deny) {
        let already = match &what {
            What::BinaryProfilePath => any_path,
            w => found.contains(w),
        };
        if !already {
            out.push(BlobHit {
                line: None,
                reading: None,
                what,
            });
        }
    }
    out
}

/// Reads every object in `shas` through one `git cat-file --batch` and hands each to `each`.
fn each_object(repo: &Path, shas: &[String], mut each: impl FnMut(&str, &[u8])) -> Result<()> {
    if shas.is_empty() {
        return Ok(());
    }
    let mut child = super::root::git_command()
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run git cat-file")?;
    let mut stdin = child.stdin.take().context("git cat-file: no stdin")?;
    let request: String = shas.iter().map(|s| format!("{s}\n")).collect();
    let writer = std::thread::spawn(move || stdin.write_all(request.as_bytes()));
    let mut reader = BufReader::new(child.stdout.take().context("git cat-file: no stdout")?);
    let mut result = Ok(());
    for sha in shas {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            result = Err(anyhow::anyhow!("git cat-file stopped before {sha}"));
            break;
        }
        let fields: Vec<&str> = header.split_whitespace().collect();
        let [got, _kind, size] = fields[..] else {
            result = Err(anyhow::anyhow!(
                "git cat-file: object {sha} can't be read ({})",
                header.trim()
            ));
            break;
        };
        if got != sha {
            result = Err(anyhow::anyhow!("git cat-file answered {got} for {sha}"));
            break;
        }
        let size: usize = size.parse().context("git cat-file: bad size")?;
        let mut buf = vec![0; size + 1];
        reader.read_exact(&mut buf)?;
        buf.pop();
        each(sha, &buf);
    }
    drop(reader);
    let _ = writer.join();
    let status = child.wait()?;
    result?;
    if !status.success() {
        bail!("git cat-file --batch failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Modes.
// ---------------------------------------------------------------------------------------------

/// What to scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// `<base>..<head>`, or a single revision for all of its history: every commit's added
    /// lines (against its first parent), its new and renamed paths, its headers, message and
    /// identities.
    Range(String),
    /// Every tracked file's path and content at a revision.
    Tree(String),
    /// Identities only, for `<base>..<head>` or a single revision's history: author and
    /// committer e-mails must be `*@users.noreply.github.com` or `noreply@github.com`;
    /// `Co-Authored-By:` and `Signed-off-by:` e-mails must be `noreply@anthropic.com` or
    /// `*@users.noreply.github.com`; tagger e-mails embedded in `mergetag` headers follow the
    /// author rule; author and committer names are checked against the denylist.
    Commits(String),
}

/// One ref from git's pre-push list that needs a scan. `remote_ref` is the name the remote
/// will show (`refs/heads/x`); `head` is the pushed object (a commit, or an annotated tag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushScan {
    /// An existing remote ref whose old commit this repository has: the commits in
    /// `base..head`.
    Range {
        remote_ref: String,
        base: String,
        head: String,
    },
    /// A new remote ref, or one whose old commit isn't here: the commits reachable from `head`
    /// that none of `exclude` (what the push destination currently has, from `git ls-remote`,
    /// limited to objects this repository has) reaches. Remote-tracking refs are never trusted:
    /// they can be stale.
    NewBranch {
        remote_ref: String,
        head: String,
        exclude: Vec<String>,
    },
}

/// `a..b` into git revisions (`b`, `^a`), both pinned to SHAs; a single revision is all of its
/// history. `a...b` and empty sides are refused.
fn range_revs(repo: &Path, spec: &str) -> Result<Vec<String>> {
    if spec.contains("...") {
        bail!("`{spec}`: give a range as <base>..<head>");
    }
    match spec.split_once("..") {
        Some((base, head)) => {
            if base.is_empty() || head.is_empty() {
                bail!("`{spec}`: give a range as <base>..<head>");
            }
            let base = resolve_commit(repo, base)?;
            let head = resolve_commit(repo, head)?;
            Ok(vec![head, format!("^{base}")])
        }
        None => Ok(vec![resolve_commit(repo, spec)?]),
    }
}

pub fn scan(repo: &Path, mode: &Mode, deny: &Denylist) -> Result<Vec<Finding>> {
    match mode {
        Mode::Tree(rev) => scan_tree(repo, rev, deny),
        Mode::Range(spec) => scan_history(repo, &range_revs(repo, spec)?, deny, true),
        Mode::Commits(spec) => scan_history(repo, &range_revs(repo, spec)?, deny, false),
    }
}

/// Findings in a pushed ref's name (`refs/heads/x`): path, e-mail and denylist rules. The name
/// is shown with every finding in it replaced by `***`.
pub fn ref_name_findings(name: &str, deny: &Denylist) -> Vec<Finding> {
    scan_line(name, deny)
        .into_iter()
        .map(|what| Finding {
            place: format!("pushed ref name {}", redact(name, deny)),
            what,
        })
        .collect()
}

/// The scan for one pushed ref: its name, the annotated tag it points at (if any), and its
/// commits ([`PushScan`]).
pub fn scan_push(repo: &Path, push: &PushScan, deny: &Denylist) -> Result<Vec<Finding>> {
    let (remote_ref, head) = match push {
        PushScan::Range {
            remote_ref, head, ..
        }
        | PushScan::NewBranch {
            remote_ref, head, ..
        } => (remote_ref, head),
    };
    let mut out = ref_name_findings(remote_ref, deny);
    tag_findings(repo, head, deny, &mut out)?;
    let head_commit = resolve_commit(repo, head)?;
    let revs = match push {
        PushScan::Range { base, .. } => {
            vec![head_commit, format!("^{}", resolve_commit(repo, base)?)]
        }
        PushScan::NewBranch { exclude, .. } => {
            let mut revs = vec![head_commit];
            for sha in exclude {
                if !is_sha(sha) {
                    bail!("not an object id: `{}`", escape_controls(sha));
                }
                revs.push(format!("^{sha}"));
            }
            revs
        }
    };
    out.extend(scan_history(repo, &revs, deny, true)?);
    Ok(out)
}

/// One header of a git object: its key (the text before the first space), its value (the text
/// after it, continuation lines joined with `\n`), and the whole header as written (the key,
/// the value and the continuation lines, joined with `\n`), which is what the line rules read
/// (ruling R37: a key is scanned like any other text).
struct Header {
    key: String,
    value: String,
    whole: String,
}

/// The headers of a git object (`commit` or `tag`) and its message. A header's continuation
/// lines (starting with a space) are joined to it with `\n`, without the space; a continuation
/// line with no header before it is a header of its own (with an empty key), so it is scanned
/// too.
fn parse_object(text: &str) -> (Vec<Header>, String) {
    let (head, message) = text.split_once("\n\n").unwrap_or((text, ""));
    let mut headers: Vec<Header> = Vec::new();
    for line in head.split('\n') {
        if let Some(cont) = line.strip_prefix(' ')
            && let Some(last) = headers.last_mut()
        {
            for part in [&mut last.value, &mut last.whole] {
                part.push('\n');
                part.push_str(cont);
            }
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        headers.push(Header {
            key: key.to_string(),
            value: value.to_string(),
            whole: line.to_string(),
        });
    }
    (headers, message.to_string())
}

/// Header keys git writes. Only these are ever printed; any other key could be anything (an
/// address, a path, a denylist entry), so it is named by its position instead (ruling R37).
const STANDARD_HEADERS: [&str; 12] = [
    "tree",
    "parent",
    "author",
    "committer",
    "tagger",
    "encoding",
    "gpgsig",
    "gpgsig-sha256",
    "mergetag",
    "object",
    "type",
    "tag",
];

/// How a finding names the header at 1-based position `index`: `gpgsig header` for a standard
/// key ([`STANDARD_HEADERS`]), `header #4` for any other. The key's own text is never printed.
fn header_label(key: &str, index: usize) -> String {
    match STANDARD_HEADERS.iter().find(|k| **k == key) {
        Some(k) => format!("{k} header"),
        None => format!("header #{index}"),
    }
}

/// `Name <email> <time> <zone>` into the e-mail and the rest of the line (the name and anything
/// after `>`, joined by a space). Without `<` the whole line is the rest.
fn split_ident(ident: &str) -> (String, String) {
    match ident.split_once('<') {
        Some((name, rest)) => match rest.split_once('>') {
            Some((email, after)) => (email.to_string(), format!("{name} {after}")),
            None => (rest.to_string(), name.to_string()),
        },
        None => (String::new(), ident.to_string()),
    }
}

/// Which identity an ident header is, for its finding.
#[derive(Clone, Copy)]
enum Role {
    Author,
    Committer,
    Tagger,
}

/// An ident header (`author`, `committer`, `tagger`): the e-mail must be a GitHub no-reply
/// address and must not hold a denylist entry; the rest of the line (the name, anything after
/// `>`, continuation lines) gets the line rules (ruling R35).
fn ident_findings(value: &str, role: Role, place: &str, deny: &Denylist, out: &mut Vec<Finding>) {
    let (email, rest) = split_ident(value);
    if !is_github_no_reply(&email) {
        let m = mask_address(&email);
        out.push(Finding {
            place: place.to_string(),
            what: match role {
                Role::Author => What::AuthorEmail(m),
                Role::Committer => What::CommitterEmail(m),
                Role::Tagger => What::TaggerEmail(m),
            },
        });
    }
    // The address itself gets the path and denylist rules: a no-reply address can still hold
    // a profile path or an entry (a path before `@users.noreply.github.com`; R37, M1).
    for what in scan_line_rule(&email, deny, EmailRule::Off) {
        out.push(Finding {
            place: format!("{place} e-mail"),
            what,
        });
    }
    for line in rest.split('\n') {
        for what in scan_line(line, deny) {
            out.push(Finding {
                place: format!("{place} line"),
                what,
            });
        }
    }
}

/// The whole header (key, value and continuation lines), each line with the line rules. The
/// place names the header by [`header_label`], never by its key's text.
fn header_findings(
    header: &Header,
    index: usize,
    place: &str,
    deny: &Denylist,
    out: &mut Vec<Finding>,
) {
    for line in header.whole.split('\n') {
        for what in scan_line(line, deny) {
            out.push(Finding {
                place: format!("{place} {}", header_label(&header.key, index)),
                what,
            });
        }
    }
}

/// `true` when `value` is a well-formed object id as git writes it (40 or 64 lowercase hex
/// digits), the only thing `tree`, `parent` and `object` headers may hold unscanned.
fn is_object_id(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64)
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `true` when `value` is a well-formed object type, the only thing a `type` header may hold
/// unscanned.
fn is_object_type(value: &str) -> bool {
    matches!(value, "commit" | "tree" | "blob" | "tag")
}

/// Every message line (nothing is ever skipped: a signature block is scanned too, ruling R35),
/// with its 1-based number.
fn message_lines(message: &str) -> impl Iterator<Item = (usize, &str)> {
    message.lines().enumerate().map(|(n, l)| (n + 1, l))
}

/// Findings in one tag object's text: the tagger (identity rules), every header except a
/// well-formed `object` or `type` (line rules), and the message (line rules). `place` names the
/// tag (`tag abc1234`, or `commit abc1234 mergetag`). The tagger is always checked; the rest
/// only with `content`. Returns the tagged object when it is itself a tag.
fn tag_text_findings(
    text: &str,
    place: &str,
    deny: &Denylist,
    content: bool,
    out: &mut Vec<Finding>,
) -> Option<String> {
    let (headers, message) = parse_object(text);
    let mut object = None;
    let mut is_tag = false;
    for (i, h) in headers.iter().enumerate() {
        let value = &h.value;
        match h.key.as_str() {
            "object" if is_object_id(value) => object = Some(value.clone()),
            "type" if is_object_type(value) => is_tag = value == "tag",
            "tagger" => ident_findings(value, Role::Tagger, &format!("{place} tagger"), deny, out),
            _ if content => header_findings(h, i + 1, place, deny, out),
            _ => {}
        }
    }
    if content {
        message_findings(&message, place, deny, true, false, out);
    }
    if is_tag { object } else { None }
}

/// Findings in the annotated tag `sha` (and any tag it points at). Nothing for a commit.
fn tag_findings(repo: &Path, sha: &str, deny: &Denylist, out: &mut Vec<Finding>) -> Result<()> {
    let mut cur = sha.to_string();
    for _ in 0..16 {
        if !is_sha(&cur) {
            bail!("not an object id: `{}`", escape_controls(&cur));
        }
        let kind = git(repo, &["cat-file", "-t", &cur])?;
        if String::from_utf8_lossy(&kind).trim() != "tag" {
            return Ok(());
        }
        let raw = git(repo, &["cat-file", "tag", &cur])?;
        let place = format!("tag {}", short(&cur));
        match tag_text_findings(&String::from_utf8_lossy(&raw), &place, deny, true, out) {
            Some(next) => cur = next,
            None => return Ok(()),
        }
    }
    bail!("tag {} nests too deeply", short(sha))
}

fn path_findings(path: &str, suffix: &str, deny: &Denylist, out: &mut Vec<Finding>) {
    for what in scan_line(path, deny) {
        out.push(Finding {
            place: format!("{} (file name{suffix})", redact(path, deny)),
            what,
        });
    }
}

fn scan_tree(repo: &Path, rev: &str, deny: &Denylist) -> Result<Vec<Finding>> {
    let commit = resolve_commit(repo, rev)?;
    let listing = git(repo, &["ls-tree", "-r", "-z", "--full-tree", &commit, "--"])?;
    let listing = String::from_utf8_lossy(&listing);
    let mut out = Vec::new();
    let mut blobs: Vec<String> = Vec::new();
    let mut path_of: HashMap<String, Vec<String>> = HashMap::new();
    for entry in listing.split('\0').filter(|e| !e.is_empty()) {
        let (meta, path) = entry.split_once('\t').with_context(|| {
            format!("git ls-tree: unexpected entry `{}`", escape_controls(entry))
        })?;
        let fields: Vec<&str> = meta.split_whitespace().collect();
        let [_mode, kind, sha] = fields[..] else {
            bail!("git ls-tree: unexpected entry `{}`", escape_controls(meta));
        };
        let path = normalize_path(path);
        path_findings(&path, "", deny, &mut out);
        // Submodule entries (`commit`) have no blob here; their path is all there is.
        if kind == "blob" {
            let paths = path_of.entry(sha.to_string()).or_default();
            if paths.is_empty() {
                blobs.push(sha.to_string());
            }
            paths.push(path);
        }
    }
    let mut content = Vec::new();
    each_object(repo, &blobs, |sha, bytes| {
        let hits = blob_findings(bytes, deny);
        for path in &path_of[sha] {
            let shown = redact(path, deny);
            for hit in &hits {
                content.push(Finding {
                    place: hit.place(&shown, ""),
                    what: hit.what.clone(),
                });
            }
        }
    })?;
    out.extend(content);
    Ok(out)
}

fn is_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `git log` over `revs`, which are passed on stdin (`--stdin`), never as arguments.
fn git_log(repo: &Path, revs: &[String], extra: &[&str]) -> Result<Vec<u8>> {
    let mut args = vec![
        "log",
        "--no-color",
        "--no-show-signature",
        "--no-notes",
        "--encoding=UTF-8",
    ];
    args.extend_from_slice(extra);
    args.extend(["--stdin", "--"]);
    let input: String = revs.iter().map(|r| format!("{r}\n")).collect();
    git_stdin(repo, &args, input)
}

/// The commit ids in `revs` (`git rev-list --stdin`), newest first.
fn commit_ids(repo: &Path, revs: &[String]) -> Result<Vec<String>> {
    let input: String = revs.iter().map(|r| format!("{r}\n")).collect();
    let out = git_stdin(repo, &["rev-list", "--stdin", "--"], input)?;
    String::from_utf8_lossy(&out)
        .lines()
        .map(|l| {
            let sha = l.trim();
            if is_sha(sha) {
                Ok(sha.to_string())
            } else {
                bail!("git rev-list: unexpected line `{}`", escape_controls(sha))
            }
        })
        .collect()
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// `true` for a `Co-Authored-By:` or `Signed-off-by:` line.
fn is_trailer(line: &str) -> bool {
    let l = line.trim_start().to_ascii_lowercase();
    ["co-authored-by", "signed-off-by"].iter().any(|t| {
        l.strip_prefix(t)
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
    })
}

fn trailer_email_allowed(addr: &str) -> bool {
    let a = addr.to_ascii_lowercase();
    a == "noreply@anthropic.com" || (is_github_no_reply(&a) && a != "noreply@github.com")
}

/// Findings in one raw commit object (`git cat-file commit`), with `content` (`--range`) or
/// identities only (`--commits`). Nothing is skipped except well-formed object ids (ruling
/// R35):
///
/// - `author` and `committer`: the identity rules ([`ident_findings`]), always;
/// - `mergetag`: the embedded tag, as a tag ([`tag_text_findings`]; its tagger always gets the
///   identity rules);
/// - `tree` and `parent` holding a well-formed id: skipped; anything else in them, `gpgsig`,
///   `gpgsig-sha256`, `encoding` and every unknown header: the line rules, with `content`;
/// - the message: trailers always get the trailer rule; with `content`, every line gets the
///   line rules (trailer addresses are judged by the trailer rule only).
fn commit_findings(sha: &str, raw: &str, deny: &Denylist, content: bool, out: &mut Vec<Finding>) {
    let id = short(sha);
    let place = format!("commit {id}");
    let (headers, message) = parse_object(raw);
    for (i, h) in headers.iter().enumerate() {
        let value = &h.value;
        match h.key.as_str() {
            "tree" | "parent" if is_object_id(value) => {}
            "author" => ident_findings(value, Role::Author, &format!("{place} author"), deny, out),
            "committer" => ident_findings(
                value,
                Role::Committer,
                &format!("{place} committer"),
                deny,
                out,
            ),
            "mergetag" => {
                tag_text_findings(value, &format!("{place} mergetag"), deny, content, out);
            }
            _ if content => header_findings(h, i + 1, &place, deny, out),
            _ => {}
        }
    }
    message_findings(&message, &place, deny, content, true, out);
}

/// Findings in a commit or tag message. Every line is read; nothing is skipped (R35). A
/// message with a NUL (one stored as UTF-16, `encoding UTF-16LE`) is also read in the four
/// UTF-16 readings, as a blob is ([`UTF16_READINGS`]), with printable-only addresses there;
/// those findings are kept once per kind. With `trailers`, `Co-Authored-By:` and
/// `Signed-off-by:` addresses get the trailer rule (always); with `content`, every line gets
/// the line rules (a trailer's addresses only the trailer rule).
fn message_findings(
    message: &str,
    place: &str,
    deny: &Denylist,
    content: bool,
    trailers: bool,
    out: &mut Vec<Finding>,
) {
    let mut readings: Vec<(Option<&'static str>, String)> = vec![(None, message.to_string())];
    if message.contains('\0') {
        let bytes = message.as_bytes();
        for (reading, le, start) in UTF16_READINGS {
            readings.push((
                Some(reading),
                decode_utf16(&bytes[start.min(bytes.len())..], le),
            ));
        }
    }
    let mut wide_seen: HashSet<LineHit> = HashSet::new();
    for (reading, text) in &readings {
        let label = reading.map(|r| format!(" ({r})")).unwrap_or_default();
        let rule = if reading.is_some() {
            EmailRule::Printable
        } else {
            EmailRule::Normal
        };
        for (n, line) in message_lines(text) {
            let trailer = trailers && is_trailer(line);
            let mut found = Vec::new();
            if trailer {
                for m in all_emails(line) {
                    let addr = &line[m.local_start..m.end];
                    if !trailer_email_allowed(addr) {
                        found.push(LineHit {
                            what: What::TrailerEmail(mask_address(addr)),
                            raw: raw_key(3, addr),
                        });
                    }
                }
            }
            if content {
                let rule = if trailer { EmailRule::Off } else { rule };
                found.extend(scan_line_hits(line, deny, rule));
            }
            for hit in found {
                // The UTF-16 readings overlap: one finding per kind and raw match (R37, M3).
                if reading.is_some() && !wide_seen.insert(hit.clone()) {
                    continue;
                }
                out.push(Finding {
                    place: format!("{place} message line {n}{label}"),
                    what: hit.what,
                });
            }
        }
    }
}

/// A file a commit's patch touches: the commit, the new path and its blob, if it has one.
struct RawEntry {
    commit: String,
    path: String,
    blob: Option<String>,
}

/// Parses one `--raw` line (without `-z`): `:<mode> <mode> <sha> <sha> <status>\t<path>[\t<path>]`.
fn parse_raw_line(commit: &str, line: &str) -> Result<Option<RawEntry>> {
    let (meta, paths) = line
        .split_once('\t')
        .with_context(|| format!("git log --raw: unexpected line `{}`", escape_controls(line)))?;
    let fields: Vec<&str> = meta.trim_start_matches(':').split_whitespace().collect();
    let [_old_mode, new_mode, _old_sha, new_sha, status] = fields[..] else {
        bail!("git log --raw: unexpected line `{}`", escape_controls(line));
    };
    if status.starts_with('D') {
        return Ok(None);
    }
    let path = paths
        .split('\t')
        .next_back()
        .map(unquote_path)
        .map(|p| normalize_path(&p))
        .context("git log --raw: no path")?;
    // Submodule entries (160000) have no blob in this repository.
    let blob = (new_mode != "160000" && is_sha(new_sha)).then(|| new_sha.to_string());
    Ok(Some(RawEntry {
        commit: commit.to_string(),
        path,
        blob,
    }))
}

/// `+c` from `@@ -a,b +c,d @@`.
fn hunk_new_start(line: &str) -> Result<usize> {
    line.split_whitespace()
        .find_map(|t| t.strip_prefix('+'))
        .and_then(|t| t.split(',').next())
        .and_then(|n| n.parse().ok())
        .with_context(|| {
            format!(
                "git log -p: unexpected hunk header `{}`",
                escape_controls(line)
            )
        })
}

/// One added line of a commit's patch.
struct Added {
    commit: String,
    path: String,
    blob: Option<String>,
    line_no: usize,
    text: String,
}

/// Where the patch parser is.
enum PatchFile {
    /// Before a file's `+++` header (or between files).
    Unknown,
    /// A file that is deleted (`+++ /dev/null`): its hunks add nothing.
    Deleted,
    /// A file with a new side.
    Path(String),
}

/// The raw entries and added lines of every commit in `revs`, each against its first parent.
fn patches(repo: &Path, revs: &[String]) -> Result<(Vec<RawEntry>, Vec<Added>)> {
    let raw = git_log(
        repo,
        revs,
        &[
            "--format=%x01%H",
            "--raw",
            "-p",
            "--no-abbrev",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
            "--text",
            "-U0",
            "-M",
            "--root",
            "--diff-merges=first-parent",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ],
    )?;
    let text = String::from_utf8_lossy(&raw);
    let mut entries: Vec<RawEntry> = Vec::new();
    let mut added = Vec::new();
    let mut commit = String::new();
    let mut blob_of: HashMap<String, Option<String>> = HashMap::new();
    let mut file = PatchFile::Unknown;
    let mut next_line: Option<usize> = None;
    for line in text.split('\n') {
        if let Some(n) = next_line.as_mut() {
            if let Some(content) = line.strip_prefix('+') {
                if let PatchFile::Path(path) = &file {
                    added.push(Added {
                        commit: commit.clone(),
                        blob: blob_of.get(path).cloned().flatten(),
                        path: path.clone(),
                        line_no: *n,
                        text: content.to_string(),
                    });
                }
                *n += 1;
                continue;
            }
            if line.starts_with('-') || line.starts_with('\\') {
                continue;
            }
            next_line = None;
        }
        if let Some(sha) = line.strip_prefix('\u{1}') {
            if !is_sha(sha) {
                bail!("git log: unexpected commit id `{}`", escape_controls(sha));
            }
            commit = sha.to_string();
            blob_of.clear();
            file = PatchFile::Unknown;
        } else if line.starts_with(':') {
            if let Some(e) = parse_raw_line(&commit, line)? {
                blob_of.insert(e.path.clone(), e.blob.clone());
                entries.push(e);
            }
        } else if line.starts_with("diff --git ") {
            file = PatchFile::Unknown;
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            let p = unquote_path(rest);
            file = if p == "/dev/null" {
                PatchFile::Deleted
            } else {
                match p.strip_prefix("b/") {
                    Some(path) => PatchFile::Path(normalize_path(path)),
                    None => bail!("git log -p: unexpected header `{}`", escape_controls(line)),
                }
            };
        } else if line.starts_with("@@") {
            if let PatchFile::Unknown = file {
                bail!("git log -p: a hunk without a file");
            }
            next_line = Some(hunk_new_start(line)?);
        }
    }
    Ok((entries, added))
}

fn scan_history(
    repo: &Path,
    revs: &[String],
    deny: &Denylist,
    content: bool,
) -> Result<Vec<Finding>> {
    let mut out = Vec::new();
    // Each commit object as git stores it, so no header is missed.
    let ids = commit_ids(repo, revs)?;
    let mut raw: HashMap<String, String> = HashMap::new();
    each_object(repo, &ids, |sha, bytes| {
        raw.insert(sha.to_string(), String::from_utf8_lossy(bytes).into_owned());
    })?;
    for sha in &ids {
        commit_findings(sha, &raw[sha], deny, content, &mut out);
    }
    if !content {
        return Ok(out);
    }
    let (entries, added) = patches(repo, revs)?;
    for e in &entries {
        path_findings(
            &e.path,
            &format!(", commit {}", short(&e.commit)),
            deny,
            &mut out,
        );
    }
    // A blob with a NUL anywhere or a UTF-16 byte-order mark ([`read_whole`], the same test as
    // `--tree`'s) is scanned whole, in every reading ([`blob_findings`]); its patch lines add
    // nothing more. Every other blob is plain text: its added lines are read.
    let mut blobs: Vec<String> = entries.iter().filter_map(|e| e.blob.clone()).collect();
    blobs.sort();
    blobs.dedup();
    let mut whole: HashMap<String, Vec<BlobHit>> = HashMap::new();
    each_object(repo, &blobs, |sha, bytes| {
        if read_whole(bytes) {
            whole.insert(sha.to_string(), blob_findings(bytes, deny));
        }
    })?;
    for e in &entries {
        if let Some(hits) = e.blob.as_ref().and_then(|b| whole.get(b)) {
            let shown = redact(&e.path, deny);
            let suffix = format!(", commit {}", short(&e.commit));
            for hit in hits {
                out.push(Finding {
                    place: hit.place(&shown, &suffix),
                    what: hit.what.clone(),
                });
            }
        }
    }
    for a in &added {
        if a.blob.as_ref().is_some_and(|b| whole.contains_key(b)) {
            continue;
        }
        let shown = redact(&a.path, deny);
        for what in scan_line(&a.text, deny) {
            out.push(Finding {
                place: format!("{shown}:{} (commit {})", a.line_no, short(&a.commit)),
                what,
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// CLI glue.
// ---------------------------------------------------------------------------------------------

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `lil dev privacy-scan`: exit 0 when nothing is found, 1 with findings, 2 when it couldn't
/// scan (no denylist, a git error). `ref_name` (CI's pushed ref) is scanned too.
pub fn run(mode: Mode, ref_name: Option<&str>) -> i32 {
    let deny = match real_denylist() {
        Ok(d) => d,
        Err(m) => {
            eprintln!("lil dev privacy-scan: {m}");
            return 2;
        }
    };
    for w in deny.warnings() {
        eprintln!("lil dev privacy-scan: warning: {w}");
    }
    let repo = match super::root::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lil dev privacy-scan: couldn't scan, so nothing is passed: {e}");
            return 2;
        }
    };
    let scanned = scan(&repo, &mode, &deny).map(|found| {
        let mut all = ref_name.map_or_else(Vec::new, |n| ref_name_findings(n, &deny));
        all.extend(found);
        all
    });
    match scanned {
        Err(e) => {
            eprintln!(
                "lil dev privacy-scan: couldn't scan, so nothing is passed: {}",
                one_line(&format!("{e:#}"))
            );
            2
        }
        Ok(found) if found.is_empty() => {
            println!("Privacy scan (QA-086): nothing personal found.");
            0
        }
        Ok(found) => {
            for f in &found {
                println!("{f}");
            }
            println!(
                "Privacy scan (QA-086): {} finding(s). Nothing personal may enter the repository: remove or replace each one.",
                found.len()
            );
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev::spec_tests::workspace_root;
    use std::path::PathBuf;
    use std::process::Command;

    // Test vectors are assembled at run time, so this file itself stays clean under
    // `lil dev privacy-scan --tree` (its own source is public too).

    /// The profile folder's name, split so the source never spells a profile path.
    fn u() -> String {
        ["Us", "ers"].concat()
    }

    /// `local` + at-sign + `domain`.
    fn mail(local: &str, domain: &str) -> String {
        format!("{local}{}{domain}", '@')
    }

    fn deny(text: &str) -> Denylist {
        Denylist::parse(text).expect("test denylist")
    }

    fn none() -> Denylist {
        deny("zz-unused-entry-zz")
    }

    fn paths_in(line: &str) -> usize {
        scan_line(line, &none())
            .iter()
            .filter(|w| matches!(w, What::ProfilePath(_)))
            .count()
    }

    fn emails_in(line: &str) -> usize {
        scan_line(line, &none())
            .iter()
            .filter(|w| matches!(w, What::Email(_)))
            .count()
    }

    // ---- user-profile paths ----------------------------------------------------------------

    #[test] // spec: QA-086
    fn every_profile_path_form_is_blocked() {
        let u = u();
        for line in [
            format!(r"C:\{u}\jdoe\AppData"),
            format!(r"C:\\{u}\\jdoe\\AppData"),
            format!(r"C:\\\\{u}\\\\jdoe"),
            format!("C:/{u}/jdoe/AppData"),
            format!("/c/{u}/jdoe/.cargo"),
            format!(r"\\?\C:\{u}\jdoe\x"),
            format!(r"d:\{}\jdoe", u.to_uppercase()),
            format!(r#"let p = "E:\{u}\jdoe";"#),
            format!("/mnt/c/{u}/jdoe/x"),
            format!("file:///C:/{u}/jdoe/x"),
            format!(r"C:\{u}\jdoe"),
            format!(r"C:\{u}\jdoe."),
        ] {
            assert_eq!(paths_in(&line), 1, "{line}");
        }
    }

    #[test] // spec: QA-086
    fn placeholder_profile_names_are_allowed() {
        let u = u();
        for seg in [
            "%USERPROFILE%",
            "%userprofile%",
            "<user>",
            "<name>",
            "<NAME>",
            "Fake",
            "FAKE",
            "fake",
            "runneradmin",
            "Public",
            "Default",
            "DEFAULT",
            "Fake.",
            "Default,",
        ] {
            for line in [
                format!(r"C:\{u}\{seg}\AppData"),
                format!("C:/{u}/{seg}"),
                format!("/c/{u}/{seg}/x"),
                format!(r"C:\\{u}\\{seg}\\x"),
            ] {
                assert_eq!(paths_in(&line), 0, "{line}");
            }
        }
        assert_eq!(
            paths_in(&format!(r"%USERPROFILE%\{}\reviewer", names::HOME_DIR)),
            0
        );
    }

    #[test] // spec: QA-086
    fn url_paths_and_partial_forms_are_not_profile_paths() {
        let u = u();
        let lu = u.to_lowercase();
        for line in [
            format!("GET /v1/{lu}/me/balance"),
            format!("https://api.example.com/v/{lu}/jdoe"),
            format!("https://example.com/{lu}/jdoe"),
            format!(r"C:\{u}"),
            format!(r"C:\{u}\"),
            format!(r#""C:\{u}\""#),
            format!(r"C:\{u}\ x"),
            format!(r"ab:\{u}\jdoe"),
            format!(r"C:\My{u}\jdoe"),
            format!(r"C:\{u}x\jdoe"),
            format!(r"C:\{u}\...\x"),
            format!(r"C:\{u}\*\AppData"),
        ] {
            assert_eq!(paths_in(&line), 0, "{line}");
        }
    }

    #[test] // spec: QA-086
    fn claude_settings_pattern_fragments_are_not_profile_paths() {
        // The quoted pattern strings in crates/lil-testkit/tests/claude_settings_patterns.rs.
        let lu = u().to_lowercase();
        let line =
            format!(r#"    for bad in [":\\\\{lu}", ":/{lu}/", "/c/{lu}/", "appdata", "@"] {{"#);
        assert_eq!(paths_in(&line), 0, "{line}");
        // And the file itself, as committed.
        let text = std::fs::read_to_string(
            workspace_root().join("crates/lil-testkit/tests/claude_settings_patterns.rs"),
        )
        .unwrap();
        for l in text.lines() {
            assert!(scan_line(l, &none()).is_empty(), "{l}");
        }
    }

    #[test] // spec: QA-086
    fn plan_task_17_placeholder_forms_are_allowed() {
        let u = u();
        let line = format!(
            r"Any `X:\{u}\<name>`, also written as `/x/{u}/<name>`, `C:\\{u}\\<name>` or `C:/{u}/<name>`, where"
        );
        assert_eq!(paths_in(&line), 0, "{line}");
    }

    // ---- e-mail addresses --------------------------------------------------------------------

    #[test] // spec: QA-086
    fn ordinary_email_addresses_are_blocked() {
        for line in [
            mail("jdoe", "uni.edu"),
            mail("x", "gmail.com"),
            format!("Author: J Doe <{}>", mail("j.doe+tag", "mail.school.edu")),
            format!("mailto:{}", mail("x", "gmail.com")),
            format!("({})", mail("someone", "company.co.uk")),
        ] {
            assert_eq!(emails_in(&line), 1, "{line}");
        }
    }

    #[test] // spec: QA-086
    fn no_reply_test_domain_and_remote_forms_are_allowed() {
        for line in [
            mail("12345+someone", "users.noreply.github.com"),
            mail("noreply", "github.com"),
            mail("noreply", "anthropic.com"),
            format!(
                "Co-Authored-By: Claude <{}>",
                mail("noreply", "anthropic.com")
            ),
            mail("git", "github.com"),
            mail("GIT", "GitHub.com"),
            mail("x", "host.invalid"),
            mail("x", "a.example"),
            mail("x", "a.test"),
            mail("x", "example.com"),
            mail("x", "sub.example.org"),
            mail("x", "example.net"),
            format!("https://{}/o/r", mail("token", "github.com")),
            format!("https://user:{}/x", mail("pw", "host.com")),
            format!("ssh://{}:22/o/r", mail("git", "github.com")),
            format!("{}:o/r", mail("evil", "github.com")),
            "uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1".into(),
            "tool: cargo-nextest@0.9.146,cargo-deny@0.20.2".into(),
            "react@18.3.1".into(),
            "a @ b.com".into(),
        ] {
            assert_eq!(emails_in(&line), 0, "{line}");
        }
    }

    #[test] // spec: QA-086
    fn merge_rs_test_urls_are_allowed() {
        let text =
            std::fs::read_to_string(workspace_root().join("crates/lil-cli/src/dev/merge.rs"))
                .unwrap();
        for l in text.lines() {
            assert!(scan_line(l, &none()).is_empty(), "{l}");
        }
    }

    #[test] // spec: QA-086
    fn findings_mask_the_name_and_the_local_part() {
        let u = u();
        let line = format!(r"see C:\{u}\jdoe\AppData and {}", mail("jdoe", "uni.edu"));
        let found = scan_line(&line, &none());
        assert_eq!(found.len(), 2, "{found:?}");
        for w in &found {
            let shown = format!(
                "{}",
                Finding {
                    place: "f.txt:1".into(),
                    what: w.clone()
                }
            );
            assert!(!shown.contains("jdoe"), "{shown}");
            assert!(!shown.contains("uni"), "{shown}");
            assert!(shown.contains("j***"), "{shown}");
        }
    }

    // ---- the denylist ------------------------------------------------------------------------

    #[test] // spec: QA-086
    fn denylist_parses_entries_and_numbers_them() {
        let d = deny("# comment\n\n  Quokka-Zebra-77  \r\nsecond\n");
        let hits = scan_line("a quokka-zebra-77 b SECOND", &d);
        assert_eq!(hits, vec![What::Denylist(1), What::Denylist(2)]);
    }

    #[test] // spec: QA-086
    fn missing_or_empty_denylist_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("privacy-denylist.txt");
        assert_eq!(
            load_denylist(None, &[&missing]),
            Err(MISSING_DENYLIST.to_string())
        );
        assert_eq!(load_denylist(None, &[]), Err(MISSING_DENYLIST.to_string()));
        // Present but with no entries: refused too.
        std::fs::write(&missing, "# nothing yet\n\n").unwrap();
        assert!(load_denylist(None, &[&missing]).is_err());
        for env in ["", "\n# c\n"] {
            assert!(load_denylist(Some(env.into()), &[]).is_err());
        }
        // The environment variable comes first (CI), so the file isn't needed then.
        let tmp2 = tempfile::tempdir().unwrap();
        let absent = tmp2.path().join("nope.txt");
        assert!(load_denylist(Some("entry".into()), &[&absent]).is_ok());
        // And a file with an entry loads.
        std::fs::write(&missing, "# c\nentry\n").unwrap();
        assert!(load_denylist(None, &[&missing]).is_ok());
    }

    /// The 1-based entry numbers `d` finds in `text`; entries are never shown.
    fn hits_of(d: &Result<Denylist, String>, text: &str) -> Vec<usize> {
        d.as_ref().expect("a denylist").hits(text)
    }

    /// `<profile>\<home>\privacy-denylist.txt` with `text`.
    fn write_denylist(profile: &Path, text: &str) {
        let f = denylist_file_in(profile);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, text).unwrap();
    }

    fn load_from(env: Option<&str>, profile: &Path) -> Result<Denylist, String> {
        let file = denylist_file_in(profile);
        load_denylist(env.map(Into::into), &[&file])
    }

    #[test] // spec: QA-086
    fn denylist_file_is_in_the_home_folder() {
        let profile = Path::new(r"C:\Users\Fake");
        assert_eq!(
            denylist_file_in(profile),
            profile.join(names::HOME_DIR).join("privacy-denylist.txt")
        );
    }

    #[test] // spec: QA-086
    fn denylist_env_comes_before_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_denylist(tmp.path(), "file-entry\n");
        let d = load_from(Some("env-entry"), tmp.path());
        assert_eq!(hits_of(&d, "env-entry"), vec![1]);
        assert!(hits_of(&d, "file-entry").is_empty());
        // A value that isn't UTF-8 is an error naming the variable.
        use std::os::windows::ffi::OsStringExt;
        let e = load_denylist(Some(OsString::from_wide(&[0xD800])), &[]).unwrap_err();
        assert!(e.contains(names::DENYLIST_ENV), "{e}");
    }

    #[test] // spec: QA-086
    fn denylist_file_is_read_without_the_env() {
        let tmp = tempfile::tempdir().unwrap();
        write_denylist(tmp.path(), "file-entry\n");
        let d = load_from(None, tmp.path());
        assert_eq!(hits_of(&d, "file-entry"), vec![1]);
        // The file that exists is the denylist: an empty one is refused.
        write_denylist(tmp.path(), "# nothing yet\n");
        assert_eq!(
            load_from(None, tmp.path()).err(),
            Some(EMPTY_DENYLIST.to_string())
        );
    }

    /// RN4: an unset or blank variable is not a source; the file is tried.
    #[test] // spec: QA-086
    fn blank_env_falls_through_to_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_denylist(tmp.path(), "file-entry\n");
        for blank in ["", " ", "\n", " \r\n\t \n"] {
            let d = load_from(Some(blank), tmp.path());
            assert_eq!(hits_of(&d, "file-entry"), vec![1], "{blank:?}");
        }
        // A blank variable and no file: still a refusal.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            load_from(Some(" "), empty.path()).err(),
            Some(MISSING_DENYLIST.to_string())
        );
    }

    #[test] // spec: QA-086
    fn comment_only_env_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        write_denylist(tmp.path(), "file-entry\n");
        for comments in ["# nothing yet", "\n# a\n\n# b\n", " \n# c\n"] {
            assert_eq!(
                load_from(Some(comments), tmp.path()).err(),
                Some(EMPTY_DENYLIST.to_string()),
                "{comments:?}"
            );
        }
    }

    #[test] // spec: QA-086
    fn no_denylist_anywhere_is_still_a_refusal() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            load_from(None, tmp.path()).err(),
            Some(MISSING_DENYLIST.to_string())
        );
        // An empty home folder holds no denylist.
        std::fs::create_dir_all(tmp.path().join(names::HOME_DIR)).unwrap();
        assert_eq!(
            load_from(None, tmp.path()).err(),
            Some(MISSING_DENYLIST.to_string())
        );
        // A folder where the file should be is an error, never a pass.
        std::fs::create_dir_all(denylist_file_in(tmp.path())).unwrap();
        assert!(load_from(None, tmp.path()).is_err());
    }

    // ---- git fixtures ------------------------------------------------------------------------

    struct Repo {
        _tmp: tempfile::TempDir,
        dir: PathBuf,
        empty: PathBuf,
    }

    const OK_EMAIL: &str = "12345+fixture@users.noreply.github.com";

    impl Repo {
        fn new() -> Repo {
            let tmp = tempfile::tempdir().unwrap();
            let empty = tmp.path().join("empty-gitconfig");
            std::fs::write(&empty, "").unwrap();
            let dir = tmp.path().join("repo");
            std::fs::create_dir_all(&dir).unwrap();
            let r = Repo {
                _tmp: tmp,
                dir,
                empty,
            };
            r.git(&["-c", "init.defaultBranch=main", "init", "-q"]);
            r.git(&["config", "user.name", "Fixture"]);
            r.git(&["config", "user.email", OK_EMAIL]);
            r.git(&["config", "commit.gpgsign", "false"]);
            r.git(&["config", "core.autocrlf", "false"]);
            r
        }

        fn cmd(&self, args: &[&str]) -> Command {
            let mut c = Command::new("git");
            c.arg("-C")
                .arg(&self.dir)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", &self.empty)
                .env_remove("GIT_DIR")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_AUTHOR_NAME")
                .env_remove("GIT_AUTHOR_EMAIL")
                .env_remove("GIT_COMMITTER_NAME")
                .env_remove("GIT_COMMITTER_EMAIL");
            c
        }

        fn git(&self, args: &[&str]) -> String {
            let out = self.cmd(args).output().expect("git must be installed");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        fn write(&self, name: &str, bytes: &[u8]) {
            let p = self.dir.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, bytes).unwrap();
        }

        /// Commits everything with `message` and returns the sha.
        fn commit(&self, message: &str) -> String {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "--allow-empty", "-m", message]);
            self.git(&["rev-parse", "HEAD"])
        }

        /// Commits everything as author `Someone <author>` and committer `Committer <committer>`.
        fn commit_as(&self, author: &str, committer: &str, message: &str) -> String {
            self.git(&["add", "-A"]);
            let out = self
                .cmd(&[
                    "commit",
                    "-q",
                    "--allow-empty",
                    "--author",
                    &format!("Someone <{author}>"),
                    "-m",
                    message,
                ])
                .env("GIT_COMMITTER_NAME", "Committer")
                .env("GIT_COMMITTER_EMAIL", committer)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            self.git(&["rev-parse", "HEAD"])
        }
    }

    fn rendered(found: &[Finding]) -> String {
        found
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ---- --tree ------------------------------------------------------------------------------

    #[test] // spec: QA-086
    fn tree_mode_finds_nothing_on_a_clean_fixture() {
        let r = Repo::new();
        let u = u();
        r.write(
            "README.md",
            format!(
                "Keys live in %USERPROFILE%\\.lilith.\nExample: C:\\{u}\\<user>\\x\nContact {}\n",
                mail("noreply", "github.com")
            )
            .as_bytes(),
        );
        r.write("src/lib.rs", b"fn main() {}\n");
        r.write("assets/icon.bin", b"\x00\x01binary\x00");
        r.commit("initial");
        let found = scan(&r.dir, &Mode::Tree("HEAD".into()), &deny("quokka-zebra-77")).unwrap();
        assert!(found.is_empty(), "{}", rendered(&found));
    }

    #[test] // spec: QA-086
    fn tree_mode_reports_file_and_line() {
        let r = Repo::new();
        let u = u();
        r.write(
            "docs/notes.md",
            format!(
                "one\ntwo C:\\{u}\\jdoe\\x\nthree {}\n",
                mail("jdoe", "uni.edu")
            )
            .as_bytes(),
        );
        r.commit("initial");
        let found = scan(&r.dir, &Mode::Tree("HEAD".into()), &none()).unwrap();
        let places: Vec<&str> = found.iter().map(|f| f.place.as_str()).collect();
        assert_eq!(places, ["docs/notes.md:2", "docs/notes.md:3"], "{found:?}");
    }

    #[test] // spec: QA-086
    fn denylist_hit_is_reported_without_the_text() {
        let r = Repo::new();
        r.write("notes.txt", b"one\ntwo\nthe Quokka-Zebra-77 line\n");
        r.write("quokka-zebra-77-dir/a.txt", b"clean\n");
        r.commit("initial");
        let d = deny("# private\nalpha-entry\nQUOKKA-ZEBRA-77\n");
        let found = scan(&r.dir, &Mode::Tree("HEAD".into()), &d).unwrap();
        let text = rendered(&found);
        assert_eq!(found.len(), 2, "{text}");
        assert!(text.contains("notes.txt:3"), "{text}");
        assert!(text.contains("denylist entry 2"), "{text}");
        let lower = text.to_lowercase();
        assert!(!lower.contains("quokka"), "{text}");
        assert!(!lower.contains("zebra"), "{text}");
        assert!(
            !lower.contains("line"),
            "the matching line is never shown: {text}"
        );
        assert!(!lower.contains("alpha"), "{text}");
    }

    #[test] // spec: QA-086
    fn binary_file_path_and_text_inside_are_scanned() {
        // Every blob is read as text too (R34), so an address inside a binary is found.
        let r = Repo::new();
        let base = r.commit("base");
        let mut content = b"\x00\x01".to_vec();
        content.extend_from_slice(format!("by {}", mail("jdoe", "uni.edu")).as_bytes());
        r.write("quokka-zebra-77.bin", &content);
        r.write("plain.bin", &content);
        r.commit("add binaries");
        let d = deny("quokka-zebra-77");
        for mode in [
            Mode::Tree("HEAD".into()),
            Mode::Range(format!("{base}..HEAD")),
        ] {
            let found = scan(&r.dir, &mode, &d).unwrap();
            let text = rendered(&found);
            assert_eq!(found.len(), 3, "{mode:?}: {text}");
            assert_eq!(
                found
                    .iter()
                    .filter(|f| f.place.contains("file name"))
                    .map(|f| &f.what)
                    .collect::<Vec<_>>(),
                [&What::Denylist(1)],
                "{text}"
            );
            assert_eq!(
                found
                    .iter()
                    .filter(|f| matches!(f.what, What::Email(_)))
                    .count(),
                2,
                "{text}"
            );
            assert!(!text.contains("jdoe") && !text.contains("quokka"), "{text}");
        }
    }

    // ---- --range -----------------------------------------------------------------------------

    #[test] // spec: QA-086
    fn range_mode_scans_added_lines_paths_messages_and_identities() {
        let r = Repo::new();
        let u = u();
        // Already on the base: not this range's business.
        r.write("old.txt", mail("old", "gmail.com").as_bytes());
        let base = r.commit("base");
        r.write(
            "new.txt",
            format!("ok\n{}\n", mail("jdoe", "uni.edu")).as_bytes(),
        );
        r.commit_as(
            &mail("dev", "gmail.com"),
            "noreply@github.com",
            &format!("add new\n\nbuilt in C:\\{u}\\jdoe\\src\n"),
        );
        let found = scan(&r.dir, &Mode::Range(format!("{base}..HEAD")), &none()).unwrap();
        let text = rendered(&found);
        assert_eq!(found.len(), 3, "{text}");
        assert!(
            found.iter().any(|f| f.place.starts_with("new.txt:2")),
            "{text}"
        );
        assert!(
            found.iter().any(|f| matches!(f.what, What::AuthorEmail(_))),
            "{text}"
        );
        assert!(
            found.iter().any(|f| f.place.contains("message line 3")),
            "{text}"
        );
        assert!(!text.contains("old.txt"), "{text}");
        assert!(!text.contains("jdoe") && !text.contains("dev@"), "{text}");
    }

    #[test] // spec: QA-086
    fn range_mode_sees_lines_added_then_removed() {
        let r = Repo::new();
        let base = r.commit("base");
        r.write("a.txt", format!("{}\n", mail("x", "gmail.com")).as_bytes());
        r.commit("add");
        r.write("a.txt", b"clean\n");
        r.commit("remove");
        let found = scan(&r.dir, &Mode::Range(format!("{base}..HEAD")), &none()).unwrap();
        assert_eq!(found.len(), 1, "{}", rendered(&found));
    }

    #[test] // spec: QA-086
    fn range_mode_scans_renamed_paths() {
        let r = Repo::new();
        r.write("a.txt", b"same\n");
        let base = r.commit("base");
        r.git(&["mv", "a.txt", "quokka-zebra-77.txt"]);
        r.commit("rename");
        let found = scan(
            &r.dir,
            &Mode::Range(format!("{base}..HEAD")),
            &deny("quokka-zebra-77"),
        )
        .unwrap();
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        assert!(!found[0].place.contains("quokka"), "{}", found[0]);
    }

    #[test] // spec: QA-086
    fn bad_revisions_are_errors_not_passes() {
        let r = Repo::new();
        r.commit("base");
        for mode in [
            Mode::Range("nope..HEAD".into()),
            Mode::Range("HEAD...HEAD".into()),
            Mode::Range("-x..HEAD".into()),
            Mode::Range("HEAD..".into()),
            Mode::Tree("nope".into()),
            Mode::Commits("nope".into()),
        ] {
            assert!(scan(&r.dir, &mode, &none()).is_err(), "{mode:?}");
        }
    }

    // ---- identities --------------------------------------------------------------------------

    #[test] // spec: QA-086
    fn identity_check_flags_a_bad_author_and_trailer() {
        let r = Repo::new();
        let base = r.commit("base");
        r.write(
            "x.txt",
            format!("{}\n", mail("content", "gmail.com")).as_bytes(),
        );
        r.commit_as(
            &mail("dev", "gmail.com"),
            "noreply@github.com",
            &format!(
                "work\n\nCo-Authored-By: Claude <noreply@anthropic.com>\nSigned-off-by: Me <{OK_EMAIL}>\nCo-authored-by: Pal <{}>\n",
                mail("pal", "uni.edu")
            ),
        );
        r.commit_as(OK_EMAIL, &mail("ci", "corp.com"), "second");
        let found = scan(&r.dir, &Mode::Commits(format!("{base}..HEAD")), &none()).unwrap();
        let text = rendered(&found);
        let kinds: Vec<&What> = found.iter().map(|f| &f.what).collect();
        assert_eq!(kinds.len(), 3, "{text}");
        assert!(
            kinds.iter().any(|w| matches!(w, What::AuthorEmail(_))),
            "{text}"
        );
        assert!(
            kinds.iter().any(|w| matches!(w, What::CommitterEmail(_))),
            "{text}"
        );
        assert!(
            kinds.iter().any(|w| matches!(w, What::TrailerEmail(_))),
            "{text}"
        );
        // --commits never looks at content.
        assert!(!text.contains("x.txt"), "{text}");
    }

    #[test] // spec: QA-086
    fn identity_check_passes_no_reply_identities_and_checks_names_against_the_denylist() {
        let r = Repo::new();
        r.commit_as(OK_EMAIL, "noreply@github.com", "one");
        let found = scan(&r.dir, &Mode::Commits("HEAD".into()), &none()).unwrap();
        assert!(found.is_empty(), "{}", rendered(&found));
        // The fixture's author name is "Someone".
        let found = scan(&r.dir, &Mode::Commits("HEAD".into()), &deny("someone")).unwrap();
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        assert!(!rendered(&found).to_lowercase().contains("someone"));
    }

    // ---- pre-push ----------------------------------------------------------------------------

    #[test] // spec: QA-086
    fn new_branch_push_scans_only_commits_the_remote_lacks() {
        let r = Repo::new();
        // What the remote has (an excluded id) is already public and not rescanned.
        r.write("old.txt", b"quokka-zebra-77\n");
        let public = r.commit("public");
        r.git(&["checkout", "-q", "-b", "feature"]);
        r.write("new.txt", b"clean\n");
        let clean_head = r.commit("clean");
        let d = deny("quokka-zebra-77");
        let push = |head: &str, exclude: Vec<String>| PushScan::NewBranch {
            remote_ref: "refs/heads/feature".into(),
            head: head.into(),
            exclude,
        };
        let found = scan_push(&r.dir, &push(&clean_head, vec![public.clone()]), &d).unwrap();
        assert!(found.is_empty(), "{}", rendered(&found));
        // A new commit with a hit is found, and so is its message.
        r.write("new.txt", b"QUOKKA-ZEBRA-77\n");
        let dirty_head = r.commit("quokka-zebra-77 in the message");
        let found = scan_push(&r.dir, &push(&dirty_head, vec![public.clone()]), &d).unwrap();
        assert_eq!(found.len(), 2, "{}", rendered(&found));
        // When the remote has nothing, everything reachable is new.
        let found = scan_push(&r.dir, &push(&dirty_head, vec![]), &d).unwrap();
        assert_eq!(found.len(), 3, "{}", rendered(&found));
        // An exclusion that isn't an object id is an error, not a pass.
        assert!(scan_push(&r.dir, &push(&dirty_head, vec!["--all".into()]), &d).is_err());
    }

    #[test] // spec: QA-086
    fn existing_branch_push_scans_remote_to_local() {
        let r = Repo::new();
        let base = r.commit("base");
        r.write("a.txt", b"quokka-zebra-77\n");
        let head = r.commit("add");
        let push = PushScan::Range {
            remote_ref: "refs/heads/main".into(),
            base,
            head,
        };
        let found = scan_push(&r.dir, &push, &deny("quokka-zebra-77")).unwrap();
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        assert!(has_commit(&r.dir, &r.git(&["rev-parse", "HEAD"])));
        assert!(!has_commit(&r.dir, &"1".repeat(40)));
    }

    // ---- fix round 1 (ruling R33) ------------------------------------------------------------

    #[test] // spec: QA-086
    fn deleted_files_add_nothing_and_do_not_stop_the_scan() {
        let r = Repo::new();
        r.write(
            "gone.txt",
            format!("{}\n", mail("old", "gmail.com")).as_bytes(),
        );
        r.write("also-gone.txt", b"one\ntwo\n");
        let base = r.commit("base");
        std::fs::remove_file(r.dir.join("gone.txt")).unwrap();
        std::fs::remove_file(r.dir.join("also-gone.txt")).unwrap();
        r.write("kept.txt", b"clean\n");
        r.commit("delete two files");
        let found = scan(&r.dir, &Mode::Range(format!("{base}..HEAD")), &none()).unwrap();
        assert!(found.is_empty(), "{}", rendered(&found));
        // A deletion before an addition in the same commit doesn't hide the addition.
        std::fs::remove_file(r.dir.join("kept.txt")).unwrap();
        r.write(
            "z-new.txt",
            format!("{}\n", mail("x", "gmail.com")).as_bytes(),
        );
        r.commit("delete and add");
        let found = scan(&r.dir, &Mode::Range(format!("{base}..HEAD")), &none()).unwrap();
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        assert!(found[0].place.starts_with("z-new.txt:1"), "{}", found[0]);
    }

    fn utf16le(text: &str, bom: bool) -> Vec<u8> {
        let mut out = if bom { vec![0xFF, 0xFE] } else { vec![] };
        out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        out
    }

    #[test] // spec: QA-086
    fn utf16_text_is_decoded_and_scanned() {
        let r = Repo::new();
        let base = r.commit("base");
        let u = u();
        let text = format!("first\nC:\\{u}\\jdoe\\x\n");
        r.write("bom.txt", &utf16le(&text, true));
        r.write("nobom.txt", &utf16le(&text, false));
        let mut be = vec![0xFE, 0xFF];
        be.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
        r.write("be.txt", &be);
        r.commit("utf-16");
        for mode in [
            Mode::Tree("HEAD".into()),
            Mode::Range(format!("{base}..HEAD")),
        ] {
            let found = scan(&r.dir, &mode, &none()).unwrap();
            let text = rendered(&found);
            assert_eq!(found.len(), 3, "{mode:?}: {text}");
            for f in &found {
                assert!(matches!(f.what, What::ProfilePath(_)), "{f}");
                assert!(f.place.contains(":2 (UTF-16"), "{f}");
            }
            assert!(!text.contains("jdoe"), "{text}");
        }
    }

    #[test] // spec: QA-086
    fn binary_blobs_are_searched_for_denylist_entries_and_profile_paths() {
        let r = Repo::new();
        let base = r.commit("base");
        let u = u();
        // A .json with a NUL byte, holding a denylist entry (in another case).
        r.write(
            "data.json",
            b"{\"a\": \"x\x00y\", \"who\": \"Quokka-Zebra-77\"}\n",
        );
        // A binary with an embedded UTF-16LE profile path.
        let mut bin = vec![0x89, 0x50, 0x00, 0x01, 0x02, 0x03];
        bin.extend(utf16le(&format!("C:\\{u}\\jdoe\\AppData"), false));
        bin.extend([0x00, 0x00, 0xFF, 0x10, 0x00, 0x7F]);
        r.write("tool.dat", &bin);
        // A binary with an ASCII placeholder path and no entry: nothing.
        let mut ok = vec![0x00, 0x01];
        ok.extend(format!("C:\\{u}\\runneradmin\\x").as_bytes());
        r.write("ok.dat", &ok);
        // The denylist entry in UTF-16LE inside a binary.
        let mut wide = vec![0x00, 0x05, 0x06];
        wide.extend(utf16le("QUOKKA-ZEBRA-77", false));
        wide.extend([0x00, 0x00, 0x09]);
        r.write("wide.dat", &wide);
        r.commit("binaries");
        let d = deny("quokka-zebra-77");
        for mode in [
            Mode::Tree("HEAD".into()),
            Mode::Range(format!("{base}..HEAD")),
        ] {
            let found = scan(&r.dir, &mode, &d).unwrap();
            let text = rendered(&found);
            assert_eq!(found.len(), 3, "{mode:?}: {text}");
            let at = |p: &str| found.iter().find(|f| f.place.starts_with(p)).unwrap();
            // Found by the text reading and the UTF-16 readings; the byte search adds nothing
            // they already found (R34 deduplication).
            assert_eq!(at("data.json:1").what, What::Denylist(1), "{text}");
            assert!(
                matches!(at("tool.dat:1 (UTF-16LE").what, What::ProfilePath(_)),
                "{text}"
            );
            assert_eq!(at("wide.dat:1 (UTF-16LE").what, What::Denylist(1), "{text}");
            let lower = text.to_lowercase();
            assert!(
                !lower.contains("quokka") && !lower.contains("jdoe"),
                "{text}"
            );
        }
    }

    #[test] // spec: QA-086
    fn new_branch_excludes_only_what_the_remote_has_not_stale_tracking_refs() {
        let r = Repo::new();
        let bare = r._tmp.path().join("remote.git");
        r.git(&["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
        r.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
        // main is on the remote; it holds an entry, but it is already public.
        r.write("public.txt", b"quokka-zebra-77 public\n");
        let public = r.commit("public");
        r.git(&["push", "-q", "origin", "main"]);
        // `old` was pushed and then deleted on the remote: its tracking ref is stale.
        r.git(&["checkout", "-q", "-b", "old"]);
        r.write("secret.txt", b"quokka-zebra-77 never published\n");
        r.commit("on the deleted branch");
        r.git(&["push", "-q", "origin", "old"]);
        let out = Command::new("git")
            .arg("-C")
            .arg(&bare)
            .args(["update-ref", "-d", "refs/heads/old"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(!r.git(&["rev-parse", "refs/remotes/origin/old"]).is_empty());
        r.git(&["checkout", "-q", "-b", "feature"]);
        r.write("feature.txt", b"clean\n");
        let head = r.commit("feature");

        let tips = remote_tips(&r.dir, "origin").unwrap();
        assert_eq!(tips, vec![public.clone()]);
        let push = PushScan::NewBranch {
            remote_ref: "refs/heads/feature".into(),
            head,
            exclude: tips,
        };
        let found = scan_push(&r.dir, &push, &deny("quokka-zebra-77")).unwrap();
        // The stale branch's commit is scanned; main's isn't.
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        assert!(found[0].place.starts_with("secret.txt:1"), "{}", found[0]);
        // A remote that can't be asked is an error (the hook refuses).
        assert!(remote_tips(&r.dir, "no-such-remote").is_err());
        assert!(remote_tips(&r.dir, "--upload-pack=x").is_err());
    }

    #[test] // spec: QA-086
    fn pushed_ref_names_and_annotated_tags_are_scanned() {
        let r = Repo::new();
        r.write("a.txt", b"clean\n");
        let commit = r.commit("clean");
        let d = deny("quokka-zebra-77");
        // The ref name.
        let push = PushScan::Range {
            remote_ref: "refs/heads/quokka-zebra-77/topic".into(),
            base: commit.clone(),
            head: commit.clone(),
        };
        let found = scan_push(&r.dir, &push, &d).unwrap();
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        assert_eq!(found[0].place, "pushed ref name refs/heads/***/topic");
        // An annotated tag: tagger e-mail, message and name.
        let out = r
            .cmd(&[
                "tag",
                "-a",
                "rel-quokka-zebra-77",
                "-m",
                &format!("release\n\nbuilt by {}\n", mail("jdoe", "uni.edu")),
            ])
            .env("GIT_COMMITTER_NAME", "Tagger")
            .env("GIT_COMMITTER_EMAIL", mail("tagger", "gmail.com"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let tag = r.git(&["rev-parse", "refs/tags/rel-quokka-zebra-77"]);
        assert_ne!(tag, commit);
        let push = PushScan::NewBranch {
            remote_ref: "refs/tags/v1".into(),
            head: tag,
            exclude: vec![commit],
        };
        let found = scan_push(&r.dir, &push, &d).unwrap();
        let text = rendered(&found);
        assert_eq!(found.len(), 3, "{text}");
        assert!(
            found.iter().any(|f| matches!(f.what, What::TaggerEmail(_))),
            "{text}"
        );
        assert!(
            found.iter().any(|f| f.place.contains("message line 3")),
            "{text}"
        );
        assert!(
            found.iter().any(|f| f.place.ends_with(" tag header")),
            "{text}"
        );
        let lower = text.to_lowercase();
        assert!(!lower.contains("quokka") && !lower.contains("gmail") && !lower.contains("uni"));
    }

    #[test] // spec: QA-086
    fn masks_never_echo_the_domain_or_a_denylist_entry() {
        let d = deny("quokkazebra");
        let addr = mail("jdoe", "mail.quokkazebra");
        let found = scan_line(&format!("contact {addr}"), &d);
        let shown: Vec<String> = found
            .iter()
            .map(|w| {
                Finding {
                    place: "f:1".into(),
                    what: w.clone(),
                }
                .to_string()
            })
            .collect();
        assert!(
            found.iter().any(|w| matches!(w, What::Email(_))),
            "{shown:?}"
        );
        for line in &shown {
            assert!(!line.contains("quokkazebra"), "{line}");
            assert!(!line.contains("mail."), "{line}");
        }
        assert!(shown.iter().any(|l| l.contains("j***@***")), "{shown:?}");
        // Commit identities: author and committer e-mails in the TLD.
        let r = Repo::new();
        r.commit_as(&addr, &mail("ci", "corp.quokkazebra"), "one");
        let found = scan(&r.dir, &Mode::Commits("HEAD".into()), &d).unwrap();
        let text = rendered(&found);
        assert!(
            found.iter().any(|f| matches!(f.what, What::AuthorEmail(_))),
            "{text}"
        );
        assert!(
            !text.contains("quokkazebra") && !text.contains("corp"),
            "{text}"
        );
    }

    #[test] // spec: QA-086
    fn encoded_spoofed_and_unc_forms_are_caught() {
        let u = u();
        for line in [
            format!("C:%5C{u}%5Czed"),
            format!("C:%5c{u}%5czed"),
            format!("C%3A%2F{u}%2Fzed"),
            format!("file:///c/{u}/zed"),
            format!(r"\\localhost\c$\{u}\zed"),
        ] {
            assert_eq!(paths_in(&line), 1, "{line}");
        }
        for line in [
            format!("https://{}@x", mail("jane", "gmail.com")),
            format!("jane%40{}", "gmail.com"),
            format!("https://jane%40gmail.com{}github.com/", '@'),
            mail("jan\u{e9}", "gmail.com"),
            mail("\u{e9}lise", "gmail.com"),
        ] {
            assert!(emails_in(&line) >= 1, "{line}");
        }
        // High-DPI asset names are file names, not addresses.
        for line in ["icon@2x.png", "icon@3X.png", "logo@10x.svg"] {
            assert_eq!(emails_in(line), 0, "{line}");
        }
    }

    #[test] // spec: QA-086
    fn format_characters_do_not_hide_a_denylist_entry() {
        let d = deny("quokka-zebra-77");
        for line in [
            "quokka\u{200B}-zebra-77",
            "quo\u{00AD}kka-zebra\u{2060}-77",
            "\u{FEFF}QUOKKA-\u{200D}ZEBRA-77",
        ] {
            assert_eq!(scan_line(line, &d), vec![What::Denylist(1)], "{line:?}");
            assert!(
                !redact(line, &d).to_lowercase().contains("zebra"),
                "{line:?}"
            );
        }
    }

    #[test] // spec: QA-086
    fn evil_merge_content_is_scanned() {
        let r = Repo::new();
        let base = r.commit("base");
        r.git(&["checkout", "-q", "-b", "side"]);
        r.write("side.txt", b"side\n");
        r.commit("side");
        r.git(&["checkout", "-q", "main"]);
        r.write("main.txt", b"main\n");
        r.commit("main");
        r.git(&["merge", "-q", "--no-ff", "--no-commit", "side"]);
        // Content that neither parent has, added in the merge commit itself.
        r.write(
            "evil.txt",
            format!("{}\n", mail("x", "gmail.com")).as_bytes(),
        );
        let merge = r.commit("merge side");
        let found = scan(&r.dir, &Mode::Range(format!("{base}..HEAD")), &none()).unwrap();
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        assert!(found[0].place.contains(short(&merge)), "{}", found[0]);
    }

    /// Adds an index entry with `mode` pointing at `sha`, plus whatever else is staged, and
    /// commits without `add -A` (which would undo the entry).
    fn commit_index_entry(r: &Repo, mode: &str, sha: &str, path: &str, message: &str) -> String {
        r.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("{mode},{sha},{path}"),
        ]);
        r.git(&["commit", "-q", "-m", message]);
        r.git(&["rev-parse", "HEAD"])
    }

    #[test] // spec: QA-086
    fn symlink_targets_are_scanned() {
        let r = Repo::new();
        let base = r.commit("base");
        let u = u();
        let target = r.dir.join("target.txt");
        std::fs::write(&target, format!("C:/{u}/jdoe/notes")).unwrap();
        let blob = r.git(&["hash-object", "-w", "target.txt"]);
        std::fs::remove_file(&target).unwrap();
        commit_index_entry(&r, "120000", &blob, "link", "add a symlink");
        for mode in [
            Mode::Tree("HEAD".into()),
            Mode::Range(format!("{base}..HEAD")),
        ] {
            let found = scan(&r.dir, &mode, &none()).unwrap();
            assert_eq!(found.len(), 1, "{mode:?}: {}", rendered(&found));
            assert!(found[0].place.starts_with("link:1"), "{}", found[0]);
        }
    }

    #[test] // spec: QA-086
    fn submodules_are_scanned_by_gitmodules_and_path() {
        let r = Repo::new();
        let base = r.commit("base");
        r.write(
            ".gitmodules",
            b"[submodule \"sub\"]\n\tpath = sub\n\turl = https://example.com/quokka-zebra-77.git\n",
        );
        r.git(&["add", ".gitmodules"]);
        commit_index_entry(&r, "160000", &base, "sub", "add a submodule");
        let d = deny("quokka-zebra-77");
        for mode in [
            Mode::Tree("HEAD".into()),
            Mode::Range(format!("{base}..HEAD")),
        ] {
            let found = scan(&r.dir, &mode, &d).unwrap();
            assert_eq!(found.len(), 1, "{mode:?}: {}", rendered(&found));
            assert!(found[0].place.starts_with(".gitmodules:3"), "{}", found[0]);
        }
    }

    // ---- fix round 2 (ruling R34) ------------------------------------------------------------

    /// Commits `files` on top of a base commit and returns the base, for tree and range scans.
    fn commit_files(r: &Repo, files: &[(&str, Vec<u8>)]) -> String {
        let base = r.commit("base");
        for (name, bytes) in files {
            r.write(name, bytes);
        }
        r.commit("vectors");
        base
    }

    fn both_modes(base: &str) -> [Mode; 2] {
        [
            Mode::Tree("HEAD".into()),
            Mode::Range(format!("{base}..HEAD")),
        ]
    }

    #[test] // spec: QA-086
    fn a_bom_or_a_utf16_head_does_not_hide_utf8_content() {
        let r = Repo::new();
        let u = u();
        let path = format!("C:\\{u}\\zed\\x\n");
        let mut bom_le = vec![0xFF, 0xFE];
        bom_le.extend(path.as_bytes());
        let mut bom_be = vec![0xFE, 0xFF];
        bom_be.extend(b"quokka-zebra-77\n");
        // 4000 bytes of UTF-16LE, then a plain UTF-8 line.
        let mut head = utf16le(&"a\n".repeat(1000), false);
        head.extend(path.as_bytes());
        let base = commit_files(
            &r,
            &[
                ("bom-le.txt", bom_le),
                ("bom-be.txt", bom_be),
                ("utf16-head.txt", head),
            ],
        );
        for mode in both_modes(&base) {
            let found = scan(&r.dir, &mode, &deny("quokka-zebra-77")).unwrap();
            let text = rendered(&found);
            let at = |p: &str| {
                found
                    .iter()
                    .find(|f| f.place.starts_with(p))
                    .unwrap_or_else(|| panic!("{mode:?}: no finding at {p}: {text}"))
            };
            assert!(matches!(at("bom-le.txt:1").what, What::ProfilePath(_)));
            assert_eq!(at("bom-be.txt:1").what, What::Denylist(1));
            assert!(matches!(at("utf16-head.txt:").what, What::ProfilePath(_)));
            assert_eq!(found.len(), 3, "{mode:?}: {text}");
        }
    }

    #[test] // spec: QA-086
    fn late_and_mixed_utf16_content_is_found() {
        let r = Repo::new();
        let u = u();
        // 10 KB of ASCII, then UTF-16LE at an odd offset: a path, and an entry.
        let mut late_path = b"a\n".repeat(5000);
        late_path.push(b'\n');
        late_path.extend(utf16le(&format!("C:\\{u}\\zed\\x\n"), false));
        let mut late_entry = b"a\n".repeat(5000);
        late_entry.push(b'\n');
        late_entry.extend(utf16le("QUOKKA-ZEBRA-77\n", false));
        // Mixed: a UTF-8 line, binary bytes, then a UTF-16BE entry.
        let mut mixed = format!("note C:/{u}/zed/x\n").into_bytes();
        mixed.extend([0x00, 0x01, 0x02, 0xFF, 0x00]);
        mixed.extend("quokka-zebra-77".encode_utf16().flat_map(u16::to_be_bytes));
        mixed.extend([0x00, 0x0A]);
        let base = commit_files(
            &r,
            &[
                ("late-path.txt", late_path),
                ("late-entry.txt", late_entry),
                ("mixed.bin", mixed),
            ],
        );
        for mode in both_modes(&base) {
            let found = scan(&r.dir, &mode, &deny("quokka-zebra-77")).unwrap();
            let text = rendered(&found);
            let of = |p: &str| -> Vec<&What> {
                found
                    .iter()
                    .filter(|f| f.place.starts_with(p))
                    .map(|f| &f.what)
                    .collect()
            };
            assert!(
                matches!(of("late-path.txt")[..], [What::ProfilePath(_)]),
                "{mode:?}: {text}"
            );
            assert_eq!(
                of("late-entry.txt"),
                [&What::Denylist(1)],
                "{mode:?}: {text}"
            );
            let mixed = of("mixed.bin");
            assert_eq!(mixed.len(), 2, "{mode:?}: {text}");
            assert!(mixed.iter().any(|w| matches!(w, What::ProfilePath(_))));
            assert!(mixed.contains(&&What::Denylist(1)), "{mode:?}: {text}");
            assert!(!text.to_lowercase().contains("quokka"), "{text}");
        }
    }

    #[test] // spec: QA-086
    fn ls_remote_asks_the_push_url_not_the_fetch_url() {
        let r = Repo::new();
        let fetch = r._tmp.path().join("fetch.git");
        let push = r._tmp.path().join("push.git");
        for bare in [&fetch, &push] {
            r.git(&["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
        }
        // The fetch repository has the leak; the push destination is empty.
        r.write("leak.txt", b"quokka-zebra-77\n");
        let leak = r.commit("leak");
        r.git(&["push", "-q", fetch.to_str().unwrap(), "main"]);
        r.git(&["remote", "add", "origin", fetch.to_str().unwrap()]);
        r.git(&[
            "remote",
            "set-url",
            "--push",
            "origin",
            push.to_str().unwrap(),
        ]);
        let push_url = r.git(&["remote", "get-url", "--push", "origin"]);
        assert_eq!(remote_tips(&r.dir, "origin").unwrap(), vec![leak.clone()]);
        let tips = remote_tips(&r.dir, &push_url).unwrap();
        assert!(tips.is_empty(), "{tips:?}");
        let scan = PushScan::NewBranch {
            remote_ref: "refs/heads/main".into(),
            head: leak,
            exclude: tips,
        };
        let found = scan_push(&r.dir, &scan, &deny("quokka-zebra-77")).unwrap();
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        // The ext:: transport runs a command, so it is never asked.
        assert!(remote_tips(&r.dir, "ext::sh -c true").is_err());
        assert!(remote_tips(&r.dir, "EXT::x").is_err());
    }

    #[test] // spec: QA-086
    fn ref_names_get_the_email_rule() {
        let r = Repo::new();
        let commit = r.commit("clean");
        let push = PushScan::Range {
            remote_ref: format!("refs/heads/{}", mail("jane.doe", "gmail.com")),
            base: commit.clone(),
            head: commit,
        };
        let found = scan_push(&r.dir, &push, &none()).unwrap();
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        assert!(matches!(found[0].what, What::Email(_)));
        assert!(!found[0].to_string().contains("gmail"), "{}", found[0]);
    }

    #[test] // spec: QA-086
    fn extra_commit_headers_and_mergetags_are_scanned() {
        let r = Repo::new();
        let base = r.commit("base");
        let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
        let u = u();
        let raw = format!(
            "tree {tree}\nparent {base}\nauthor A <{OK_EMAIL}> 1 +0000\ncommitter C <{OK_EMAIL}> 1 +0000\nencoding ISO-8859-1\nx-extra C:\\{u}\\zed\\x\nmergetag object {base}\n type commit\n tag v0\n tagger T <{}> 1 +0000\n \n tag message quokka-zebra-77\ngpgsig -----BEGIN PGP SIGNATURE-----\n C:/{u}/zed/inside-the-signature\n -----END PGP SIGNATURE-----\n\nmessage\n",
            mail("tagger", "gmail.com")
        );
        let raw_path = r._tmp.path().join("raw-commit");
        std::fs::write(&raw_path, raw).unwrap();
        let sha = r.git(&[
            "hash-object",
            "-t",
            "commit",
            "-w",
            "--literally",
            raw_path.to_str().unwrap(),
        ]);
        let d = deny("quokka-zebra-77");
        let found = scan(&r.dir, &Mode::Range(format!("{base}..{sha}")), &d).unwrap();
        let text = rendered(&found);
        assert!(!text.contains("x-extra"), "{text}");
        // R35: the gpgsig header is scanned too (nothing is skipped).
        assert_eq!(found.len(), 4, "{text}");
        assert!(
            found
                .iter()
                .any(|f| f.place.ends_with("gpgsig header")
                    && matches!(f.what, What::ProfilePath(_))),
            "{text}"
        );
        assert!(
            found
                .iter()
                // R37: an unknown key is named by its position, never by its text.
                .any(|f| f.place.ends_with(" header #6") && matches!(f.what, What::ProfilePath(_))),
            "{text}"
        );
        assert!(
            found
                .iter()
                .any(|f| f.place.contains("mergetag tagger")
                    && matches!(f.what, What::TaggerEmail(_))),
            "{text}"
        );
        assert!(
            found
                .iter()
                .any(|f| f.place.contains("mergetag message") && f.what == What::Denylist(1)),
            "{text}"
        );
        // --commits checks the embedded tagger's identity only.
        let found = scan(&r.dir, &Mode::Commits(format!("{base}..{sha}")), &d).unwrap();
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        assert!(matches!(found[0].what, What::TaggerEmail(_)));
    }

    #[test] // spec: QA-086
    fn short_denylist_entries_warn_without_echoing() {
        let d = deny("abcd\nlonger-entry\nxy\n");
        let w = d.warnings();
        assert_eq!(
            w,
            [
                "privacy denylist entry 1 is shorter than 5 characters and may match random binary data or signature text",
                "privacy denylist entry 3 is shorter than 5 characters and may match random binary data or signature text",
            ]
        );
        // Short entries still apply.
        assert_eq!(scan_line("xx ABCD yy", &d), vec![What::Denylist(1)]);
        assert!(deny("fiveC").warnings().is_empty());
    }

    #[test] // spec: QA-086
    fn a_nul_ends_a_profile_name_so_placeholders_in_binaries_stay_allowed() {
        let u = u();
        let line = format!("\u{0}\u{1}C:\\{u}\\runneradmin\u{0}\u{2}tail");
        assert_eq!(paths_in(&line), 0, "{line:?}");
        let line = format!("\u{0}C:\\{u}\\zed\u{0}");
        assert_eq!(paths_in(&line), 1, "{line:?}");
    }

    // ---- fix round 3 (ruling R35) ------------------------------------------------------------

    /// Writes a raw commit object (`hash-object --literally`) and returns its id.
    fn raw_commit(r: &Repo, body: &str) -> String {
        let path = r._tmp.path().join("raw-object");
        std::fs::write(&path, body).unwrap();
        r.git(&[
            "hash-object",
            "-t",
            "commit",
            "-w",
            "--literally",
            path.to_str().unwrap(),
        ])
    }

    /// `tree` and identity headers for a raw commit on top of `parent`.
    fn raw_head(r: &Repo, parent: &str) -> String {
        let tree = r.git(&["rev-parse", &format!("{parent}^{{tree}}")]);
        format!(
            "tree {tree}\nparent {parent}\nauthor A <{OK_EMAIL}> 1 +0000\ncommitter C <{OK_EMAIL}> 1 +0000\n"
        )
    }

    /// `text` as a header value: continuation lines start with a space.
    fn continued(text: &str) -> String {
        text.trim_end_matches('\n').replace('\n', "\n ")
    }

    /// A fake signature start, then a path, a denylist entry and a trailer address.
    fn fake_signature_body() -> String {
        let u = u();
        format!(
            "-----BEGIN PGP SIGNATURE-----\nC:\\{u}\\zed\\x\nquokka-zebra-77\nCo-Authored-By: J <{}>\n",
            mail("jane", "gmail.com")
        )
    }

    #[test] // spec: QA-086
    fn a_fake_signature_block_hides_nothing_in_a_commit_message() {
        let r = Repo::new();
        let base = r.commit("base");
        let sha = raw_commit(
            &r,
            &format!(
                "{}\nsubject\n\n{}",
                raw_head(&r, &base),
                fake_signature_body()
            ),
        );
        let d = deny("quokka-zebra-77");
        let found = scan(&r.dir, &Mode::Range(format!("{base}..{sha}")), &d).unwrap();
        let text = rendered(&found);
        assert_eq!(found.len(), 3, "{text}");
        assert!(found.iter().any(|f| matches!(f.what, What::ProfilePath(_))));
        assert!(found.iter().any(|f| f.what == What::Denylist(1)));
        assert!(
            found
                .iter()
                .any(|f| matches!(f.what, What::TrailerEmail(_)))
        );
        // --commits still sees the trailer.
        let found = scan(&r.dir, &Mode::Commits(format!("{base}..{sha}")), &d).unwrap();
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        assert!(matches!(found[0].what, What::TrailerEmail(_)));
    }

    #[test] // spec: QA-086
    fn a_fake_signature_block_hides_nothing_in_tag_and_mergetag_messages() {
        let r = Repo::new();
        let base = r.commit("base");
        let d = deny("quokka-zebra-77");
        // An annotated tag.
        r.git(&[
            "tag",
            "-a",
            "v1",
            "-m",
            &format!("release\n\n{}", fake_signature_body()),
        ]);
        let tag = r.git(&["rev-parse", "refs/tags/v1"]);
        let push = PushScan::NewBranch {
            remote_ref: "refs/tags/v1".into(),
            head: tag,
            exclude: vec![base.clone()],
        };
        let found = scan_push(&r.dir, &push, &d).unwrap();
        let text = rendered(&found);
        assert_eq!(found.len(), 3, "{text}");
        assert!(
            found.iter().all(|f| f.place.contains("message line")),
            "{text}"
        );
        // A mergetag.
        let tag_text = format!(
            "object {base}\ntype commit\ntag v0\ntagger T <{OK_EMAIL}> 1 +0000\n\nm\n{}",
            fake_signature_body()
        );
        let sha = raw_commit(
            &r,
            &format!(
                "{}mergetag {}\n\nmerge\n",
                raw_head(&r, &base),
                continued(&tag_text)
            ),
        );
        let found = scan(&r.dir, &Mode::Range(format!("{base}..{sha}")), &d).unwrap();
        let text = rendered(&found);
        assert_eq!(found.len(), 3, "{text}");
        assert!(
            found.iter().all(|f| f.place.contains("mergetag message")),
            "{text}"
        );
    }

    #[test] // spec: QA-086
    fn malformed_id_headers_and_signatures_are_scanned() {
        let r = Repo::new();
        let base = r.commit("base");
        let u = u();
        let path = format!("C:\\{u}\\zed\\x");
        let tag_text =
            format!("object {path}\ntype {path}\ntag v0\ntagger T <{OK_EMAIL}> 1 +0000\n\nm\n");
        let body = format!(
            "{}tree {path}\nparent {path}\ngpgsig -----BEGIN PGP SIGNATURE-----\n {path}\n -----END PGP SIGNATURE-----\nmergetag {}\n\nmessage\n",
            raw_head(&r, &base),
            continued(&tag_text)
        );
        let sha = raw_commit(&r, &body);
        let found = scan(&r.dir, &Mode::Range(format!("{base}..{sha}")), &none()).unwrap();
        let text = rendered(&found);
        for place in [
            " tree header",
            " parent header",
            " gpgsig header",
            " mergetag object header",
            " mergetag type header",
        ] {
            assert!(
                found
                    .iter()
                    .any(|f| f.place.ends_with(place) && matches!(f.what, What::ProfilePath(_))),
                "{place}: {text}"
            );
        }
        assert_eq!(found.len(), 5, "{text}");
        // Well-formed ids and types are not findings.
        let clean = raw_commit(&r, &format!("{}\nclean\n", raw_head(&r, &base)));
        let found = scan(&r.dir, &Mode::Range(format!("{base}..{clean}")), &none()).unwrap();
        assert!(found.is_empty(), "{}", rendered(&found));
    }

    #[test] // spec: QA-086
    fn whole_ident_lines_are_scanned() {
        let r = Repo::new();
        let base = r.commit("base");
        let u = u();
        let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
        let cases = [
            // A name holding an address.
            format!(
                "author {} <{OK_EMAIL}> 1 +0000\ncommitter C <{OK_EMAIL}> 1 +0000\n",
                mail("jane.doe", "gmail.com")
            ),
            // A name holding a path.
            format!("author C:\\{u}\\zed <{OK_EMAIL}> 1 +0000\ncommitter C <{OK_EMAIL}> 1 +0000\n"),
            // Text after the `>`.
            format!(
                "author A <{OK_EMAIL}> C:\\{u}\\zed 1 +0000\ncommitter C <{OK_EMAIL}> 1 +0000\n"
            ),
            // A continuation line under the committer.
            format!(
                "author A <{OK_EMAIL}> 1 +0000\ncommitter C <{OK_EMAIL}> 1 +0000\n C:\\{u}\\zed\n"
            ),
        ];
        for ident in cases {
            let sha = raw_commit(&r, &format!("tree {tree}\nparent {base}\n{ident}\nm\n"));
            for mode in [
                Mode::Range(format!("{base}..{sha}")),
                Mode::Commits(format!("{base}..{sha}")),
            ] {
                let found = scan(&r.dir, &mode, &none()).unwrap();
                let text = rendered(&found);
                assert_eq!(found.len(), 1, "{mode:?} {ident:?}: {text}");
                assert!(found[0].place.ends_with(" line"), "{text}");
                assert!(!text.contains("zed") && !text.contains("gmail"), "{text}");
            }
        }
    }

    /// Deterministic noise (xorshift), so the fixture is the same on every run.
    fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 32) as u8
            })
            .collect()
    }

    #[test] // spec: QA-086
    fn random_bytes_are_not_addresses_but_a_real_one_in_a_binary_is() {
        let d = none();
        // 1 MB of noise from seed 20: the plain text rule sees an "address" in it ...
        let blob = noise(20, 1 << 20);
        let text = String::from_utf8_lossy(&blob);
        let naive: usize = text
            .split('\n')
            .map(|l| {
                scan_line(l, &d)
                    .iter()
                    .filter(|w| matches!(w, What::Email(_)))
                    .count()
            })
            .sum();
        assert!(naive > 0, "the fixture must fool the plain rule");
        // ... but a blob with a NUL only reports printable, delimited addresses (R35).
        assert!(blob.contains(&0));
        assert!(blob_findings(&blob, &d).is_empty());
        // A real address stored in the binary is still found, as UTF-8 and as UTF-16LE.
        let mut with = blob.clone();
        with.extend(b"\x00");
        with.extend(mail("jane.doe", "gmail.com").as_bytes());
        // Three NULs keep the UTF-16 text on an even offset, after a U+0000 unit.
        with.extend(b"\x00\x00\x00");
        with.extend(utf16le(&mail("rob.roe", "example-mail.org"), false));
        with.extend(b"\x00\x00");
        let hits = blob_findings(&with, &d);
        assert_eq!(
            hits.iter()
                .filter(|h| matches!(h.what, What::Email(_)))
                .count(),
            2,
            "{hits:?}"
        );
        // Text without a NUL keeps the plain rule: non-ASCII local parts count there.
        let plain = format!("{}\n", mail("jan\u{e9}", "gmail.com"));
        assert_eq!(blob_findings(plain.as_bytes(), &d).len(), 1);
    }

    #[test] // spec: QA-086
    fn a_bom_brings_the_utf16_readings_without_a_nul() {
        // UTF-16 text whose code units have no zero byte (CJK, Cyrillic): only the BOM tells.
        let d = deny(
            "\u{6e2c}\u{8a66}\u{8cc7}\u{6599}\n\u{0422}\u{0435}\u{0441}\u{0442}\u{043e}\u{0432}\u{044b}\u{0439}",
        );
        let cjk = "\u{6e2c}\u{8a66}\u{8cc7}\u{6599}";
        let cyr = "\u{0422}\u{0435}\u{0441}\u{0442}\u{043e}\u{0432}\u{044b}\u{0439}";
        let le = |t: &str| -> Vec<u8> {
            let mut b = vec![0xFF, 0xFE];
            b.extend(t.encode_utf16().flat_map(u16::to_le_bytes));
            b
        };
        let be = |t: &str| -> Vec<u8> {
            let mut b = vec![0xFE, 0xFF];
            b.extend(t.encode_utf16().flat_map(u16::to_be_bytes));
            b
        };
        for (bytes, entry) in [(le(cjk), 1), (be(cjk), 1), (le(cyr), 2), (be(cyr), 2)] {
            assert!(!bytes.contains(&0), "the vector has no NUL");
            let hits = blob_findings(&bytes, &d);
            assert!(
                hits.iter().any(|h| h.what == What::Denylist(entry)),
                "{hits:?}"
            );
        }
    }

    #[test] // spec: QA-086
    fn a_utf16_commit_message_is_read_as_utf16() {
        let r = Repo::new();
        let base = r.commit("base");
        let u = u();
        let mut body = raw_head(&r, &base).into_bytes();
        body.extend(b"encoding UTF-16LE\n\n");
        body.extend(utf16le(
            &format!(
                "subject\n\nC:\\{u}\\zed\\x\nCo-Authored-By: J <{}>\nquokka-zebra-77\n",
                mail("jane", "gmail.com")
            ),
            false,
        ));
        let path = r._tmp.path().join("raw-utf16");
        std::fs::write(&path, &body).unwrap();
        let sha = r.git(&[
            "hash-object",
            "-t",
            "commit",
            "-w",
            "--literally",
            path.to_str().unwrap(),
        ]);
        let d = deny("quokka-zebra-77");
        let found = scan(&r.dir, &Mode::Range(format!("{base}..{sha}")), &d).unwrap();
        let text = rendered(&found);
        assert_eq!(found.len(), 3, "{text}");
        assert!(found.iter().all(|f| f.place.contains("(UTF-16")), "{text}");
        let found = scan(&r.dir, &Mode::Commits(format!("{base}..{sha}")), &d).unwrap();
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        assert!(matches!(found[0].what, What::TrailerEmail(_)));
    }

    #[test] // spec: QA-086
    fn wide_whitespace_before_an_address_or_path_does_not_panic() {
        // U+2028 is whitespace three bytes wide; offsets after it must stay on a boundary.
        let u = u();
        for line in [
            format!("\u{2028}{}", mail("jane", "gmail.com")),
            format!("x\u{3000}https://{}/", mail("tok", "host.com")),
            format!("\u{2028}https://h/c/{u}/zed"),
        ] {
            let _ = scan_line(&line, &none());
        }
        assert_eq!(
            emails_in(&format!("\u{2028}{}", mail("jane", "gmail.com"))),
            1
        );
    }

    #[test] // spec: QA-086
    fn ref_name_findings_cover_the_cli_ref_name() {
        let d = deny("quokka-zebra-77");
        let found = ref_name_findings("refs/heads/quokka-zebra-77", &d);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].place, "pushed ref name refs/heads/***");
        assert!(ref_name_findings("refs/heads/feature/x", &d).is_empty());
    }

    // ---- fix round 4 (ruling R37) ------------------------------------------------------------

    /// A well-formed id for pure commit and tag texts (never looked up).
    const SOME_ID: &str = "0123456789abcdef0123456789abcdef01234567";

    /// The commit's headers up to the identities, then `extra` headers and a message.
    fn commit_text(extra: &str) -> String {
        format!(
            "tree {SOME_ID}\nparent {SOME_ID}\nauthor A <{OK_EMAIL}> 1 +0000\ncommitter C <{OK_EMAIL}> 1 +0000\n{extra}\nm\n"
        )
    }

    /// The findings in one raw commit text, with content (`--range`) or without (`--commits`).
    fn commit_text_findings(raw: &str, d: &Denylist, content: bool) -> Vec<Finding> {
        let mut out = Vec::new();
        commit_findings(SOME_ID, raw, d, content, &mut out);
        out
    }

    /// Texts that must never be printed by the header-key vectors.
    const NEVER_SHOWN: [&str; 7] = ["zed", "gmail", "jane", "quokka", "wombat", "x-foo", "x-a "];

    fn assert_never_shown(text: &str) {
        for secret in NEVER_SHOWN {
            assert!(!text.contains(secret), "`{secret}` printed: {text}");
        }
    }

    #[test] // spec: QA-086
    fn header_keys_are_scanned_and_never_printed() {
        let u = u();
        let path = format!("C:\\{u}\\zed\\x");
        let jane = mail("jane.private", "gmail.com");
        let d = deny("quokka-zebra-77\nwombat-heron-12");
        // (extra headers, findings): the key is scanned like the value (R37, I1).
        let cases: Vec<(String, usize)> = vec![
            (format!("x-foo\t{path}\n"), 1),
            (format!("{path}\n"), 1),
            (format!("{path} val\n"), 1),
            (format!("{jane} v\n"), 1),
            ("quokka-zebra-77 v\n".into(), 1),
            ("quokka-zebra-77\n".into(), 1),
            (format!("x-foo\u{0}{path}\n"), 1),
            // The key echo vectors: an address, a path and entries in the key itself.
            (format!("{jane} {path} wombat-heron-12\n"), 3),
            (format!("{path}.wombat-heron-12 v\n"), 2),
            ("x-quokka-zebra-77 wombat-heron-12\n".into(), 2),
            // A continuation line under an unknown key.
            (format!("x-a v\n {path}\n"), 1),
        ];
        for (extra, n) in &cases {
            let found = commit_text_findings(&commit_text(extra), &d, true);
            let text = rendered(&found);
            assert_eq!(found.len(), *n, "{extra:?}: {text}");
            assert!(
                found.iter().all(|f| f.place.ends_with(" header #5")),
                "{extra:?}: {text}"
            );
            assert_never_shown(&text);
        }
        // The same keys inside a mergetag, and in an annotated tag.
        for (extra, n) in &cases {
            let tag_text = format!(
                "object {SOME_ID}\ntype commit\ntag v0\n{extra}tagger T <{OK_EMAIL}> 1 +0000\n\nm\n"
            );
            let raw = commit_text(&format!("mergetag {}\n", continued(&tag_text)));
            let found = commit_text_findings(&raw, &d, true);
            let text = rendered(&found);
            assert_eq!(found.len(), *n, "mergetag {extra:?}: {text}");
            assert!(
                found
                    .iter()
                    .all(|f| f.place.ends_with(" mergetag header #4")),
                "{text}"
            );
            assert_never_shown(&text);
            let mut found = Vec::new();
            tag_text_findings(&tag_text, "tag abc1234", &d, true, &mut found);
            let text = rendered(&found);
            assert_eq!(found.len(), *n, "tag {extra:?}: {text}");
            assert!(
                found.iter().all(|f| f.place == "tag abc1234 header #4"),
                "{text}"
            );
            assert_never_shown(&text);
        }
        // A continuation line before any header is scanned as a header of its own.
        let raw = format!(" {path}\n{}", commit_text(""));
        let found = commit_text_findings(&raw, &d, true);
        assert_eq!(found.len(), 1, "{}", rendered(&found));
        assert_eq!(
            found[0].place,
            format!("commit {} header #1", short(SOME_ID))
        );
        // Standard keys are named; nothing else ever is.
        assert_eq!(header_label("gpgsig-sha256", 7), "gpgsig-sha256 header");
        assert_eq!(header_label("x-foo", 7), "header #7");
        assert_eq!(header_label("Tree", 1), "header #1");
        // Only lowercase, full-length ids are skipped unscanned.
        assert!(is_object_id(SOME_ID));
        assert!(!is_object_id(&SOME_ID.to_ascii_uppercase()));
        assert!(!is_object_id(&format!("{SOME_ID} x")));
    }

    #[test] // spec: QA-086
    fn header_keys_are_scanned_end_to_end_without_echo() {
        let r = Repo::new();
        let base = r.commit("base");
        let u = u();
        let jane = mail("jane.private", "gmail.com");
        let sha = raw_commit(
            &r,
            &format!(
                "{}{jane} C:\\{u}\\zed\\x quokka-zebra-77\n\nm\n",
                raw_head(&r, &base)
            ),
        );
        let d = deny("quokka-zebra-77");
        let found = scan(&r.dir, &Mode::Range(format!("{base}..{sha}")), &d).unwrap();
        let text = rendered(&found);
        assert_eq!(found.len(), 3, "{text}");
        assert!(text.contains(" header #5: "), "{text}");
        assert_never_shown(&text);
    }

    #[test] // spec: QA-086
    fn a_path_inside_an_ident_address_is_found() {
        let u = u();
        let addr = mail(&format!("C:\\{u}\\zed\\x"), "users.noreply.github.com");
        let d = none();
        let raw_author = format!(
            "tree {SOME_ID}\nauthor A <{addr}> 1 +0000\ncommitter C <{OK_EMAIL}> 1 +0000\n\nm\n"
        );
        let raw_committer = format!(
            "tree {SOME_ID}\nauthor A <{OK_EMAIL}> 1 +0000\ncommitter C <{addr}> 1 +0000\n\nm\n"
        );
        let tag_text =
            format!("object {SOME_ID}\ntype commit\ntag v0\ntagger T <{addr}> 1 +0000\n\nm\n");
        let raw_mergetag = commit_text(&format!("mergetag {}\n", continued(&tag_text)));
        for (raw, place) in [
            (&raw_author, " author e-mail"),
            (&raw_committer, " committer e-mail"),
            (&raw_mergetag, " mergetag tagger e-mail"),
        ] {
            // Both --range and --commits: the address is identity data.
            for content in [true, false] {
                let found = commit_text_findings(raw, &d, content);
                let text = rendered(&found);
                assert_eq!(found.len(), 1, "{place}: {text}");
                assert!(found[0].place.ends_with(place), "{text}");
                assert!(matches!(found[0].what, What::ProfilePath(_)), "{text}");
                assert!(!text.contains("zed"), "{text}");
            }
        }
        // An annotated tag's tagger, with and without content.
        for content in [true, false] {
            let mut found = Vec::new();
            tag_text_findings(&tag_text, "tag abc1234", &d, content, &mut found);
            assert_eq!(found.len(), 1, "{}", rendered(&found));
            assert_eq!(found[0].place, "tag abc1234 tagger e-mail");
        }
    }

    #[test] // spec: QA-086
    fn a_bom_file_without_a_nul_is_decoded_in_range_mode_too() {
        let r = Repo::new();
        let cjk = "\u{6e2c}\u{8a66}\u{8cc7}\u{6599}";
        let d = deny(&format!("quokka-zebra-77\n{cjk}"));
        let mut le = vec![0xFF, 0xFE];
        le.extend(cjk.encode_utf16().flat_map(u16::to_le_bytes));
        let mut be = vec![0xFE, 0xFF];
        be.extend(cjk.encode_utf16().flat_map(u16::to_be_bytes));
        assert!(
            !le.contains(&0) && !be.contains(&0),
            "the vectors have no NUL"
        );
        // One test picks whole-blob reading for --tree and --range alike (R37, I2).
        assert!(read_whole(&le) && read_whole(&be));
        let base = commit_files(&r, &[("le.txt", le), ("be.txt", be)]);
        for mode in both_modes(&base) {
            let found = scan(&r.dir, &mode, &d).unwrap();
            let text = rendered(&found);
            for file in ["le.txt", "be.txt"] {
                assert!(
                    found
                        .iter()
                        .any(|f| f.place.starts_with(file) && f.what == What::Denylist(2)),
                    "{mode:?} {file}: {text}"
                );
            }
        }
    }

    #[test] // spec: QA-086
    fn binary_addresses_may_end_at_sentence_or_path_punctuation() {
        let d = none();
        let emails = |blob: &[u8]| {
            blob_findings(blob, &d)
                .iter()
                .filter(|h| matches!(h.what, What::Email(_)))
                .count()
        };
        let addr = mail("carl.marlowe", "protonmail.com");
        for tail in [".", ". See", "!", "?", "/", "-", "-x"] {
            let blob = [b"\x00\x01 ", addr.as_bytes(), tail.as_bytes(), b"\x00"].concat();
            assert_eq!(emails(&blob), 1, "{tail:?}");
        }
        // Still 8 characters or more, and still a delimiter before and after.
        let blob = [b"\x00 ", mail("a", "bc.co").as_bytes(), b".\x00"].concat();
        assert_eq!(emails(&blob), 0);
        let blob = [b"\x00 ", addr.as_bytes(), b"_\x00"].concat();
        assert_eq!(emails(&blob), 0);
        let blob = [b"\x00~", addr.as_bytes(), b".\x00"].concat();
        assert_eq!(emails(&blob), 0);
    }

    #[test] // spec: QA-086
    fn two_addresses_that_mask_alike_stay_two_findings() {
        let d = none();
        let one = mail("jane.one", "gmail.com");
        let two = mail("jane.two", "gmail.com");
        let line = format!("to {one} and {two}\n");
        let emails = |hits: &[BlobHit]| -> Vec<What> {
            hits.iter()
                .filter(|h| matches!(h.what, What::Email(_)))
                .map(|h| h.what.clone())
                .collect()
        };
        // Text: both, though they print alike (R37, M3).
        let found = emails(&blob_findings(line.as_bytes(), &d));
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0], found[1]);
        // The same address twice on one line is one finding.
        let same = format!("{one} {one}\n");
        assert_eq!(emails(&blob_findings(same.as_bytes(), &d)).len(), 1);
        // The UTF-16 readings of a blob.
        assert_eq!(emails(&blob_findings(&utf16le(&line, false), &d)).len(), 2);
        // The UTF-16 readings of a commit message.
        let message = String::from_utf8(utf16le(&format!("subject\n\n{line}"), false)).unwrap();
        let mut out = Vec::new();
        message_findings(&message, "commit abc1234", &d, true, true, &mut out);
        assert_eq!(out.len(), 2, "{}", rendered(&out));
    }

    #[test] // spec: QA-086
    fn a_line_with_thousands_of_addresses_or_paths_scans_in_linear_time() {
        // Every per-match question about "the token before this match" is answered by one
        // forward walk of the line (R37, M5). The quadratic version took about a minute here
        // in a debug build, this one well under a second (see the fix-round-4 report); the
        // bound below is generous for slow, busy machines.
        let start = std::time::Instant::now();
        let line = format!("{};", mail("u1", "y.zz")).repeat(12_000);
        assert_eq!(emails_in(&line), 12_000);
        // In one URL token: the first is userinfo, every later one isn't.
        assert_eq!(emails_in(&format!("ssh://{line}")), 11_999);
        let u = u();
        let paths = format!("/c/{u}/a").repeat(12_000);
        assert_eq!(paths_in(&paths), 12_000);
        assert_eq!(paths_in(&format!("https://h{paths}")), 0);
        assert_eq!(paths_in(&format!("file://{paths}")), 12_000);
        let took = start.elapsed();
        assert!(took < std::time::Duration::from_secs(5), "{took:?}");
    }
}
