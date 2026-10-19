//! The plugin hooks' PreToolUse patterns (spec ARCH-047, ARCH-056, ARCH-104, ARCH-105,
//! ARCH-106, CORE-008, CORE-116, QA-061; Appendix F "Plugin 1.3"). Pure: std and `serde_json`
//! only, so `tests/hook_patterns.rs` compiles this file in and checks the QA-061 table.
//!
//! **Commands.** A Bash or PowerShell command line is split into commands at `;`, `&&`, `||`,
//! `|`, a line end, `(`, `)`, `{` and `}` (Bash also at `&` and `|&`; in PowerShell `&` is the
//! call operator). Quotes are read as each shell reads them: Bash `'…'` and `"…"` with `\`
//! escapes, PowerShell `'…'` and `"…"` with backtick escapes; a Bash `\` outside quotes escapes
//! only a character the shell treats specially, so `C:\x\lil-mcp.exe` stays a path. The text of
//! every `$(…)` (and Bash `` `…` ``), quoted or not, is checked as a command line of its own.
//! `#` at a word start begins a comment. Redirections (`>`, `>>`, `2>`, `*>`, `<`) are not words:
//! their write targets are kept for the `suite.toml` rule, `2>&1` and `<` input are dropped.
//!
//! **The command word** of each command is its first word after Bash `NAME=value` assignments,
//! Bash keywords (`if`, `then`, `do`, `!`, …) and the wrappers `sudo`, `env`, `nohup`, `time`,
//! `exec`, `nice`, `xargs`, `command`, `builtin`, `timeout`, `stdbuf` and `noglob` (with their
//! flags). It is compared by its file name, case-insensitively, without `.exe`, `.cmd`, `.bat`
//! or `.com`, so `C:\b\lil.exe` is `lil`. A PowerShell command whose first word is a quoted
//! string is an expression (`"y" | …`), not a command; `$x = …` is an assignment.
//!
//! **What asks** (Appendix F): `lil-mcp`; `lil doctor` without `--non-interactive`, however its
//! input arrives (`echo y | lil doctor`, `lil doctor < answers.txt`); deletes; installs; script
//! runs; and any write-like command that names a `suite.toml` in a `LILITH` or
//! `LILITH-<channel>` folder. Reads of that file get no decision from these rules (the repo's
//! deny rules cover private paths).
//!
//! **Known limits** (QA-061), stated plainly: a pattern guard reads the text, not the program.
//! It doesn't see a command word held in a variable (`$x = "rm"; & $x f`), inside a script it
//! runs (`bash -c "…"` asks as a script run, but its text isn't read), behind an alias or
//! function, built from pieces (`r''m`), in a file a program reads, or a write made by a
//! program itself (`python -c "open(…, 'w')"` asks only as a script run; `[IO.File]::
//! WriteAllText(…)` on `suite.toml` passes). `cargo run -p lil-cli -- doctor` is not `lil
//! doctor` to it. A heredoc's body is read as commands.

use serde_json::Value;

/// Which shell a command is for: from the hook's `tool_name`. The token rules differ (quoting,
/// `\`, `&`).
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

/// The decision for a tool call. `tool_input.action` decides the comfy MCP's `job` and
/// `download`: `cancel` or none asks, any other gets no decision.
pub fn classify_tool(tool_name: &str, tool_input: &Value, core_running: bool) -> Decision {
    let command = || tool_input["command"].as_str().unwrap_or_default();
    match tool_name {
        "Bash" => return classify_command(Shell::Bash, command()),
        "PowerShell" => return classify_command(Shell::PowerShell, command()),
        "Write" | "Edit" | "MultiEdit" => {
            return tool_input["file_path"]
                .as_str()
                .map_or(Decision::None, classify_file_write);
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
        if action.is_none_or(|a| a == "cancel") {
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
    let segments = segments(path);
    let name = segments
        .last()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let parent = segments
        .len()
        .checked_sub(2)
        .map(|i| segments[i].to_ascii_lowercase())
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

/// The path's segments, split at `/` and `\`.
fn segments(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).filter(|s| !s.is_empty()).collect()
}

/// A `suite.toml` directly in a folder named `LILITH` or `LILITH-<channel>` (any case).
fn is_guarded_suite_toml(path: &str) -> bool {
    let s = segments(path.trim());
    let [.., folder, file] = s.as_slice() else {
        return false;
    };
    let folder = folder.to_ascii_lowercase();
    file.eq_ignore_ascii_case("suite.toml")
        && (folder == "lilith"
            || folder
                .strip_prefix("lilith-")
                .is_some_and(|c| !c.is_empty()))
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

/// How deep `$(…)` substitutions are followed.
const MAX_DEPTH: usize = 8;

fn first_reason(shell: Shell, line: &str, depth: usize) -> Option<String> {
    if depth > MAX_DEPTH {
        return None;
    }
    let parsed = lex(shell, line);
    parsed
        .commands
        .iter()
        .find_map(|c| command_reason(shell, c))
        .or_else(|| {
            parsed
                .subs
                .iter()
                .find_map(|s| first_reason(shell, s, depth + 1))
        })
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
    /// It follows a `|`.
    piped: bool,
}

#[derive(Debug, Default)]
struct Parsed {
    commands: Vec<Command>,
    /// The text of each `$(…)` and Bash `` `…` ``.
    subs: Vec<String>,
}

/// The lexer's state while it reads one line.
struct Lexer {
    parsed: Parsed,
    cmd: Command,
    word: Word,
    started: bool,
    /// The next word is a redirection target: `Some(true)` for a write.
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
        if !cmd.words.is_empty() || !cmd.writes.is_empty() {
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
}

/// Characters a Bash `\` escapes outside quotes; before any other, the `\` is kept (a Windows
/// path).
const BASH_SPECIAL: &str = " \t\n\\'\"$`;&|<>()*?![]{}#~=%";

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
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
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
            '{' if !lx.started => lx.finish_command(false),
            '>' | '<' => {
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
                while i < chars.len() {
                    if chars[i] == '\'' {
                        if !bash && chars.get(i + 1) == Some(&'\'') {
                            lx.word.text.push('\'');
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    lx.word.text.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                lx.mark_start(true, false);
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    let d = chars[i];
                    let escape = if bash { '\\' } else { '`' };
                    if d == escape && i + 1 < chars.len() {
                        lx.word.text.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if d == '$' && chars.get(i + 1) == Some(&'(') {
                        let (sub, end) = capture_parens(&chars, i + 2);
                        lx.word.text.push_str(&format!("$({sub})"));
                        lx.parsed.subs.push(sub);
                        i = end;
                        continue;
                    }
                    if bash && d == '`' {
                        let (sub, end) = capture_backticks(&chars, i + 1);
                        lx.parsed.subs.push(sub);
                        i = end;
                        continue;
                    }
                    lx.word.text.push(d);
                    i += 1;
                }
            }
            '\\' if bash => {
                match next {
                    // A line continuation.
                    Some('\n') => i += 1,
                    Some(n) if BASH_SPECIAL.contains(n) => {
                        lx.push_char(n);
                        i += 1;
                    }
                    _ => lx.push_char('\\'),
                }
            }
            '`' if bash => {
                lx.mark_start(false, true);
                let (sub, end) = capture_backticks(&chars, i + 1);
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
                lx.mark_start(false, true);
                match next {
                    Some('(') => {
                        let (sub, end) = capture_parens(&chars, i + 2);
                        lx.word.text.push_str(&format!("$({sub})"));
                        lx.parsed.subs.push(sub);
                        i = end;
                        continue;
                    }
                    Some('{') => {
                        let end = chars[i..]
                            .iter()
                            .position(|&c| c == '}')
                            .map_or(chars.len(), |p| i + p + 1);
                        lx.word.text.extend(&chars[i..end]);
                        i = end;
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

/// Reads the redirection at `chars[i]` (`>`, `>>`, `>|`, `>&2`, `<`, `<<`) and marks the next
/// word as its target; returns where reading goes on.
fn redirect(lx: &mut Lexer, chars: &[char], mut i: usize) -> usize {
    let write = chars[i] == '>';
    i += 1;
    while i < chars.len() && (chars[i] == '>' || chars[i] == '<' || chars[i] == '|') {
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

/// The text up to the `)` that closes a `(` just before `start`, and the index after it.
fn capture_parens(chars: &[char], start: usize) -> (String, usize) {
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
                        return (chars[start..i].iter().collect(), i + 1);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    (chars[start..].iter().collect(), chars.len())
}

/// The text up to the next unescaped `` ` ``, and the index after it.
fn capture_backticks(chars: &[char], start: usize) -> (String, usize) {
    let mut i = start;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == '`' {
            return (chars[start..i].iter().collect(), i + 1);
        }
        i += 1;
    }
    (chars[start..].iter().collect(), chars.len())
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

/// Whether `w` names a file by a path (it has a separator or a drive colon).
fn is_path(w: &str) -> bool {
    w.contains(['/', '\\', ':'])
}

fn is_python(name: &str) -> bool {
    name == "py"
        || name.strip_prefix("python").is_some_and(|rest| {
            rest.chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == 'w')
        })
}

/// The arguments that don't start with `-`.
fn positionals<'a>(args: &[&'a str]) -> Vec<&'a str> {
    args.iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect()
}

fn command_reason(shell: Shell, cmd: &Command) -> Option<String> {
    if cmd.writes.iter().any(|t| is_guarded_suite_toml(t)) {
        return Some("this writes to a LILITH suite.toml (your caps and settings)".to_string());
    }
    let mut words: &[Word] = &cmd.words;
    // PowerShell: `$x = …` assigns; what follows is an expression or a command.
    if shell == Shell::PowerShell
        && words.len() >= 2
        && words[0].var
        && words[1].text.ends_with('=')
        && !words[1].quoted
    {
        words = &words[2..];
    }
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
    if shell == Shell::PowerShell && (first.text == "&" || (first.text == "." && !args.is_empty()))
    {
        // `& <path>` and `. <path>` run a program or a script by path.
        let operand = words.get(1)?;
        if operand.var {
            return None;
        }
        if is_path(&operand.text) || operand.quoted {
            return Some(format!("this runs {} by its path", operand.text));
        }
        let rest = Command {
            words: words[1..].to_vec(),
            writes: Vec::new(),
            piped: cmd.piped,
        };
        return command_reason(shell, &rest);
    }
    if first.var || (shell == Shell::PowerShell && first.quoted) {
        // A variable's value isn't known; a quoted PowerShell string is an expression.
        return None;
    }
    let raw = first.text.as_str();
    let name = command_name(raw);
    if let Some(reason) = write_like_suite_toml(&name, &args) {
        return Some(reason);
    }
    if cmd.piped && (is_python(&name) || matches!(name.as_str(), "sh" | "bash" | "iex")) {
        return Some(format!("this pipes text into {name}, which runs it"));
    }
    if ["./", ".\\", "../", "..\\"]
        .iter()
        .any(|p| raw.starts_with(p))
    {
        return Some(format!("this runs the script or program {raw}"));
    }
    named_reason(&name, &args)
}

/// The rule for write-like commands that name a guarded `suite.toml`.
fn write_like_suite_toml(name: &str, args: &[&str]) -> Option<String> {
    const WRITE_LIKE: [&str; 23] = [
        "set-content",
        "add-content",
        "out-file",
        "clear-content",
        "new-item",
        "remove-item",
        "move-item",
        "rename-item",
        "copy-item",
        "rm",
        "del",
        "mv",
        "cp",
        "tee",
        // PowerShell's aliases of the above.
        "ac",
        "clc",
        "ni",
        "ri",
        "mi",
        "move",
        "rni",
        "ren",
        "cpi",
    ];
    let sed_in_place = name == "sed"
        && args
            .iter()
            .any(|a| a.starts_with("-i") || a.starts_with("--in-place"));
    let write_like = sed_in_place || WRITE_LIKE.contains(&name) || name == "copy";
    (write_like && args.iter().any(|a| is_guarded_suite_toml(a)))
        .then(|| "this changes a LILITH suite.toml (your caps and settings)".to_string())
}

/// The command-word rules (Appendix F, "Plugin 1.3").
fn named_reason(name: &str, args: &[&str]) -> Option<String> {
    let pos = positionals(args);
    let first = pos.first().copied().unwrap_or_default();
    let reason = |what: &str| Some(format!("`{name}` {what}"));
    let lower: Vec<String> = args.iter().map(|a| a.to_ascii_lowercase()).collect();
    let has = |flag: &str| lower.iter().any(|a| a == flag);
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
        "rm" | "del" | "erase" | "rd" | "rmdir" | "remove-item" | "ri" => reason("deletes files"),
        "git" => match git_subcommand(args) {
            Some("clean") => reason("clean deletes untracked files"),
            Some("reset") if has("--hard") => reason("reset --hard throws away changes"),
            _ => None,
        },
        "pip" | "pip3" if first == "install" => reason("installs packages"),
        n if n.starts_with("pip3.") && first == "install" => reason("installs packages"),
        "uv" if (first == "pip" && pos.get(1) == Some(&"install")) || first == "add" => {
            reason("installs packages")
        }
        "npm" if matches!(first, "install" | "i" | "ci" | "add") => reason("installs packages"),
        "npm" if matches!(first, "run" | "run-script") => reason("runs a package script"),
        "pnpm" if matches!(first, "add" | "install" | "i") => reason("installs packages"),
        "yarn" if first == "add" => reason("installs packages"),
        "cargo" | "winget" | "choco" | "scoop" if first == "install" => reason("installs software"),
        "install-module" | "install-package" => reason("installs software"),
        "comfy"
            if pos
                .windows(2)
                .any(|w| w[0] == "node" && matches!(w[1], "install" | "registry-install")) =>
        {
            reason("installs a ComfyUI custom node")
        }
        n if is_python(n) => python_reason(name, args),
        "node" if !first.is_empty() || has("-e") || has("--eval") => reason("runs a script"),
        "bash" | "sh" | "zsh"
            if !first.is_empty()
                || args
                    .iter()
                    .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c')) =>
        {
            reason("runs a script")
        }
        "cmd" if has("/c") || has("/k") => reason("runs a command line"),
        "pwsh" | "powershell" if pwsh_runs_script(&lower) => reason("runs a script"),
        "blender" if args.iter().any(|a| *a == "-P" || a.starts_with("--python")) => {
            reason("runs a Python script inside Blender")
        }
        "invoke-expression" | "iex" => reason("runs text as code"),
        "source" | "." if !first.is_empty() => reason("runs a script"),
        "npx" | "uvx" => reason("downloads and runs a package"),
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

/// Python: a file or `-c` runs a script; `-m pip install` installs; `-m <other>` and flags
/// alone (`--version`) get no decision.
fn python_reason(name: &str, args: &[&str]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        match a {
            "-c" => return Some(format!("`{name} -c` runs a script")),
            "-m" => {
                let module = args.get(i + 1).copied().unwrap_or_default();
                let rest = positionals(args.get(i + 2..).unwrap_or_default());
                return (module == "pip" && rest.first() == Some(&"install"))
                    .then(|| format!("`{name} -m pip install` installs packages"));
            }
            "-X" | "-W" | "-Q" => i += 2,
            a if a.starts_with('-') => i += 1,
            _ => return Some(format!("`{name}` runs the script {a}")),
        }
    }
    None
}

/// PowerShell with `-Command`, `-File`, `-EncodedCommand` (or their short forms), or a script
/// as its first positional argument.
fn pwsh_runs_script(lower: &[String]) -> bool {
    const SCRIPT: [&str; 9] = [
        "-c",
        "-command",
        "-f",
        "-file",
        "-e",
        "-ec",
        "-encodedcommand",
        "-commandwithargs",
        "-cwa",
    ];
    const VALUED: [&str; 8] = [
        "-executionpolicy",
        "-ex",
        "-workingdirectory",
        "-wd",
        "-windowstyle",
        "-w",
        "-configurationname",
        "-settingsfile",
    ];
    let mut i = 0;
    while i < lower.len() {
        let a = lower[i].as_str();
        if SCRIPT.contains(&a) || a.starts_with("-command:") || a.starts_with("-file:") {
            return true;
        }
        if VALUED.contains(&a) {
            i += 2;
            continue;
        }
        if !a.starts_with('-') {
            return true;
        }
        i += 1;
    }
    false
}
