//! The app's command line (APP-014, APP-020, APP-021; Appendix G "Apps", "`lil:` URLs" and
//! "`.lilref` files"; P4a-R3, P4a-R21), Qt-free:
//! `lil-image [--intent open | edit | use_as_input] [--no-activate] [--confirm [<id>]]
//! [<URL> | <path>]`, and `--smoke-exit` for tests.
//!
//! [`plan_boot`] splits the raw command line once (`lil_platform::desktop::split_first_argument`)
//! and reads its rest without re-splitting it (P4a-R3): switches are words starting `-`, and
//! whatever follows them is the one positional argument, given to `lil_parse::strip_argument` and
//! then to `lil_parse::parse_lil_url` unchanged. [`plan_args`] reads the same arguments as a
//! second launch forwards them (`single_instance`).
//!
//! Two kinds of "no":
//! - **The start is refused** ([`BootPlan::refused`]): the command line isn't the grammar (an
//!   unknown or repeated switch, a Qt option, a quoted switch, more than one positional argument,
//!   `--confirm` with a link). Qt never starts and the process exits 2.
//! - **The link is refused** ([`BootPlan::link_refused`]): the one positional argument is
//!   well formed as an argument but isn't a link or file LILITH opens. The app starts (or the
//!   running one is told) and its Opened card says why; nothing else happens, and a refused
//!   `deny/` link never answers anything.
//!
//! A `.lilref` argument (its path ends `.lilref`, any case) is read here, at most 4 KiB, and only
//! from this PC: a UNC path or a network drive is refused unread, and so is a file whose final
//! path, once opened, isn't on a local drive.
//!
//! Text from a link, a `.lilref`, a target or a title reaches a card only through
//! [`shown_outside`], which cuts what may be a deny token however it is spelled.

use std::io::Read as _;
use std::path::Path;

use lil_parse::{LilUrl, parse_lil_url, strip_argument};
use lil_platform::Env;
use lil_platform::desktop::split_first_argument;
use serde_json::{Value, json};

/// Qt's `applicationName`: the app's folder under the channel's local data folder.
pub const APP: &str = "image";

/// The program name Qt gets as its only argument.
pub const QT_ARGV0: &str = "lil-image";

/// `--smoke-exit`: quit with 0 once the window's first frame is shown and the event loop has gone
/// idle (`tools/deploy/check-deploy.ps1`).
pub const SMOKE_EXIT: &str = "--smoke-exit";

/// `--no-activate`: show the window without taking the focus; its taskbar button flashes.
pub const NO_ACTIVATE: &str = "--no-activate";

/// `--intent <open | edit | use_as_input>` (APP-066).
pub const INTENT: &str = "--intent";

/// `--confirm [<id>]`: the confirm window, on one confirmation or (with no id) every pending one.
pub const CONFIRM: &str = "--confirm";

/// The largest `.lilref` file read (Appendix G).
pub const LILREF_MAX_BYTES: u64 = 4096;

/// How many characters of a refused argument are shown.
const SHOWN_CHARS: usize = 80;

/// The refusal of a `.lilref` file elsewhere than on this PC.
pub const LILREF_NOT_LOCAL: &str = "LILITH opens .lilref files from this PC only.";

/// The window a start opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// The app's main window.
    Shell,
    /// The confirm window (P4a-R11): on one confirmation, or with no id on every pending one.
    Confirm { id: Option<String> },
}

/// What an open request asks for (APP-066): lil-qt's own, written as the core writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Open,
    Edit,
    UseAsInput,
}

impl Intent {
    /// `open`, `edit` or `use_as_input`.
    pub fn as_str(self) -> &'static str {
        match self {
            Intent::Open => "open",
            Intent::Edit => "edit",
            Intent::UseAsInput => "use_as_input",
        }
    }

    fn parse(s: &str) -> Option<Intent> {
        match s {
            "open" => Some(Intent::Open),
            "edit" => Some(Intent::Edit),
            "use_as_input" => Some(Intent::UseAsInput),
            _ => None,
        }
    }
}

/// What the positional argument asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    /// `<scheme>:confirm/<id>`: the confirm window on it.
    Confirm(String),
    /// `<scheme>:deny/<id>/<token>`: a toast's Deny, answered with no window (P4a-R9).
    Deny { id: String, token: String },
    /// `<scheme>:task/<job id>`: the Jobs panel on that job (P4a-R21).
    Task(String),
    /// `<scheme>:open/<target>` (decoded) or a file path: the Opened card.
    Target(String),
    /// A raw `lil://` ref, percent-decoded once: the Opened card.
    Ref(String),
    /// A `.lilref` file's sealed ref and title: the Opened card.
    LilRef {
        reference: String,
        title: Option<String>,
    },
}

/// What a start does, settled before Qt starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootPlan {
    pub mode: Mode,
    pub intent: Intent,
    pub no_activate: bool,
    pub smoke_exit: bool,
    /// What the positional argument opens.
    pub opened: Option<Opened>,
    /// Why the positional argument opens nothing: what the Opened card says. Never with
    /// `opened`.
    pub link_refused: Option<String>,
    /// Why the start is refused: Qt never starts and the process exits 2. Never the argument
    /// itself, which may carry a token.
    pub refused: Option<String>,
    /// Set by `single_instance::start` once the arguments reached the running instance.
    pub forwarded: bool,
    /// Set by `single_instance::start` when this start runs as an instance of its own without
    /// the app's pipe, because the hand-over failed: what the shell's banner says of why.
    pub pipe_notice: Option<&'static str>,
    /// Qt's `organizationName`: the channel's data-folder name (`LILITH`, `LILITH-dev`).
    pub org: String,
    /// Qt's `applicationName` ([`APP`]).
    pub app: String,
    /// The `argv` Qt is built from: [`QT_ARGV0`] alone.
    pub qt_args: Vec<String>,
    /// The arguments as read, which a second launch forwards: each switch and its value, then
    /// the positional argument without its surrounding spaces and quotes. Empty when refused.
    pub args: Vec<String>,
}

/// Plans a start from the raw command line (`lil_platform::desktop::raw_command_line`).
pub fn plan_boot(raw_cmdline: &str, env: &Env) -> BootPlan {
    let (_exe, rest) = split_first_argument(raw_cmdline);
    match words(&rest) {
        Ok(args) => plan_args(&args, env),
        Err(why) => refused_plan(env, why),
    }
}

/// Plans what `args` (a forwarded launch's, or [`plan_boot`]'s) ask for.
pub fn plan_args(args: &[String], env: &Env) -> BootPlan {
    match read(args, env) {
        Ok(plan) => plan,
        Err(why) => refused_plan(env, why),
    }
}

fn base_plan(env: &Env) -> BootPlan {
    let org = env
        .data_dir()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    BootPlan {
        mode: Mode::Shell,
        intent: Intent::Open,
        no_activate: false,
        smoke_exit: false,
        opened: None,
        link_refused: None,
        refused: None,
        forwarded: false,
        pipe_notice: None,
        org,
        app: APP.to_string(),
        qt_args: vec![QT_ARGV0.to_string()],
        args: Vec::new(),
    }
}

fn refused_plan(env: &Env, why: &str) -> BootPlan {
    BootPlan {
        refused: Some(format!("LILITH didn't start: {why}")),
        ..base_plan(env)
    }
}

const NOT_AN_ARGUMENT: &str = "its command line has an argument it doesn't take.";
const REPEATED: &str = "its command line repeats a switch.";
const TWO_ARGUMENTS: &str = "its command line has more than one link or file.";

/// The raw rest of the command line as arguments, never re-split (P4a-R3): each switch word,
/// the word after `--intent`, the word after `--confirm` when it is a confirmation id, and then
/// the rest as one argument, stripped of surrounding spaces and one pair of quotes. That
/// argument must be one: an unquoted one holds no space or tab, and none holds a `"`.
fn words(rest: &str) -> Result<Vec<String>, &'static str> {
    let blank = [' ', '\t'];
    let mut out = Vec::new();
    let mut s = rest;
    loop {
        s = s.trim_start_matches(blank);
        if s.is_empty() {
            return Ok(out);
        }
        let takes_value = |prev: Option<&String>, word: &str| match prev.map(String::as_str) {
            Some(INTENT) => true,
            Some(CONFIRM) => word.starts_with("cf_"),
            _ => false,
        };
        let end = s.find(blank).unwrap_or(s.len());
        let word = &s[..end];
        if word.starts_with('-') || takes_value(out.last(), word) {
            out.push(word.to_string());
            s = &s[end..];
            continue;
        }
        let raw = s.trim_end_matches(blank);
        let arg = strip_argument(raw);
        if arg.starts_with('-') {
            return Err(NOT_AN_ARGUMENT);
        }
        if arg.contains('"') || (!raw.starts_with('"') && arg.contains(blank)) {
            return Err(TWO_ARGUMENTS);
        }
        out.push(arg.to_string());
        return Ok(out);
    }
}

fn read(args: &[String], env: &Env) -> Result<BootPlan, &'static str> {
    let mut plan = base_plan(env);
    let mut intent_seen = false;
    let mut confirm: Option<Option<String>> = None;
    let mut positional: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        let seen = match arg {
            SMOKE_EXIT => std::mem::replace(&mut plan.smoke_exit, true),
            NO_ACTIVATE => std::mem::replace(&mut plan.no_activate, true),
            INTENT => {
                let value = args.get(i).ok_or(NOT_AN_ARGUMENT)?;
                i += 1;
                plan.intent = Intent::parse(value).ok_or(NOT_AN_ARGUMENT)?;
                std::mem::replace(&mut intent_seen, true)
            }
            CONFIRM => {
                let id = match args.get(i) {
                    Some(id) if id.starts_with("cf_") => {
                        i += 1;
                        if !is_confirmation_id(id) {
                            return Err(NOT_AN_ARGUMENT);
                        }
                        Some(id.clone())
                    }
                    _ => None,
                };
                confirm.replace(id).is_some()
            }
            _ if arg.starts_with('-') || arg.is_empty() || arg.contains('"') => {
                return Err(NOT_AN_ARGUMENT);
            }
            _ => {
                if positional.is_some() || i < args.len() {
                    return Err(TWO_ARGUMENTS);
                }
                positional = Some(arg);
                false
            }
        };
        if seen {
            return Err(REPEATED);
        }
    }
    // `--smoke-exit` is the deploy check's: alone, or with `--no-activate` (review M-4).
    if plan.smoke_exit && (positional.is_some() || confirm.is_some() || intent_seen) {
        return Err(NOT_AN_ARGUMENT);
    }
    if let Some(id) = confirm {
        if positional.is_some() {
            return Err(TWO_ARGUMENTS);
        }
        plan.mode = Mode::Confirm { id };
    }
    if let Some(arg) = positional {
        match classify(arg, env) {
            Ok(opened) => {
                if let Opened::Confirm(id) = &opened {
                    plan.mode = Mode::Confirm {
                        id: Some(id.clone()),
                    };
                }
                plan.opened = Some(opened);
            }
            Err(why) => plan.link_refused = Some(why),
        }
    }
    plan.args = args.to_vec();
    if plan.link_refused.is_some()
        && let Some(link) = plan.args.last_mut()
    {
        cut_for_the_wire(link);
    }
    Ok(plan)
}

/// The most of a refused link a second launch forwards, in UTF-8 bytes: a message may be 64 KiB
/// (`single_instance::MESSAGE_MAX`), and JSON may write a character in 6 bytes.
const FORWARDED_LINK_BYTES: usize = 8192;

/// A refused link as a second launch forwards it, so it reaches the Opened card, cut, however
/// long it is. It stays refused when the running instance reads it again: its `…` fits no
/// verb, ref or `.lilref`, and what's left of a path is still longer than a target may be.
fn cut_for_the_wire(link: &mut String) {
    if link.len() > FORWARDED_LINK_BYTES {
        let mut at = FORWARDED_LINK_BYTES;
        while !link.is_char_boundary(at) {
            at -= 1;
        }
        link.truncate(at);
        link.push('…');
    }
}

/// `^cf_[0-9a-f]{16}$`.
fn is_confirmation_id(id: &str) -> bool {
    id.strip_prefix("cf_").is_some_and(|hex| {
        hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// `C:\…` or `C:/…`.
fn is_drive_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/')
}

/// `\\server\share…`, `//server…`, `\\.\…` or `\\?\…`: not a plain local path.
fn is_unc_like(s: &str) -> bool {
    s.starts_with("\\\\") || s.starts_with("//") || s.starts_with("\\/") || s.starts_with("/\\")
}

/// A scheme before the first `:`: two or more letters, digits, `+`, `-` or `.`, from a letter.
fn has_scheme(s: &str) -> bool {
    s.split_once(':').is_some_and(|(scheme, _)| {
        scheme.len() >= 2
            && scheme.as_bytes()[0].is_ascii_alphabetic()
            && scheme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    })
}

/// `text` with each `%HH` escape decoded, up to three times over (a link may be encoded more than
/// once), as bytes read back as UTF-8 (lossily): the form a refused link is shown in, so no
/// spelling of a secret hides it from [`cut_secrets`] (review M-11).
fn decoded(text: &str) -> String {
    let once = |s: &str| -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if b[i] == b'%'
                && i + 2 < b.len()
                && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
            {
                out.push(u8::try_from(h * 16 + l).unwrap_or(b'?'));
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    let mut s = text.to_string();
    for _ in 0..3 {
        let next = once(&s);
        if next == s {
            break;
        }
        s = next;
    }
    s
}

/// The shortest run of hex digits taken for a secret: a deny token is 32 (P4a-R9).
const SECRET_HEX: usize = 32;

/// `text` with what may be a deny token cut to `…`, however a refused link spells it (P4a-R9;
/// review M-1, M-11): every run of 32 or more hex digits, whatever surrounds it, and whatever
/// follows `deny` and one more separator (`/` or `\`, the id's) after the id.
fn cut_secrets(text: &str) -> String {
    // Runs of hex digits.
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= SECRET_HEX {
            out.push('…');
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    // What follows `deny<sep><id><sep>`, or `deny<sep>` and no id.
    let lower = out.to_ascii_lowercase();
    let is_sep = |c: char| c == '/' || c == '\\';
    let mut from = 0;
    while let Some(found) = lower[from..].find("deny") {
        let at = from + found + "deny".len();
        from = at;
        let rest = &out[at..];
        let Some(first) = rest.chars().next().filter(|&c| is_sep(c)) else {
            continue;
        };
        let after_sep = at + first.len_utf8();
        let id_ok = |s: &str| {
            s.strip_prefix("cf_")
                .is_some_and(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()))
        };
        let tail = &out[after_sep..];
        let cut = match tail.find(is_sep) {
            // `deny/<id>/<tail>`: the id stays.
            Some(end) if id_ok(&tail[..end]) => after_sep + end + 1,
            // `deny/<tail>` with no id.
            _ => after_sep,
        };
        if cut < out.len() {
            return format!("{}…", &out[..cut]);
        }
    }
    out
}

/// `text` without what a reader can't see and that can split a word: C0 and C1 controls and the
/// default-ignorable characters (`lil_parse::without_default_ignorables`; soft hyphen,
/// zero-width space and the like). The bidirectional controls stay, for `shown_text` to mark.
fn normalised(text: &str) -> String {
    lil_parse::without_default_ignorables(text)
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// What the Opened card says of a refused link: decoded, then normalised, then its secrets cut,
/// then at most 80 characters as shown text, in that order, so nothing inside a token hides it
/// from the cut (review M-11, M-18).
fn not_a_link(arg: &str) -> String {
    format!(
        "This link isn't one LILITH opens: {}",
        lil_parse::shown_text(&cut_secrets(&normalised(&decoded(arg))), SHOWN_CHARS)
    )
}

/// `text` from a link, a `.lilref`, a target or a title, ready to be shown: as it came, unless
/// [`cut_secrets`] finds something in its decoded and normalised form (M-18), and then that form,
/// cut. So `100%25 done.png` still reads as written, and no spelling of a token gets through.
fn without_secrets(text: &str) -> String {
    let plain = normalised(&decoded(text));
    let cut = cut_secrets(&plain);
    if cut == plain { text.to_string() } else { cut }
}

/// Such a text as a card, a banner or a log shows it: [`without_secrets`], then at most
/// `max_chars` of `lil_parse::shown_text` (P4a-R8, P4a-R9). `Core.shownLinkText` to the Opened
/// card, which so cuts what reaches it however it came.
pub fn shown_outside(text: &str, max_chars: usize) -> String {
    lil_parse::shown_text(&without_secrets(text), max_chars)
}

/// The longest target, in UTF-8 bytes, as an `open/` link carries (Appendix G).
const MAX_TARGET_BYTES: usize = 4096;

/// A target the card shows and plan 4b's Inspector opens, held to the `open/` rules whichever
/// way it came (review M-2): 1 to 4,096 bytes, no control character, and on this PC.
fn check_target(target: &str) -> Result<(), String> {
    if target.is_empty() || target.len() > MAX_TARGET_BYTES {
        return Err(format!(
            "This file or link is longer than {MAX_TARGET_BYTES} bytes, so LILITH doesn't open it."
        ));
    }
    if target.chars().any(char::is_control) {
        return Err(not_a_link(target));
    }
    if is_unc_like(target) {
        return Err("LILITH opens files from this PC only.".to_string());
    }
    Ok(())
}

/// What the one positional argument opens, or what the Opened card says instead.
fn classify(arg: &str, env: &Env) -> Result<Opened, String> {
    let lower = arg.to_ascii_lowercase();
    if lower.ends_with(".lilref") && !has_scheme(arg) {
        return lilref(arg);
    }
    if has_scheme(arg) {
        return match parse_lil_url(arg, &env.activation_scheme()) {
            Ok(LilUrl::Confirm(id)) => Ok(Opened::Confirm(id)),
            Ok(LilUrl::Deny { id, token }) => Ok(Opened::Deny { id, token }),
            Ok(LilUrl::Task(job)) => Ok(Opened::Task(job)),
            Ok(LilUrl::Open(target)) => check_target(&target).map(|()| Opened::Target(target)),
            Ok(LilUrl::Ref(r)) => Ok(Opened::Ref(r)),
            Err(_) => Err(not_a_link(arg)),
        };
    }
    if is_drive_absolute(arg) {
        return check_target(arg).map(|()| Opened::Target(arg.to_string()));
    }
    if is_unc_like(arg) {
        return Err("LILITH opens files from this PC only.".to_string());
    }
    Err(not_a_link(arg))
}

/// An open file's final path (`lil_platform::fs::final_path`) names a drive of this PC: `\\?\C:\…`,
/// never `\\?\UNC\…` (a share, a mapped drive) or anything else.
fn final_path_is_local(path: &str) -> bool {
    path.strip_prefix(r"\\?\").is_some_and(is_drive_absolute)
}

/// Reads a `.lilref` file: local only, a regular file of at most 4 KiB, UTF-8, and one
/// `lil_schema::parse_lilref` accepts.
fn lilref(arg: &str) -> Result<Opened, String> {
    if is_unc_like(arg) {
        return Err(LILREF_NOT_LOCAL.to_string());
    }
    if !is_drive_absolute(arg) {
        return Err("LILITH opens a .lilref file by its full path.".to_string());
    }
    check_target(arg)?;
    let path = Path::new(arg);
    let unreadable = || "LILITH couldn't read this .lilref file.".to_string();
    // A mapped network drive is a network share too, and a volume that can't be told is taken
    // for one: refused before the file is opened (review M-7).
    match lil_platform::fs::volume_info(path) {
        Ok(v) if !v.remote => {}
        Ok(_) => return Err(LILREF_NOT_LOCAL.to_string()),
        Err(_) => return Err(unreadable()),
    }
    // A link (symbolic, a junction, any reparse point) could lead off this PC: never followed.
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let meta = std::fs::symlink_metadata(path).map_err(|_| unreadable())?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(LILREF_NOT_LOCAL.to_string());
        }
    }
    let too_large = || {
        format!(
            "This .lilref file is larger than {LILREF_MAX_BYTES} bytes, so it isn't one LILITH opens."
        )
    };
    let file = std::fs::File::open(path).map_err(|_| unreadable())?;
    // A parent folder may be a link to a share, which the checks above can't see: where the
    // opened file really is decides, before a byte of it is read.
    let really = lil_platform::fs::final_path(&file).map_err(|_| unreadable())?;
    if !final_path_is_local(&really.to_string_lossy()) {
        return Err(LILREF_NOT_LOCAL.to_string());
    }
    match lil_platform::fs::volume_info(&really) {
        Ok(v) if !v.remote => {}
        Ok(_) => return Err(LILREF_NOT_LOCAL.to_string()),
        Err(_) => return Err(unreadable()),
    }
    let meta = file.metadata().map_err(|_| unreadable())?;
    if !meta.is_file() {
        return Err(unreadable());
    }
    if meta.len() > LILREF_MAX_BYTES {
        return Err(too_large());
    }
    // Read one byte past the limit, so a file that grew since is refused too.
    let mut bytes = Vec::new();
    file.take(LILREF_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    if bytes.len() as u64 > LILREF_MAX_BYTES {
        return Err(too_large());
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        "This .lilref file isn't one LILITH opens: it isn't UTF-8 text.".to_string()
    })?;
    let parsed = lil_schema::parse_lilref(&text).map_err(|why| {
        format!(
            "This .lilref file isn't one LILITH opens: {}",
            shown_outside(&why, 200)
        )
    })?;
    Ok(Opened::LilRef {
        reference: parsed.r#ref,
        title: parsed.title,
    })
}

/// What the window does with a plan, as the JSON `ShellWindow.handleLaunch` takes:
/// `{kind, intent, focus, activate, …}`, `kind` being `refused` (`message`), `target`, `ref` or
/// `lilref` (`target`, and `title` for a `.lilref`), `task` (`job`), `confirm` (`id`),
/// `confirm_list` or `activate` (nothing to show). `focus` is false under `--no-activate`;
/// `activate` false when the window was shown by the start itself; `pipe_notice` the banner's
/// text when this start runs without the app's pipe, else null. A deny is never shown, so it has none: `None`.
pub fn launch_json(plan: &BootPlan, activate: bool) -> Option<String> {
    let mut v = json!({
        "intent": plan.intent.as_str(),
        "focus": !plan.no_activate,
        "activate": activate,
        "pipe_notice": plan.pipe_notice,
    });
    let set = |v: &mut Value, fields: Value| {
        if let (Some(into), Value::Object(from)) = (v.as_object_mut(), fields) {
            into.extend(from);
        }
    };
    match (&plan.link_refused, &plan.opened, &plan.mode) {
        (Some(message), _, _) => set(&mut v, json!({"kind": "refused", "message": message})),
        (None, Some(Opened::Deny { .. }), _) => return None,
        (None, Some(Opened::Target(t)), _) => set(&mut v, json!({"kind": "target", "target": t})),
        (None, Some(Opened::Ref(r)), _) => set(&mut v, json!({"kind": "ref", "target": r})),
        (None, Some(Opened::LilRef { reference, title }), _) => set(
            &mut v,
            json!({"kind": "lilref", "target": reference, "title": title}),
        ),
        (None, Some(Opened::Task(job)), _) => set(&mut v, json!({"kind": "task", "job": job})),
        (None, Some(Opened::Confirm(id)), _) | (None, None, Mode::Confirm { id: Some(id) }) => {
            set(&mut v, json!({"kind": "confirm", "id": id}));
        }
        (None, None, Mode::Confirm { id: None }) => set(&mut v, json!({"kind": "confirm_list"})),
        (None, None, Mode::Shell) => set(&mut v, json!({"kind": "activate"})),
    }
    Some(v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lil_platform::{Channel, Roots};
    use std::path::PathBuf;

    fn env(channel: Channel) -> Env {
        let roots = Roots {
            roaming: PathBuf::from(r"C:\R"),
            local: PathBuf::from(r"C:\L"),
            user_profile: PathBuf::from(r"C:\P"),
        };
        Env::with_roots(channel, roots, "S-1-5-21-1-2-3-1001".to_string())
    }

    const EXE: &str = r#""C:\b\lil-image.exe""#;
    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const CF: &str = "cf_0123456789abcdef";
    const TOKEN: &str = "00112233445566778899aabbccddeeff";

    fn sealed() -> String {
        format!("lil://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0#sha256={HEX}")
    }

    fn plan(rest: &str) -> BootPlan {
        plan_boot(&format!("{EXE} {rest}"), &env(Channel::Dev))
    }

    fn b64(text: &str) -> String {
        lil_parse::lil_url::open_url("lil-dev", text)
    }

    #[test] // spec: APP-001
    fn boot_returns_shell_mode_by_default() {
        let plan = plan_boot(EXE, &env(Channel::Dev));
        assert_eq!(
            plan,
            BootPlan {
                mode: Mode::Shell,
                intent: Intent::Open,
                no_activate: false,
                smoke_exit: false,
                opened: None,
                link_refused: None,
                refused: None,
                forwarded: false,
                pipe_notice: None,
                org: "LILITH-dev".to_string(),
                app: "image".to_string(),
                qt_args: vec!["lil-image".to_string()],
                args: vec![],
            }
        );
        assert_eq!(plan_boot(EXE, &env(Channel::Stable)).org, "LILITH");
    }

    #[test] // spec: APP-001
    fn boot_reads_the_smoke_switches() {
        let plan = plan("--smoke-exit\t --no-activate ");
        assert!(plan.smoke_exit && plan.no_activate);
        assert_eq!(plan.refused, None);
        let plan = plan_boot(r"lil-image.exe --no-activate", &env(Channel::Dev));
        assert!(plan.no_activate && !plan.smoke_exit);
    }

    #[test] // spec: APP-020
    fn boot_refuses_anything_else_before_qt_starts() {
        for rest in [
            r#""lil:x" -platformpluginpath "\\host\share""#,
            "-platform offscreen",
            "-qmljsdebugger=port:1234",
            "--no-activate --no-activate",
            "--smoke-exit=1",
            r#""--no-activate""#,
            "--SMOKE-EXIT",
            "--intent",
            "--intent view",
            "--intent open --intent edit",
            "--confirm cf_0123 ",
            "--confirm --confirm",
            &format!("--confirm {CF} lil-dev:task/j_0123456789abcdef"),
            "lil-dev:task/j_0123456789abcdef --no-activate",
            r"C:\a b.png",
            r#""C:\a".png"#,
        ] {
            let plan = plan(rest);
            assert!(plan.refused.is_some(), "{rest}");
            assert!(!plan.refused.as_deref().unwrap().contains(rest), "{rest}");
            assert!(!plan.no_activate && !plan.smoke_exit, "{rest}");
            assert!(plan.opened.is_none() && plan.args.is_empty(), "{rest}");
        }
    }

    #[test] // spec: APP-020
    fn qt_gets_only_the_program_name() {
        for cmdline in [
            EXE.to_string(),
            format!("{EXE} --smoke-exit --no-activate"),
            format!(r#"{EXE} "lil:x" -platformpluginpath "\\host\share""#),
            format!(r#"{EXE} "lil-dev:task/j_0123456789abcdef""#),
            r#""\\host\share\lil-image.exe" -platform minimal"#.to_string(),
        ] {
            assert_eq!(
                plan_boot(&cmdline, &env(Channel::Dev)).qt_args,
                ["lil-image"],
                "{cmdline}"
            );
        }
    }

    #[test] // spec: APP-014, APP-020, APP-021, APP-066
    fn args_parse_every_form() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Crane Ref.LILREF");
        std::fs::write(
            &file,
            format!(
                r#"{{"lilref": 1, "ref": "{}", "title": "Paper crane"}}"#,
                sealed()
            ),
        )
        .unwrap();

        let p = plan("--intent edit --no-activate \"lil-dev:open/");
        // An unbalanced quote is two arguments' worth of trouble: refused.
        assert!(p.refused.is_some());

        let p = plan(&format!(
            "--intent edit --no-activate \"{}\"",
            b64(r"C:\art\crane.png")
        ));
        assert_eq!(p.intent, Intent::Edit);
        assert!(p.no_activate);
        assert_eq!(p.opened, Some(Opened::Target(r"C:\art\crane.png".into())));
        assert_eq!(
            p.args,
            [
                "--intent",
                "edit",
                "--no-activate",
                &b64(r"C:\art\crane.png")
            ]
        );
        assert_eq!(plan("--intent use_as_input").intent, Intent::UseAsInput);
        assert_eq!(plan("--intent open").intent, Intent::Open);

        let p = plan(CONFIRM);
        assert_eq!(p.mode, Mode::Confirm { id: None });
        assert_eq!(p.opened, None);
        let p = plan(&format!("--confirm {CF}"));
        assert_eq!(
            p.mode,
            Mode::Confirm {
                id: Some(CF.into())
            }
        );

        let p = plan(&format!("lil-dev:confirm/{CF}"));
        assert_eq!(
            p.mode,
            Mode::Confirm {
                id: Some(CF.into())
            }
        );
        assert_eq!(p.opened, Some(Opened::Confirm(CF.into())));
        let p = plan(&format!("\"LIL-DEV:deny/{CF}/{TOKEN}/\""));
        assert_eq!(
            p.opened,
            Some(Opened::Deny {
                id: CF.into(),
                token: TOKEN.into()
            })
        );
        assert_eq!(p.mode, Mode::Shell);
        let p = plan("lil-dev:task/j_0123456789abcdef");
        assert_eq!(p.opened, Some(Opened::Task("j_0123456789abcdef".into())));
        let raw = "lil://0123456789abcdef/sketchbook/chr_paper%5Fcrane@1.0.0";
        let p = plan(raw);
        assert_eq!(
            p.opened,
            Some(Opened::Ref(
                "lil://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0".into()
            ))
        );
        let p = plan(r#"  "C:\My Art\crane.png"  "#);
        assert_eq!(
            p.opened,
            Some(Opened::Target(r"C:\My Art\crane.png".into()))
        );
        assert_eq!(p.args, [r"C:\My Art\crane.png"]);
        let p = plan(&format!("\"{}\"", file.display()));
        assert_eq!(
            p.opened,
            Some(Opened::LilRef {
                reference: sealed(),
                title: Some("Paper crane".into())
            })
        );
        // Forwarded arguments read the same.
        let forwarded = plan_args(&p.args, &env(Channel::Dev));
        assert_eq!(forwarded.opened, p.opened);
        for p in [&p, &forwarded] {
            assert_eq!((&p.refused, &p.link_refused), (&None, &None));
        }
    }

    #[test] // spec: APP-020, APP-021, QA-050
    fn args_refuse_a_large_lilref_and_a_bad_hash() {
        let dir = tempfile::tempdir().unwrap();
        // A 1 GB sparse file (a file of that length; the bytes are never written).
        let big = dir.path().join("big.lilref");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(1 << 30).unwrap();
        drop(f);
        let started = std::time::Instant::now();
        let p = plan(&format!("\"{}\"", big.display()));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "not read whole"
        );
        assert_eq!(p.refused, None);
        assert_eq!(p.opened, None);
        assert!(
            p.link_refused
                .as_deref()
                .unwrap()
                .contains("larger than 4096 bytes"),
            "{p:?}"
        );
        // Just over the limit, by its bytes.
        let over = dir.path().join("over.lilref");
        std::fs::write(&over, vec![b' '; 4097]).unwrap();
        assert!(
            plan(&format!("\"{}\"", over.display()))
                .link_refused
                .unwrap()
                .contains("larger than")
        );
        // A hash that isn't 64 hex characters: parse_lilref's own words.
        let bad = dir.path().join("bad.lilref");
        let short = &HEX[..63];
        std::fs::write(
            &bad,
            format!(
                r#"{{"lilref": 1, "ref": "lil://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0#sha256={short}"}}"#
            ),
        )
        .unwrap();
        let p = plan(&format!("\"{}\"", bad.display()));
        assert_eq!(p.opened, None);
        let message = p.link_refused.unwrap();
        assert!(
            message.starts_with("This .lilref file isn't one LILITH opens: "),
            "{message}"
        );
        // No hash at all, and not JSON.
        let none = dir.path().join("none.lilref");
        std::fs::write(
            &none,
            r#"{"lilref": 1, "ref": "lil://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0"}"#,
        )
        .unwrap();
        assert!(
            plan(&format!("\"{}\"", none.display()))
                .link_refused
                .unwrap()
                .contains("sha256")
        );
        let junk = dir.path().join("junk.lilref");
        std::fs::write(&junk, [0xff, 0xfe, 0x00]).unwrap();
        assert!(
            plan(&format!("\"{}\"", junk.display()))
                .link_refused
                .is_some()
        );
        // A missing file and a folder named like one.
        let folder = dir.path().join("folder.lilref");
        std::fs::create_dir(&folder).unwrap();
        for missing in [dir.path().join("missing.lilref"), folder] {
            let p = plan(&format!("\"{}\"", missing.display()));
            assert_eq!(
                p.link_refused.as_deref(),
                Some("LILITH couldn't read this .lilref file.")
            );
        }
    }

    #[test] // spec: APP-021, QA-050
    fn args_refuse_a_network_lilref() {
        for path in [
            r"\\server\share\x.lilref",
            r"//server/share/x.lilref",
            r"\\?\UNC\server\share\x.lilref",
            r"\\.\pipe\x.lilref",
        ] {
            let p = plan(&format!("\"{path}\""));
            assert_eq!(p.refused, None, "{path}");
            assert_eq!(p.opened, None, "{path}");
            assert_eq!(p.link_refused.as_deref(), Some(LILREF_NOT_LOCAL), "{path}");
        }
        // A relative path names no file of this PC for sure either.
        let p = plan("crane.lilref");
        assert_eq!(
            p.link_refused.as_deref(),
            Some("LILITH opens a .lilref file by its full path.")
        );
    }

    #[test] // spec: APP-020, APP-022, QA-050
    fn a_refused_argument_never_answers_or_opens() {
        for rest in [
            format!("lil-dev:deny/{CF}/{}", &TOKEN[..31]),
            format!("lil-dev:deny/{CF}"),
            format!("lil:deny/{CF}/{TOKEN}"),
            format!("lil-test-x:confirm/{CF}"),
            "lil-dev:confirm/cf_XYZ".to_string(),
            "lil-dev:open/@@@".to_string(),
            "lil-dev:task/1".to_string(),
            "lil-dev:approve/cf_0123456789abcdef".to_string(),
            "https://example.com/".to_string(),
            "lil:task/1".to_string(),
            r"\\server\share\a.png".to_string(),
            "crane.png".to_string(),
            "x".repeat(50_000),
        ] {
            let p = plan(&format!("\"{rest}\""));
            assert_eq!(p.refused, None, "{rest}");
            assert_eq!(p.opened, None, "{rest}");
            assert_eq!(p.mode, Mode::Shell, "{rest}");
            let message = p.link_refused.clone().unwrap();
            assert!(!message.contains(TOKEN), "{message}");
            assert!(message.chars().count() <= 120, "{message}");
            // The window gets the message, never an action.
            let json: Value = serde_json::from_str(&launch_json(&p, true).unwrap()).unwrap();
            assert_eq!(json["kind"], "refused", "{rest}");
        }
    }

    #[test] // spec: APP-014, APP-065, P4a-R21
    fn launch_json_says_what_the_window_does() {
        let kind = |rest: &str| -> Value {
            serde_json::from_str(&launch_json(&plan(rest), true).unwrap()).unwrap()
        };
        assert_eq!(kind("")["kind"], "activate");
        assert_eq!(kind("--no-activate")["focus"], false);
        assert_eq!(kind("")["focus"], true);
        assert_eq!(kind("--confirm")["kind"], "confirm_list");
        assert_eq!(kind(&format!("--confirm {CF}"))["id"], CF);
        assert_eq!(
            kind("lil-dev:task/j_0123456789abcdef")["job"],
            "j_0123456789abcdef"
        );
        let t = kind(&format!("--intent edit {}", b64(r"C:\x.png")));
        assert_eq!(
            (t["kind"].as_str(), t["intent"].as_str()),
            (Some("target"), Some("edit"))
        );
        assert_eq!(
            launch_json(&plan(&format!("lil-dev:deny/{CF}/{TOKEN}")), true),
            None
        );
        let first: Value = serde_json::from_str(&launch_json(&plan(""), false).unwrap()).unwrap();
        assert_eq!(first["activate"], false);
    }

    #[test] // spec: APP-020
    fn smoke_exit_takes_no_link_and_no_confirm() {
        for rest in [
            "--smoke-exit lil-dev:task/j_0123456789abcdef".to_string(),
            format!("--smoke-exit \"lil-dev:deny/{CF}/{TOKEN}\""),
            "--smoke-exit --confirm".to_string(),
            format!("--confirm {CF} --smoke-exit"),
            "--smoke-exit --intent edit".to_string(),
            r#"--smoke-exit "C:\a.png""#.to_string(),
        ] {
            let p = plan(&rest);
            assert!(p.refused.is_some(), "{rest}");
            assert_eq!(p.opened, None, "{rest}");
        }
        let p = plan("--smoke-exit --no-activate");
        assert!(p.smoke_exit && p.no_activate && p.refused.is_none());
    }

    /// A deny token, every way it has been spelled to get past the cut.
    fn token_spellings() -> Vec<String> {
        vec![
            format!("lil-dev:DENY/{CF}/{TOKEN}"),
            format!("lil-dev:Deny%2F{CF}%2f{TOKEN}"),
            format!("lil-dev:deny%2f{CF}/{TOKEN}"),
            format!("lil:deny/{CF}/{TOKEN}"),
            format!("other:x/deny/{CF}/{TOKEN}"),
            format!("deny/{CF}/{TOKEN}"),
            // Review M-11's spellings.
            format!("lil-dev:deny\\{CF}\\{TOKEN}"),
            format!("lil-dev:deny%5C{CF}%5c{TOKEN}"),
            format!("lil-dev:deny%252F{CF}%252F{TOKEN}"),
            format!("lil-dev:de%6Ey/{CF}/{TOKEN}"),
            format!("lil-dev:%64%65%6e%79/{CF}/{TOKEN}"),
            format!("lil-dev:deny/{TOKEN}"),
            format!("lil-dev:x?t={}", TOKEN.to_uppercase()),
            format!("lil-dev:deny/{CF}/%30%30{}", &TOKEN[2..]),
            // Review M-18's spellings: a token split by an invisible character, outside any
            // deny/.
            format!("lil-dev:open/x?t={}%00{}", &TOKEN[..16], &TOKEN[16..]),
            format!("lil-dev:x/y?t={}%01{}", &TOKEN[..16], &TOKEN[16..]),
            format!(
                "https://example.com/?t={}%C2%85{}",
                &TOKEN[..16],
                &TOKEN[16..]
            ),
            format!("lil-dev:open/x?t={}%C2%AD{}", &TOKEN[..16], &TOKEN[16..]),
            format!("lil-dev:open/x?t={}%E2%80%8B{}", &TOKEN[..16], &TOKEN[16..]),
        ]
    }

    /// `message` shows nothing of the token.
    fn assert_no_token(rest: &str, message: &str) {
        let lower = message.to_ascii_lowercase();
        // Not the token, nor any 20 of its characters in a row.
        for start in 0..=12 {
            assert!(
                !lower.contains(&TOKEN[start..start + 20]),
                "{rest}: {message}"
            );
        }
        // Nor either half, however it was split (a marked U+200B between them included).
        for half in [&TOKEN[..16], &TOKEN[16..]] {
            assert!(!lower.contains(half), "{rest}: {message}");
        }
    }

    #[test] // spec: APP-020, P4a-R9
    fn a_refused_link_never_shows_a_token_however_it_is_spelled() {
        for rest in token_spellings() {
            let p = plan(&format!("\"{rest}\""));
            let message = p
                .link_refused
                .unwrap_or_else(|| panic!("{rest}: not refused"));
            assert_no_token(&rest, &message);
            if rest.contains(CF) {
                assert!(message.contains(CF), "{rest}: the id stays: {message}");
            }
        }
        assert_eq!(cut_secrets("lil-dev:open/@@@"), "lil-dev:open/@@@");
        assert_eq!(cut_secrets("deny/"), "deny/");
    }

    #[test] // spec: APP-014, APP-020
    fn a_refused_link_is_cut_before_it_goes_on_the_wire_and_stays_refused() {
        let scheme = "lil-dev";
        let task = "task/j_0123456789abcdef";
        for link in [
            // Each its worst case: 3-byte characters, and characters JSON writes in 6 bytes.
            format!("{scheme}:{}", "€".repeat(25_000)),
            format!("{scheme}:{}", "\u{1}".repeat(25_000)),
            // Keeping only its start must not make a link of it, or a target it would open.
            format!("{scheme}:{task}/{}", "x".repeat(25_000)),
            format!("C:\\{}", "x".repeat(25_000)),
            format!("C:\\{}.lilref", "x".repeat(25_000)),
            b64(&"x".repeat(25_000)),
        ] {
            let p = plan_args(std::slice::from_ref(&link), &env(Channel::Dev));
            let message = p
                .link_refused
                .as_deref()
                .unwrap_or_else(|| panic!("{link:.30}"));
            assert_eq!(p.opened, None, "{link:.30}");
            let wire = json!({"args": p.args}).to_string();
            assert!(
                wire.len() < crate::single_instance::MESSAGE_MAX,
                "{link:.30}"
            );
            let again = plan_args(&p.args, &env(Channel::Dev));
            assert_eq!(again.opened, None, "{link:.30}");
            // It says the same, but for what the cut itself took off a link it can't show.
            let start = |text: &str| text.chars().take(40).collect::<String>();
            assert_eq!(
                start(again.link_refused.as_deref().unwrap_or_default()),
                start(message),
                "{link:.30}"
            );
        }
        // One that fits goes as it came.
        let short = format!("{scheme}:{}", "x".repeat(100));
        assert_eq!(
            plan_args(std::slice::from_ref(&short), &env(Channel::Dev)).args,
            [short]
        );
    }

    #[test] // spec: APP-021, P4a-R9
    fn a_lilref_never_shows_a_token_however_it_is_spelled() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.lilref");
        let open = |text: String| -> BootPlan {
            std::fs::write(&file, text).unwrap();
            plan(&format!("\"{}\"", file.display()))
        };
        let exact = format!("lil-dev:deny/{CF}/{TOKEN}");
        for rest in std::iter::once(exact).chain(token_spellings()) {
            let refused = |text: Value, what: &str| {
                let p = open(text.to_string());
                let message = p
                    .link_refused
                    .unwrap_or_else(|| panic!("{what} {rest}: not refused"));
                assert_no_token(&rest, &message);
            };
            // Its ref, its version and a field of its own: parse_lilref repeats each.
            refused(json!({"lilref": 1, "ref": rest}), "ref");
            refused(json!({"lilref": rest, "ref": sealed()}), "version");
            refused(
                json!({"lilref": 1, "ref": sealed(), rest.clone(): 1}),
                "field",
            );
        }
    }

    #[test] // spec: APP-020, APP-021, P4a-R9
    fn a_target_or_a_title_as_shown_never_holds_a_token() {
        for rest in token_spellings() {
            assert_no_token(&rest, &shown_outside(&rest, 300));
            // As a title, and a part of a path or an open/ target.
            assert_no_token(&rest, &shown_outside(&format!("Paper crane {rest}"), 200));
            assert_no_token(&rest, &shown_outside(&format!("C:\\art\\{rest}.png"), 300));
        }
        // An accepted open/ target and drive path reach the window's card through it.
        for accepted in [
            format!(r"C:\art\deny\{CF}\{TOKEN}.png"),
            format!(r"C:\art\{TOKEN}.png"),
        ] {
            let opened = plan(&b64(&accepted)).opened;
            assert_eq!(
                opened,
                Some(Opened::Target(accepted.clone())),
                "kept as it came"
            );
            assert_no_token(&accepted, &shown_outside(&accepted, 300));
            assert_eq!(
                plan(&format!("\"{accepted}\"")).opened,
                Some(Opened::Target(accepted.clone()))
            );
        }
        // What has nothing to cut reads as it came, its % and all.
        for plain in [
            r"C:\art\100%25 done.png",
            "Paper crane",
            "deny",
            "a deny of b",
        ] {
            assert_eq!(shown_outside(plain, 300), plain);
        }
    }

    #[test] // spec: APP-020, QA-050
    fn a_target_is_held_to_the_open_rules_whichever_way_it_comes() {
        // A path with a control character, or longer than an open/ target may be.
        for target in [
            "C:\\a\u{7}b.png".to_string(),
            format!("C:\\{}.png", "x".repeat(4100)),
        ] {
            let p = plan_args(std::slice::from_ref(&target), &env(Channel::Dev));
            assert_eq!(p.opened, None, "{target:.20}");
            assert!(p.link_refused.is_some(), "{target:.20}");
        }
        // An open/ target naming a share is refused as a share path is.
        let p = plan(&b64(r"\\host\share\x.png"));
        assert_eq!(
            p.link_refused.as_deref(),
            Some("LILITH opens files from this PC only.")
        );
        assert_eq!(
            plan(r#""C:\a.png""#).opened,
            Some(Opened::Target(r"C:\a.png".into()))
        );
    }

    #[test] // spec: APP-021
    fn a_lilref_is_on_this_pc_only_when_its_final_path_says_so() {
        for local in [r"\\?\C:\a\x.lilref", r"\\?\d:/x.lilref"] {
            assert!(final_path_is_local(local), "{local}");
        }
        for not in [
            r"\\?\UNC\host\share\x.lilref",
            r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\x.lilref",
            r"\\host\share\x.lilref",
            r"\\.\C:\x.lilref",
            r"C:\x.lilref",
            r"\\?\",
            "",
        ] {
            assert!(!final_path_is_local(not), "{not}");
        }
    }

    #[test] // spec: APP-021
    fn a_lilref_behind_a_local_folder_link_is_read_from_where_it_really_is() {
        // A junction needs no privilege. It can only lead to this PC, so this shows the check on
        // a real handle, and that it lets such a file through.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(
            real.join("x.lilref"),
            format!(r#"{{"lilref": 1, "ref": "{}"}}"#, sealed()),
        )
        .unwrap();
        let link = dir.path().join("link");
        lil_platform::fs::create_junction(&link, &real).unwrap();
        let through = link.join("x.lilref");
        let file = std::fs::File::open(&through).unwrap();
        let really = lil_platform::fs::final_path(&file).unwrap();
        let really = really.to_string_lossy();
        assert!(final_path_is_local(&really), "{really}");
        assert!(really.ends_with(r"\real\x.lilref"), "{really}");
        assert!(plan(&format!("\"{}\"", through.display())).opened.is_some());
    }

    #[test] // spec: APP-021
    fn a_lilref_behind_a_folder_link_to_a_share_is_refused() {
        // A folder symbolic link needs Developer Mode or the privilege, and a share of this PC
        // an account allowed to reach it: without them the case is skipped, and
        // `a_lilref_is_on_this_pc_only_when_its_final_path_says_so` is the decision's test.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("x.lilref"),
            format!(r#"{{"lilref": 1, "ref": "{}"}}"#, sealed()),
        )
        .unwrap();
        let full = std::fs::canonicalize(dir.path()).unwrap();
        let full = full.to_string_lossy();
        let local = full.strip_prefix(r"\\?\").unwrap();
        let share = format!(r"\\localhost\{}$\{}", &local[..1], &local[3..]);
        let link = dir.path().join("link");
        if std::os::windows::fs::symlink_dir(&share, &link).is_err() {
            eprintln!("no folder symbolic link could be made here: case skipped");
            return;
        }
        let through = link.join("x.lilref");
        if std::fs::read(&through).is_err() {
            eprintln!("this PC's own share can't be reached here: case skipped");
            return;
        }
        // Refused either way: the volume check may fail on such a path before the file is opened,
        // or the final path says where it really is. It depends on the PC.
        let p = plan(&format!("\"{}\"", through.display()));
        assert_eq!(p.opened, None);
        let refused = p.link_refused.as_deref();
        assert!(
            refused == Some(LILREF_NOT_LOCAL)
                || refused == Some("LILITH couldn't read this .lilref file."),
            "{refused:?}"
        );
    }

    #[test] // spec: APP-021
    fn a_lilref_link_is_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.lilref");
        std::fs::write(&real, format!(r#"{{"lilref": 1, "ref": "{}"}}"#, sealed())).unwrap();
        assert!(plan(&format!("\"{}\"", real.display())).opened.is_some());
        let link = dir.path().join("link.lilref");
        // A symbolic link needs Developer Mode or the privilege: without it the case is skipped.
        if std::os::windows::fs::symlink_file(&real, &link).is_err() {
            eprintln!("no symbolic link could be made here: case skipped");
            return;
        }
        let p = plan(&format!("\"{}\"", link.display()));
        assert_eq!(p.opened, None);
        assert_eq!(p.link_refused.as_deref(), Some(LILREF_NOT_LOCAL));
    }
}
