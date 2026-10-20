//! The plugin hooks' PreToolUse patterns (spec ARCH-047, ARCH-056, ARCH-104, ARCH-105,
//! ARCH-106, CORE-008, CORE-116, QA-061; Appendix F "Plugin 1.3"; ruling P3-R28). Pure: std
//! and `serde_json` only, so `tests/hook_patterns.rs` compiles this file in and checks the
//! QA-061 table.
//!
//! **Conservative** (ruling P3-R28). Pattern matching is never complete, so a command line
//! gets no decision only when it is read to its end as the plain grammar below and nothing in
//! it is on the ask lists. Anything else asks, never allows:
//! - text the hook can't read to its end (an open quote, `$(`, here-string or comment);
//! - comments (`#`, PowerShell `<# … #>`), here-strings (`@'…'@`, `@"…"@`, Bash `<<` and
//!   `<<<`), Bash `$'…'` and typographic quotes (U+2018–U+201F, which PowerShell reads as
//!   quotes);
//! - a PowerShell hashtable (`@{`), a call operator (`&`, `.`), a static .NET call
//!   (`[IO.File]::Delete(…)`), an alias or function definition (`Set-Alias`/`sal`/`New-Alias`/
//!   `nal`, or `Set-Item`/`New-Item` on the `alias:`/`function:` drive), any `$ExecutionContext`
//!   use, or a method call on an expression (`(Get-Item x).Delete()`);
//! - Bash: a command word that is a shell expansion (`$x`, `${x}`), a `${…}` holding a command
//!   substitution, and `coproc`;
//! - interpreter re-entry: `cmd`, `pwsh`, `powershell`, `bash`, `sh`, `zsh`, `python*`, `py`,
//!   `node`, `perl`, `ruby`, `wsl`, `cscript`, `wscript`, `eval`, `Invoke-Expression`/`iex`,
//!   `Invoke-Command`, `Start-Process`/`saps`/`start`, `Invoke-Item`/`ii`, `source`, … (only a
//!   lone version or help flag such as `python --version` passes), also when text is piped or
//!   redirected into them;
//! - a program run by its path (`./x.sh`, `.\x.ps1`, `C:\tools\x.bat`, `/c/tools/x.sh`);
//! - an installer with a flag before its subcommand, or whose arguments name an install verb
//!   anywhere (`pip --index-url u install x`);
//! - the Appendix F lists: `lil-mcp`, `lil doctor` without `--non-interactive`, deletes,
//!   installs, script runs, and write-like commands that name a guarded `suite.toml`.
//!
//! **Commands.** A line is split at `;`, `&&`, `||`, `|`, a line end, `(` and `)`, `{` and `}`
//! (Bash also at `&` and `|&`; in PowerShell `&` is the call operator, and an assignment's `=`
//! starts the command on its right). Quotes are read as each shell reads them: Bash `'…'` and
//! `"…"` with `\` escapes, PowerShell `'…'` and `"…"` with backtick escapes; a Bash `\` outside
//! quotes escapes only a character the shell treats specially, so `C:\x\lil-mcp.exe` stays a
//! path. The text of every `$(…)` (and Bash `` `…` ``), quoted or not, is checked as a command
//! line of its own. Redirection targets are not words: write targets go to the `suite.toml`
//! rule, and an input redirection is remembered.
//!
//! **The command word** of each command is its first word after Bash `NAME=value`
//! assignments, keywords (`if`, `then`, `do`, `!`, …) and wrappers (`sudo`, `env`, `nohup`,
//! `exec`, `xargs`, …, with their flags). It is compared by its file name, case-insensitively,
//! without `.exe`, `.cmd`, `.bat` or `.com`. A PowerShell command whose first word is a quoted
//! string is an expression (`"y" | …`), not a command.
//!
//! **`suite.toml`** (ARCH-056, CORE-008). A path is guarded when, read as Windows reads it, it
//! is a `suite.toml` directly in a folder named `LILITH` or `LILITH-<channel>` (or the legacy
//! `Worldwright`/`Worldwright-<channel>` the suite still reads, M2): `.` and `..`
//! are resolved, trailing dots and spaces dropped, an alternate data stream (`::$DATA`)
//! dropped, case ignored. An 8.3 short name (`SUITE~1.TOM`, `LILITH~1`) or a wildcard in the
//! last two segments is resolved through the file system when the file is there (the long
//! name decides), and otherwise asks when it could name such a file. A `cd` into a guarded
//! folder makes a later relative `suite.toml` on the same line guarded too.
//!
//! **Known limits** (QA-061), stated plainly: a pattern guard reads the text, not the program.
//! It doesn't see a command behind an alias or a function defined in an earlier session (a
//! definition on the same line asks), a path built
//! at run time (`Join-Path`, a variable holding `suite.toml`), a `cd` on an earlier command
//! line, or what a program it allows does by itself. `cargo run -p lil-cli -- doctor` is not
//! `lil doctor` to it. If `lil` can't start at all, Claude Code runs the tool anyway (a hook
//! that fails to start is a non-blocking error); `lil doctor` checks that the hook runs.

use serde_json::Value;

/// Which shell a command is for: from the hook's `tool_name`. The token rules differ (quoting,
/// `\`, `&`, `{`, `=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    PowerShell,
}

/// What a PreToolUse hook says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// `permissionDecision: "ask"`, with the reason shown to the person.
    Ask(String),
    /// `permissionDecision: "deny"`, with the reason.
    Deny(String),
    /// A `systemMessage` warning, no decision.
    Warn(String),
    /// Nothing to say.
    None,
}

// ---------------------------------------------------------------------------------------------
// Tools.
// ---------------------------------------------------------------------------------------------

/// The comfy MCP's tool-name prefix (it comes from your own Claude Code configuration).
const COMFY: &str = "mcp__comfyui__";

/// The comfy MCP tools denied while a core runs (ARCH-106): the suite owns ComfyUI then.
const COMFY_DENIED_WITH_CORE: [&str; 5] = [
    "launch_comfyui",
    "stop_comfyui",
    "restart_comfyui",
    "partner_generate",
    "auth_login",
];

/// The comfy MCP tools that always ask (ARCH-105, CORE-116).
const COMFY_ASK: [&str; 5] = [
    "install_node",
    "update_comfyui",
    "switch_comfyui_version",
    "download_model",
    "free_memory",
];

/// Whether [`classify_tool`]'s answer for `tool_name` depends on whether a core runs: only
/// then does the hook look (`lil status --no-start`).
pub fn depends_on_core(tool_name: &str) -> bool {
    tool_name
        .strip_prefix(COMFY)
        .is_some_and(|t| COMFY_DENIED_WITH_CORE.contains(&t))
}

/// The answer for a core-dependent tool when the hook can't tell whether a core runs: ask
/// (fail closed, ruling P3-R28).
pub fn unknown_core(tool_name: &str) -> Decision {
    Decision::Ask(format!(
        "LILITH: the hook can't tell whether a LILITH core is running, so it can't tell whether \
         {tool_name} is turned off. Allow it?"
    ))
}

/// The decision for a tool call: never `allow` (ruling P3-R28). `tool_input.action` decides the
/// comfy MCP's `job` and `download`: `cancel` (any case, spaces ignored) or none asks, any other
/// gets no decision. A shell call without a `command` string, or a write without a
/// `file_path` string, is malformed and asks.
pub fn classify_tool(tool_name: &str, tool_input: &Value, core_running: bool) -> Decision {
    let missing = |field: &str| {
        Decision::Ask(format!(
            "LILITH: this {tool_name} call has no {field} the hook can read. Allow it?"
        ))
    };
    match tool_name {
        "Bash" | "PowerShell" => {
            let shell = if tool_name == "Bash" {
                Shell::Bash
            } else {
                Shell::PowerShell
            };
            return tool_input["command"]
                .as_str()
                .map_or_else(|| missing("command"), |c| classify_command(shell, c));
        }
        "Write" | "Edit" | "MultiEdit" => {
            return tool_input["file_path"]
                .as_str()
                .map_or_else(|| missing("file_path"), classify_file_write);
        }
        _ => {}
    }
    let Some(tool) = tool_name.strip_prefix(COMFY) else {
        return Decision::None;
    };
    if COMFY_DENIED_WITH_CORE.contains(&tool) {
        if core_running {
            return Decision::Deny(format!(
                "LILITH: a LILITH core is running and manages ComfyUI, so the comfy MCP's {tool} \
                 is turned off. Use the suite's engine tools (engines_status, engines_start, \
                 engines_stop) instead."
            ));
        }
        if tool == "restart_comfyui" {
            return Decision::Ask(
                "LILITH: restarting ComfyUI stops every job it is running. Allow it?".to_string(),
            );
        }
        return Decision::None;
    }
    if COMFY_ASK.contains(&tool) {
        return Decision::Ask(format!(
            "LILITH: the comfy MCP's {tool} installs, updates, downloads or frees memory in \
             ComfyUI. Allow it?"
        ));
    }
    if matches!(tool, "job" | "download") {
        let action = tool_input["action"].as_str();
        if action.is_none_or(|a| a.trim().eq_ignore_ascii_case("cancel")) {
            return Decision::Ask(format!(
                "LILITH: the comfy MCP's {tool} call cancels work (its action is {}). Allow it?",
                action.unwrap_or("not given")
            ));
        }
    }
    Decision::None
}

/// A Write, Edit or MultiEdit of `path`.
fn classify_file_write(path: &str) -> Decision {
    if is_guarded_suite_toml(path) {
        return Decision::Ask(SUITE_TOML_REASON.to_string());
    }
    let segments = normalized(path);
    let name = segments.last().cloned().unwrap_or_default();
    let parent = segments
        .len()
        .checked_sub(2)
        .map(|i| segments[i].clone())
        .unwrap_or_default();
    let log = name.ends_with(".jsonl")
        && ((parent == "queue" && name.starts_with("jobs"))
            || (parent == "registry" && name.starts_with("usage")));
    if log || name.ends_with(".manifest.json") {
        return Decision::Warn(format!(
            "LILITH: {path} is a file the suite keeps (a jobs or usage log, or an asset \
             manifest). While a LILITH core runs, make this change through the suite's lil MCP \
             tools instead of writing the file directly (lil status --no-start tells whether \
             one runs)."
        ));
    }
    Decision::None
}

const SUITE_TOML_REASON: &str = "LILITH: this changes a LILITH suite.toml, which holds your \
    spending caps and settings that only you may change. Allow it?";

// ---------------------------------------------------------------------------------------------
// suite.toml paths.
// ---------------------------------------------------------------------------------------------

/// `path`'s segments as Windows reads them, lower-cased: split at `/` and `\`, `.` and `..`
/// resolved, trailing dots and spaces dropped, and a `:stream` suffix dropped (not from a drive
/// such as `c:`).
fn normalized(path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in path.trim().split(['/', '\\']) {
        match raw {
            "" | "." => continue,
            ".." => {
                out.pop();
                continue;
            }
            _ => {}
        }
        let mut seg = raw.to_ascii_lowercase();
        let drive = seg.len() == 2 && seg.ends_with(':');
        if !drive && let Some(at) = seg.find(':') {
            seg.truncate(at);
        }
        let seg = seg.trim_end_matches(['.', ' ']).to_string();
        if !seg.is_empty() {
            out.push(seg);
        }
    }
    out
}

/// A folder a guarded `suite.toml` sits directly in (lower-cased): `LILITH` or
/// `LILITH-<channel>`, and the legacy `Worldwright` or `Worldwright-<channel>` the suite still
/// reads when the new file is missing (M2, R4), so a write to the legacy `suite.toml` is guarded
/// the same way.
fn is_guarded_folder(seg: &str) -> bool {
    for base in ["lilith", "worldwright"] {
        if seg == base {
            return true;
        }
        if let Some(rest) = seg.strip_prefix(base)
            && let Some(chan) = rest.strip_prefix('-')
            && !chan.is_empty()
        {
            return true;
        }
    }
    false
}

/// The segment may be an 8.3 short name or holds a wildcard: only the file system can tell.
fn uncertain(seg: &str) -> bool {
    seg.contains(['*', '?', '['])
        || seg
            .char_indices()
            .any(|(i, c)| c == '~' && seg[i + 1..].starts_with(|d: char| d.is_ascii_digit()))
}

/// Whether the wildcard pattern `pattern` (`*`, `?`, `[…]`) matches `text`.
fn glob(pattern: &[char], text: &[char]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some(('*', rest)) => (0..=text.len()).any(|i| glob(rest, &text[i..])),
        Some(('?', rest)) => !text.is_empty() && glob(rest, &text[1..]),
        Some(('[', rest)) => {
            let Some(end) = rest.iter().position(|&c| c == ']') else {
                return text.first() == Some(&'[') && glob(rest, &text[1..]);
            };
            let Some(&c) = text.first() else {
                return false;
            };
            let set = &rest[..end];
            let hit = set
                .windows(3)
                .any(|w| w[1] == '-' && w[0] <= c && c <= w[2])
                || set.contains(&c);
            hit && glob(&rest[end + 1..], &text[1..])
        }
        Some((c, rest)) => text.first() == Some(c) && glob(rest, &text[1..]),
    }
}

/// An uncertain segment could be `names`' one: a short name could be anything, a wildcard is
/// matched.
fn could_be(seg: &str, names: &[&str]) -> bool {
    if seg.contains('~') && !seg.contains(['*', '?', '[']) {
        return true;
    }
    let pattern: Vec<char> = seg.chars().collect();
    names
        .iter()
        .any(|n| glob(&pattern, &n.chars().collect::<Vec<_>>()))
}

/// The lexical rule on normalized segments.
fn names_guarded(segs: &[String]) -> bool {
    matches!(segs, [.., folder, file] if file == "suite.toml" && is_guarded_folder(folder))
}

/// A `suite.toml` directly in a `LILITH` or `LILITH-<channel>` folder, read as Windows reads
/// the path (module docs).
fn is_guarded_suite_toml(path: &str) -> bool {
    let segs = normalized(path);
    if names_guarded(&segs) {
        return true;
    }
    let [.., folder, file] = segs.as_slice() else {
        return false;
    };
    if !uncertain(folder) && !uncertain(file) {
        return false;
    }
    // The long name decides when the file is there.
    if let Ok(long) = std::fs::canonicalize(path.trim()) {
        return names_guarded(&normalized(&long.to_string_lossy()));
    }
    let folder_ok = is_guarded_folder(folder)
        || (uncertain(folder)
            && could_be(
                folder,
                &[
                    "lilith",
                    "lilith-dev",
                    "lilith-test-0",
                    "worldwright",
                    "worldwright-dev",
                ],
            ));
    let file_ok = file == "suite.toml" || (uncertain(file) && could_be(file, &["suite.toml"]));
    folder_ok && file_ok
}

/// A bare `suite.toml` (relative, one segment): guarded after a `cd` into a guarded folder.
fn is_bare_suite_toml(path: &str) -> bool {
    matches!(normalized(path).as_slice(), [file] if file == "suite.toml" || (uncertain(file) && could_be(file, &["suite.toml"])))
}

// ---------------------------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------------------------

/// The decision for a Bash or PowerShell command line: `Ask` with the first reason found, or
/// `None`.
pub fn classify_command(shell: Shell, command: &str) -> Decision {
    match first_reason(shell, command, 0) {
        Some(reason) => Decision::Ask(format!("LILITH: {reason}. Allow it?")),
        None => Decision::None,
    }
}

/// How deep `$(…)` substitutions are followed; deeper asks.
const MAX_DEPTH: usize = 8;

/// The quote characters PowerShell reads like `'` and `"`.
fn typographic_quote(c: char) -> bool {
    ('\u{2018}'..='\u{201F}').contains(&c)
}

/// A PowerShell line that runs code through an expression the lexer doesn't model (ruling
/// P3-R28 extended): any `$ExecutionContext` use, and any method call on an expression — a `)`
/// followed by `.<name>(`, as in `(Get-Command x).Invoke()` or `(Get-Item y).Delete()`.
fn powershell_expression_reason(line: &str) -> Option<String> {
    if line.to_ascii_lowercase().contains("$executioncontext") {
        return Some(
            "this uses $ExecutionContext, which can run any command the hook can't see".to_string(),
        );
    }
    let chars: Vec<char> = line.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c != ')' {
            continue;
        }
        let mut j = i + 1;
        while chars.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
        if chars.get(j) != Some(&'.') {
            continue;
        }
        j += 1;
        let start = j;
        while chars
            .get(j)
            .is_some_and(|c| c.is_alphanumeric() || *c == '_')
        {
            j += 1;
        }
        if j == start {
            continue;
        }
        while chars.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
        if chars.get(j) == Some(&'(') {
            return Some(
                "this calls a method on an expression, which can run any command the hook can't \
                 see"
                .to_string(),
            );
        }
    }
    None
}

fn first_reason(shell: Shell, line: &str, depth: usize) -> Option<String> {
    if depth > MAX_DEPTH {
        return Some("this nests command substitutions too deeply to read".to_string());
    }
    if line.chars().any(typographic_quote) {
        return Some("this uses typographic quotes, which PowerShell reads as quotes".to_string());
    }
    if shell == Shell::PowerShell
        && let Some(reason) = powershell_expression_reason(line)
    {
        return Some(reason);
    }
    let parsed = lex(shell, line);
    if let Some(problem) = parsed.problem {
        return Some(problem);
    }
    let mut in_lilith = false;
    for cmd in &parsed.commands {
        if let Some(reason) = command_reason(shell, cmd, in_lilith) {
            return Some(reason);
        }
        if let Some(dir) = cd_target(cmd) {
            in_lilith = normalized(dir).last().is_some_and(|f| is_guarded_folder(f));
        }
    }
    parsed
        .subs
        .iter()
        .find_map(|s| first_reason(shell, s, depth + 1))
}

/// The folder a `cd`-like command changes to.
fn cd_target(cmd: &Command) -> Option<&str> {
    let first = cmd.words.first()?;
    let name = command_name(&first.text);
    matches!(
        name.as_str(),
        "cd" | "chdir" | "pushd" | "set-location" | "sl" | "push-location"
    )
    .then(|| {
        cmd.words[1..]
            .iter()
            .map(|w| w.text.as_str())
            .find(|a| !a.starts_with('-'))
    })
    .flatten()
}

#[derive(Debug, Clone, Default)]
struct Word {
    text: String,
    /// It began with an unquoted `$` (a variable or a substitution).
    var: bool,
    /// It began with a quote.
    quoted: bool,
}

#[derive(Debug, Default)]
struct Command {
    words: Vec<Word>,
    /// The targets of its `>` and `>>` redirections.
    writes: Vec<String>,
    /// It has an input redirection (`<`).
    stdin: bool,
    /// It follows a `|`.
    piped: bool,
}

#[derive(Debug, Default)]
struct Parsed {
    commands: Vec<Command>,
    /// The text of each `$(…)` and Bash `` `…` ``.
    subs: Vec<String>,
    /// Why the line can't be read as the plain grammar: it then asks.
    problem: Option<String>,
}

/// The lexer's state while it reads one line.
struct Lexer {
    parsed: Parsed,
    cmd: Command,
    word: Word,
    started: bool,
    /// The next word is a redirection target: `Some(true)` for a write, `Some(false)` input.
    redirect: Option<bool>,
}

impl Lexer {
    fn push_char(&mut self, c: char) {
        self.started = true;
        self.word.text.push(c);
    }

    fn finish_word(&mut self) {
        if !self.started {
            return;
        }
        let word = std::mem::take(&mut self.word);
        self.started = false;
        match self.redirect.take() {
            Some(true) => self.cmd.writes.push(word.text),
            Some(false) => {}
            None => self.cmd.words.push(word),
        }
    }

    fn finish_command(&mut self, next_piped: bool) {
        self.finish_word();
        self.redirect = None;
        let cmd = std::mem::take(&mut self.cmd);
        if !cmd.words.is_empty() || !cmd.writes.is_empty() || cmd.stdin {
            self.parsed.commands.push(cmd);
        }
        self.cmd.piped = next_piped;
    }

    /// A quote or `$` that begins the word marks it.
    fn mark_start(&mut self, quoted: bool, var: bool) {
        if !self.started {
            self.word.quoted = quoted;
            self.word.var = var;
            self.started = true;
        }
    }

    /// The first problem found is kept.
    fn problem(&mut self, why: &str) {
        if self.parsed.problem.is_none() {
            self.parsed.problem = Some(why.to_string());
        }
    }
}

/// Characters a Bash `\` escapes outside quotes; before any other, the `\` is kept (a Windows
/// path).
const BASH_SPECIAL: &str = " \t\n\\'\"$`;&|<>()*?![]{}#~=%";

const UNREADABLE: &str = "this command line can't be read to its end (an open quote or \
    substitution)";

fn lex(shell: Shell, line: &str) -> Parsed {
    let chars: Vec<char> = line.chars().collect();
    let mut lx = Lexer {
        parsed: Parsed::default(),
        cmd: Command::default(),
        word: Word::default(),
        started: false,
        redirect: None,
    };
    let bash = shell == Shell::Bash;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            ' ' | '\t' => lx.finish_word(),
            '\n' | '\r' | ';' => lx.finish_command(false),
            '#' if !lx.started => {
                lx.problem("this has a comment, which the hook doesn't read past");
                break;
            }
            '<' if !bash && next == Some('#') => {
                lx.problem("this has a PowerShell block comment, which the hook doesn't read");
                break;
            }
            '@' if !bash && !lx.started && matches!(next, Some('\'' | '"')) => {
                lx.problem("this has a PowerShell here-string, which the hook doesn't read");
                break;
            }
            '@' if !bash && !lx.started && next == Some('{') => {
                lx.problem("this has a PowerShell hashtable, which can hide a command");
                break;
            }
            '|' => {
                if next == Some('|') {
                    lx.finish_command(false);
                    i += 1;
                } else {
                    if bash && next == Some('&') {
                        i += 1;
                    }
                    lx.finish_command(true);
                }
            }
            '&' => {
                if next == Some('&') {
                    lx.finish_command(false);
                    i += 1;
                } else if bash && next == Some('>') {
                    // `&>file`: both streams to a file.
                    lx.finish_word();
                    i = redirect(&mut lx, &chars, i + 1);
                    continue;
                } else if bash {
                    lx.finish_command(false);
                } else {
                    // PowerShell's call operator: a word of its own.
                    lx.finish_word();
                    lx.push_char('&');
                    lx.finish_word();
                }
            }
            '(' | ')' | '}' => lx.finish_command(false),
            // A PowerShell script block starts a command, glued to a word or not.
            '{' if !bash || !lx.started => lx.finish_command(false),
            '=' if !bash && !lx.started_with_assignee() => lx.push_char('='),
            '=' if !bash => {
                // `$x = …`, `$x=…`, `[t]$x = …`, `$a, $b = …`: the right side is a command.
                lx.finish_command(false);
            }
            '>' | '<' => {
                if bash && c == '<' && next == Some('<') {
                    lx.problem("this feeds a here-document or here-string to a command");
                    break;
                }
                // A word of digits (or PowerShell's `*`) right before it is the stream number.
                let fd = lx.started
                    && !lx.word.quoted
                    && (lx.word.text.chars().all(|c| c.is_ascii_digit()) || lx.word.text == "*");
                if fd {
                    lx.word = Word::default();
                    lx.started = false;
                } else {
                    lx.finish_word();
                }
                i = redirect(&mut lx, &chars, i);
                continue;
            }
            '\'' => {
                lx.mark_start(true, false);
                i += 1;
                let mut closed = false;
                while i < chars.len() {
                    if chars[i] == '\'' {
                        if !bash && chars.get(i + 1) == Some(&'\'') {
                            lx.word.text.push('\'');
                            i += 2;
                            continue;
                        }
                        closed = true;
                        break;
                    }
                    lx.word.text.push(chars[i]);
                    i += 1;
                }
                if !closed {
                    lx.problem(UNREADABLE);
                    break;
                }
            }
            '"' => {
                lx.mark_start(true, false);
                i += 1;
                let escape = if bash { '\\' } else { '`' };
                let mut closed = false;
                while i < chars.len() {
                    let d = chars[i];
                    if d == '"' {
                        closed = true;
                        break;
                    }
                    if d == escape && i + 1 < chars.len() {
                        lx.word.text.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if d == '$' && chars.get(i + 1) == Some(&'(') {
                        let Some((sub, end)) = capture_parens(&chars, i + 2) else {
                            lx.problem(UNREADABLE);
                            break;
                        };
                        lx.word.text.push_str(&format!("$({sub})"));
                        lx.parsed.subs.push(sub);
                        i = end;
                        continue;
                    }
                    if bash && d == '`' {
                        let Some((sub, end)) = capture_backticks(&chars, i + 1) else {
                            lx.problem(UNREADABLE);
                            break;
                        };
                        lx.parsed.subs.push(sub);
                        i = end;
                        continue;
                    }
                    lx.word.text.push(d);
                    i += 1;
                }
                if !closed {
                    lx.problem(UNREADABLE);
                    break;
                }
            }
            '\\' if bash => match next {
                // A line continuation.
                Some('\n') => i += 1,
                Some(n) if BASH_SPECIAL.contains(n) => {
                    lx.push_char(n);
                    i += 1;
                }
                _ => lx.push_char('\\'),
            },
            '`' if bash => {
                lx.mark_start(false, true);
                let Some((sub, end)) = capture_backticks(&chars, i + 1) else {
                    lx.problem(UNREADABLE);
                    break;
                };
                lx.parsed.subs.push(sub);
                i = end;
                continue;
            }
            '`' => {
                // PowerShell's escape; at a line end it continues the line.
                if let Some(n) = next {
                    if n != '\n' && n != '\r' {
                        lx.push_char(n);
                    }
                    i += 1;
                }
            }
            '$' => {
                if bash && next == Some('\'') {
                    lx.problem("this uses Bash $'…' quoting, which the hook doesn't read");
                    break;
                }
                lx.mark_start(false, true);
                match next {
                    Some('(') => {
                        let Some((sub, end)) = capture_parens(&chars, i + 2) else {
                            lx.problem(UNREADABLE);
                            break;
                        };
                        lx.word.text.push_str(&format!("$({sub})"));
                        lx.parsed.subs.push(sub);
                        i = end;
                        continue;
                    }
                    Some('{') => {
                        let Some(p) = chars[i..].iter().position(|&c| c == '}') else {
                            lx.problem(UNREADABLE);
                            break;
                        };
                        let body: String = chars[i..=i + p].iter().collect();
                        // A `${…}` that hides a command substitution isn't read inside: ask
                        // (ruling P3-R28 extended).
                        if body.contains("$(") || body.contains('`') {
                            lx.problem(
                                "this nests a command substitution inside a ${…} expansion, which \
                                 the hook doesn't read",
                            );
                            break;
                        }
                        lx.word.text.push_str(&body);
                        i += p + 1;
                        continue;
                    }
                    _ => lx.word.text.push('$'),
                }
            }
            _ => lx.push_char(c),
        }
        i += 1;
    }
    lx.finish_command(false);
    lx.parsed
}

impl Lexer {
    /// PowerShell: the command so far is the left side of an assignment (`$x`, `[t]$x`,
    /// `$a,$b`), so an `=` here ends it.
    fn started_with_assignee(&self) -> bool {
        let first = self
            .cmd
            .words
            .first()
            .map(|w| (w.text.as_str(), w.var, w.quoted));
        let current = (self.word.text.as_str(), self.word.var, self.word.quoted);
        let (text, var, quoted) = first.unwrap_or(current);
        !quoted && (var || text.starts_with('['))
    }
}

/// Reads the redirection at `chars[i]` (`>`, `>>`, `>|`, `>&2`, `<`) and marks the next word
/// as its target; returns where reading goes on.
fn redirect(lx: &mut Lexer, chars: &[char], mut i: usize) -> usize {
    let write = chars[i] == '>';
    if !write {
        lx.cmd.stdin = true;
    }
    i += 1;
    while i < chars.len() && (chars[i] == '>' || chars[i] == '|') {
        i += 1;
    }
    if chars.get(i) == Some(&'&') {
        // `>&2`, `2>&1`: another stream, no file.
        i += 1;
        if chars
            .get(i)
            .is_some_and(|c| c.is_ascii_digit() || *c == '-')
        {
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '-') {
                i += 1;
            }
            return i;
        }
    }
    lx.redirect = Some(write);
    i
}

/// The text up to the `)` that closes a `(` just before `start`, and the index after it;
/// `None` when it never closes.
fn capture_parens(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut depth = 1;
    let mut i = start;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((chars[start..i].iter().collect(), i + 1));
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// The text up to the next unescaped `` ` ``, and the index after it; `None` when it never
/// closes.
fn capture_backticks(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut i = start;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == '`' {
            return Some((chars[start..i].iter().collect(), i + 1));
        }
        i += 1;
    }
    None
}

/// Bash keywords before a command word.
const KEYWORDS: [&str; 9] = [
    "if", "then", "else", "elif", "do", "while", "until", "!", "time",
];

/// Wrappers that run the command after them.
const WRAPPERS: [&str; 11] = [
    "sudo", "env", "nohup", "exec", "nice", "xargs", "command", "builtin", "timeout", "stdbuf",
    "noglob",
];

/// Interpreters and launchers: they run code or another program, so they always ask unless
/// their only argument is a version or help flag (ruling P3-R28).
const INTERPRETERS: &[&str] = &[
    "cmd",
    "pwsh",
    "powershell",
    "powershell_ise",
    "bash",
    "sh",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "py",
    "node",
    "deno",
    "bun",
    "perl",
    "ruby",
    "php",
    "lua",
    "wsl",
    "cscript",
    "wscript",
    "mshta",
    "rundll32",
    "eval",
    "source",
    "invoke-expression",
    "iex",
    "invoke-command",
    "icm",
    "start-process",
    "saps",
    "start",
    "invoke-item",
    "ii",
    ".",
];

/// The flags that only print a version or help.
const VERSION_FLAGS: &[&str] = &[
    "--version",
    "-version",
    "-v",
    "--help",
    "-h",
    "/?",
    "-?",
    "-help",
    "--help-all",
];

/// The command word's comparable name: its file name, lower-cased, without `.exe`, `.cmd`,
/// `.bat` or `.com`.
fn command_name(word: &str) -> String {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let lower = base.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stem) = lower.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    lower
}

fn is_python(name: &str) -> bool {
    name == "py"
        || name.strip_prefix("python").is_some_and(|rest| {
            rest.chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == 'w')
        })
}

/// A flag of an installer: `-…`, `/…` on Windows tools, or cargo's `+toolchain`.
fn is_flag(a: &str) -> bool {
    a.starts_with('-') || a.starts_with('+')
}

/// The arguments that aren't flags.
fn positionals<'a>(args: &[&'a str]) -> Vec<&'a str> {
    args.iter().copied().filter(|a| !is_flag(a)).collect()
}

fn command_reason(shell: Shell, cmd: &Command, in_lilith: bool) -> Option<String> {
    let guarded = |t: &str| is_guarded_suite_toml(t) || (in_lilith && is_bare_suite_toml(t));
    if cmd.writes.iter().any(|t| guarded(t)) {
        return Some("this writes to a LILITH suite.toml (your caps and settings)".to_string());
    }
    let mut words: &[Word] = &cmd.words;
    loop {
        let first = words.first()?;
        let t = first.text.as_str();
        let assignment = shell == Shell::Bash
            && !first.quoted
            && t.split_once('=').is_some_and(|(n, _)| {
                !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
        if assignment || (shell == Shell::Bash && KEYWORDS.contains(&t)) {
            words = &words[1..];
            continue;
        }
        let name = command_name(t);
        if shell == Shell::Bash && WRAPPERS.contains(&name.as_str()) {
            if name == "command"
                && words
                    .get(1)
                    .is_some_and(|w| w.text == "-v" || w.text == "-V")
            {
                return None;
            }
            words = &words[1..];
            while words
                .first()
                .is_some_and(|w| w.text.starts_with('-') || (name == "env" && w.text.contains('=')))
            {
                words = &words[1..];
            }
            if name == "timeout" && !words.is_empty() {
                words = &words[1..];
            }
            continue;
        }
        break;
    }
    let first = &words[0];
    let args: Vec<&str> = words[1..].iter().map(|w| w.text.as_str()).collect();
    if shell == Shell::PowerShell {
        if first.text == "&" || first.text == "." {
            return Some(format!(
                "this uses PowerShell's call operator {}, which runs whatever follows",
                first.text
            ));
        }
        if words
            .iter()
            .any(|w| !w.quoted && w.text.starts_with('[') && w.text.contains("]::"))
        {
            return Some("this calls a .NET method directly".to_string());
        }
    }
    if first.var {
        // Bash: the command word is an expansion (`$x`, `${x}`), so the hook can't tell what
        // runs — ask (ruling P3-R28 extended). PowerShell: a bare `$x` only outputs the
        // variable; what it holds isn't run here.
        if shell == Shell::Bash {
            return Some(
                "the command word is a shell expansion, so the hook can't tell what runs"
                    .to_string(),
            );
        }
        return None;
    }
    if shell == Shell::PowerShell && first.quoted {
        // A quoted PowerShell string is an expression.
        return None;
    }
    let raw = first.text.as_str();
    let name = command_name(raw);
    if shell == Shell::Bash && name == "coproc" {
        return Some(
            "`coproc` runs a command in the background, which the hook doesn't read".into(),
        );
    }
    if shell == Shell::PowerShell
        && let Some(reason) = powershell_alias_reason(&name, &args)
    {
        return Some(reason);
    }
    if let Some(reason) = write_like_suite_toml(&name, &args, &guarded) {
        return Some(reason);
    }
    if INTERPRETERS.contains(&name.as_str()) || is_python(&name) {
        let only_version = args.len() == 1
            && VERSION_FLAGS.contains(&args[0].to_ascii_lowercase().as_str())
            && !cmd.stdin
            && !cmd.piped;
        if !only_version {
            return Some(format!(
                "`{name}` runs code or another program (an interpreter or launcher)"
            ));
        }
        return None;
    }
    if raw.contains(['/', '\\']) {
        return Some(format!("this runs the program or script {raw} by its path"));
    }
    named_reason(&name, &args)
}

/// A PowerShell command that defines an alias or a function, which hides the command it runs
/// (ruling P3-R28 extended): `Set-Alias`/`sal`/`New-Alias`/`nal`, or `Set-Item`/`New-Item`
/// pointed at the `alias:` or `function:` provider.
fn powershell_alias_reason(name: &str, args: &[&str]) -> Option<String> {
    if matches!(name, "set-alias" | "sal" | "new-alias" | "nal") {
        return Some(format!(
            "`{name}` defines an alias, which can stand for any command the hook can't see"
        ));
    }
    if matches!(name, "set-item" | "si" | "new-item" | "ni")
        && args.iter().any(|a| {
            let lower = a.to_ascii_lowercase();
            let bare = lower.strip_prefix('-').is_none();
            bare && (lower.starts_with("alias:") || lower.starts_with("function:"))
        })
    {
        return Some(format!(
            "`{name}` writes to the alias: or function: drive, which defines a command the hook \
             can't see"
        ));
    }
    None
}

/// The rule for write-like commands that name a guarded `suite.toml`: in an argument, after an
/// argument's `=` (`of=…`) or a PowerShell parameter's `:` (`-Path:…`), or as a folder argument
/// beside a `suite.toml` argument (`robocopy src <LILITH folder> suite.toml`).
fn write_like_suite_toml(
    name: &str,
    args: &[&str],
    guarded: &dyn Fn(&str) -> bool,
) -> Option<String> {
    const WRITE_LIKE: &[&str] = &[
        "set-content",
        "add-content",
        "out-file",
        "clear-content",
        "new-item",
        "remove-item",
        "move-item",
        "rename-item",
        "copy-item",
        "tee-object",
        "rm",
        "del",
        "erase",
        "mv",
        "cp",
        "tee",
        "dd",
        "truncate",
        "ln",
        "install",
        "robocopy",
        "xcopy",
        "copy",
        "move",
        "ren",
        "rename",
        "invoke-webrequest",
        "iwr",
        "invoke-restmethod",
        "irm",
        "curl",
        "wget",
        // PowerShell's aliases of the above.
        "sc",
        "ac",
        "clc",
        "ni",
        "ri",
        "mi",
        "rni",
        "cpi",
        "mklink",
    ];
    let sed_in_place = name == "sed"
        && args
            .iter()
            .any(|a| a.starts_with("-i") || a.starts_with("--in-place"));
    if !(sed_in_place || WRITE_LIKE.contains(&name)) {
        return None;
    }
    let parts = |a: &str| -> Vec<String> {
        let mut v = vec![a.to_string()];
        if let Some((_, rest)) = a.split_once('=') {
            v.push(rest.to_string());
        }
        if a.starts_with('-')
            && let Some((_, rest)) = a.split_once(':')
        {
            v.push(rest.to_string());
        }
        v
    };
    let named = args.iter().flat_map(|a| parts(a)).any(|p| guarded(&p));
    let split = args
        .iter()
        .any(|a| normalized(a).last().is_some_and(|f| is_guarded_folder(f)))
        && args.iter().any(|a| is_bare_suite_toml(a));
    (named || split)
        .then(|| "this changes a LILITH suite.toml (your caps and settings)".to_string())
}

/// An installer's install verbs (and the verbs that run a package): any one among its
/// arguments asks, and so does a flag before its subcommand (ruling P3-R28).
fn install_verbs(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "pip" | "pip3" => &["install", "download"],
        n if n.starts_with("pip3.") => &["install", "download"],
        "uv" => &["add", "install", "run", "sync"],
        "pipx" => &["install", "run", "inject", "upgrade"],
        "conda" | "mamba" | "micromamba" => &["install", "create", "update", "run"],
        "npm" => &[
            "install",
            "i",
            "in",
            "ci",
            "add",
            "run",
            "run-script",
            "exec",
            "x",
            "update",
        ],
        "pnpm" => &["add", "install", "i", "dlx", "exec", "run", "update"],
        "yarn" => &["add", "install", "dlx", "run"],
        "cargo" => &["install"],
        "gem" | "go" => &["install"],
        "winget" => &["install", "add", "upgrade"],
        "choco" | "scoop" => &["install", "upgrade", "update"],
        "comfy" => &["install", "registry-install", "reinstall", "update"],
        _ => return None,
    })
}

/// The command-word rules (Appendix F, "Plugin 1.3").
fn named_reason(name: &str, args: &[&str]) -> Option<String> {
    let reason = |what: &str| Some(format!("`{name}` {what}"));
    let lower: Vec<String> = args.iter().map(|a| a.to_ascii_lowercase()).collect();
    let has = |flag: &str| lower.iter().any(|a| a == flag);
    if let Some(verbs) = install_verbs(name) {
        let pos = positionals(args);
        let flag_first = args.first().is_some_and(|a| is_flag(a)) && !pos.is_empty();
        if pos.iter().any(|p| verbs.contains(p)) || flag_first {
            return reason("installs software or runs a package (ask before installs)");
        }
        return None;
    }
    match name {
        "lil-mcp" => {
            Some("this starts lil-mcp, the suite's MCP server, outside Claude Code".to_string())
        }
        "lil" => {
            let sub = lil_subcommand(args);
            (sub == Some("doctor") && !has("--non-interactive")).then(|| {
                "lil doctor's repairs wait for a y that only you may type; run it yourself, or \
                 with --non-interactive"
                    .to_string()
            })
        }
        "rm" | "del" | "erase" | "rd" | "rmdir" | "remove-item" | "ri" | "unlink" | "shred" => {
            reason("deletes files")
        }
        "find"
            if lower.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"
                )
            }) =>
        {
            reason("deletes or runs commands on the files it finds")
        }
        "git" => {
            if args
                .windows(2)
                .any(|w| w[0] == "-c" && w[1].contains("alias."))
            {
                return reason("defines an alias on the fly, which can run any command");
            }
            match git_subcommand(args) {
                Some("clean") => reason("clean deletes untracked files"),
                Some("reset") if has("--hard") => reason("reset --hard throws away changes"),
                _ => None,
            }
        }
        "install-module" | "install-package" | "install-script" | "install-psresource" => {
            reason("installs software")
        }
        "blender" if args.iter().any(|a| *a == "-P" || a.starts_with("--python")) => {
            reason("runs a Python script inside Blender")
        }
        "npx" | "uvx" | "pnpx" | "bunx" => reason("downloads and runs a package"),
        _ => None,
    }
}

/// `lil`'s subcommand: the first argument that isn't a global flag (or `--workspace`'s value).
fn lil_subcommand<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if a == "--workspace" {
            i += 2;
            continue;
        }
        if !a.starts_with('-') {
            return Some(a);
        }
        i += 1;
    }
    None
}

/// git's subcommand, after its global options (`-C <path>`, `-c <name=value>` and the rest).
fn git_subcommand<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if matches!(a, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace") {
            i += 2;
            continue;
        }
        if !a.starts_with('-') {
            return Some(a);
        }
        i += 1;
    }
    None
}
