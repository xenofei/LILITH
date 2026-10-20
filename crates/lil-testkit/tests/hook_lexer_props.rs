//! Property tests of the plugin hook's command lexer (QA-061, ARCH-105, CORE-116; Appendix F
//! "Plugin 1.3"; ruling P3-R28), over command lines built from a small shell grammar: plain
//! words, quoted words and separators, with one marker spliced in where the shell reads it as
//! a marker. The vocabulary is Appendix F's: the interpreter words, comments, here-strings,
//! typographic quotes and Bash `$'…'`. No command list is written by hand.
//!
//! - The hook never answers `allow`: [`Decision`] has no such answer, and the match below
//!   names every answer it has, so adding one fails to compile here.
//! - A line holding a marker always asks (or denies).
//!
//! `crates/lil-cli/src/user/hook_patterns.rs` is pure (std and `serde_json` only), so it is
//! compiled in here, as `crates/lil-cli/tests/hook_patterns.rs` does. The test lives in this
//! crate because `lil-cli`'s manifest (a protected file) has no `proptest`. The case count comes
//! from `test-params.toml` (`lil_testkit::params::proptest_cases`).

#[allow(dead_code, reason = "only the classifiers are used here")]
#[path = "../../lil-cli/src/user/hook_patterns.rs"]
mod hook_patterns;

use hook_patterns::{Decision, Shell, classify_command, classify_tool};
use proptest::prelude::*;
use serde_json::json;

fn config() -> ProptestConfig {
    ProptestConfig::with_cases(lil_testkit::params::proptest_cases())
}

fn shell() -> impl Strategy<Value = Shell> {
    prop_oneof![Just(Shell::Bash), Just(Shell::PowerShell)]
}

/// A plain word: bare, or in single or double quotes with nothing either shell expands.
fn word() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z][a-z0-9_.-]{0,7}",
        "[a-z0-9 _.-]{0,8}".prop_map(|w| format!("'{w}'")),
        "[a-z0-9 _.-]{0,8}".prop_map(|w| format!("\"{w}\"")),
    ]
}

/// A plain command: one to four words.
fn command() -> impl Strategy<Value = String> {
    prop::collection::vec(word(), 1..5).prop_map(|w| w.join(" "))
}

/// What both shells read as the end of one command and the start of the next.
fn separator() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        Just(" ; "),
        Just(" && "),
        Just(" || "),
        Just(" | "),
        Just("\n")
    ]
}

/// Plain commands joined by separators (possibly none).
fn commands() -> impl Strategy<Value = Vec<(String, &'static str)>> {
    prop::collection::vec((command(), separator()), 0..3)
}

/// `name` with each letter's case chosen by `upper`.
fn recase(name: &str, upper: &[bool]) -> String {
    name.chars()
        .zip(upper.iter().cycle())
        .map(|(c, up)| {
            if *up {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

/// Appendix F's interpreter words (ruling P3-R28), as a command word may be written: any case,
/// by a path, with `.exe`.
fn interpreter_word() -> impl Strategy<Value = String> {
    let name = prop::sample::select(vec![
        "cmd",
        "pwsh",
        "powershell",
        "bash",
        "sh",
        "python",
        "python3",
        "py",
        "node",
        "eval",
        "Invoke-Expression",
        "iex",
        "Start-Process",
    ]);
    let folder = prop::sample::select(vec!["", r"C:\tools\", "/usr/bin/", "./"]);
    let ext = prop::sample::select(vec!["", ".exe", ".EXE"]);
    (
        name,
        prop::collection::vec(any::<bool>(), 1..8),
        folder,
        ext,
    )
        .prop_map(|(name, upper, folder, ext)| format!("{folder}{}{ext}", recase(name, &upper)))
}

/// The interpreter's arguments: none, or anything but a lone version or help flag (the one
/// form that passes).
fn interpreter_args() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        "[a-z0-9_]{1,8}".prop_map(|w| format!(" {w}")),
        prop::collection::vec(word(), 2..4).prop_map(|w| format!(" {}", w.join(" "))),
    ]
}

/// An interpreter run where the shell reads a command: as a command of the line (Bash also
/// after `NAME=value` assignments), inside `$(…)`, inside Bash backticks, or in a PowerShell
/// script block.
fn interpreter(shell: Shell) -> impl Strategy<Value = String> {
    let bash = shell == Shell::Bash;
    let assign = if bash {
        "([A-Z]{1,4}=[a-z0-9]{0,4} ){0,2}".boxed()
    } else {
        Just(String::new()).boxed()
    };
    let run = (assign, interpreter_word(), interpreter_args())
        .prop_map(|(assign, word, args)| format!("{assign}{word}{args}"));
    let place = if bash {
        prop::sample::select(vec!["plain", "subst", "backticks"]).boxed()
    } else {
        prop::sample::select(vec!["plain", "subst", "block"]).boxed()
    };
    (run, place, word()).prop_map(|(run, place, w)| match place {
        "subst" => format!("echo {w} $({run})"),
        "backticks" => format!("echo {w} `{run}`"),
        "block" => format!("foreach-object {{ {run} }}"),
        _ => run,
    })
}

/// A PowerShell call operator before a word (`& x`, `. x`).
fn call_operator() -> impl Strategy<Value = String> {
    (prop::sample::select(vec!["&", "."]), word()).prop_map(|(op, w)| format!("{op} {w}"))
}

/// Printable ASCII text: what may follow a comment or fill a here-string.
fn text() -> impl Strategy<Value = String> {
    "[ -~]{0,16}"
}

/// A comment where the shell reads one: `#` at the start of a word (both shells), or a
/// PowerShell block comment.
fn comment(shell: Shell) -> impl Strategy<Value = String> {
    let hash = (word(), text()).prop_map(|(w, t)| format!("echo {w} #{t}"));
    if shell == Shell::Bash {
        hash.boxed()
    } else {
        prop_oneof![
            hash,
            (word(), text()).prop_map(|(w, t)| format!("echo {w} <#{t}#> {w}")),
        ]
        .boxed()
    }
}

/// A here-string or here-document where the shell reads one.
fn here_string(shell: Shell) -> impl Strategy<Value = String> {
    if shell == Shell::Bash {
        prop_oneof![
            (word(), "[A-Z]{1,5}", text())
                .prop_map(|(w, tag, t)| format!("cat {w} <<{tag}\n{t}\n{tag}")),
            (word(), "[A-Z]{1,5}", text())
                .prop_map(|(w, tag, t)| format!("cat {w} <<-{tag}\n{t}\n{tag}")),
            (word(), word()).prop_map(|(w, v)| format!("cat {w} <<<{v}")),
        ]
        .boxed()
    } else {
        prop_oneof![
            (word(), text()).prop_map(|(w, t)| format!("echo {w} @'\n{t}\n'@")),
            (word(), text()).prop_map(|(w, t)| format!("echo {w} @\"\n{t}\n\"@")),
        ]
        .boxed()
    }
}

/// A line that holds a marker the hook must ask about: plain commands, the marked command,
/// plain commands.
fn marked_line() -> impl Strategy<Value = (Shell, String)> {
    shell().prop_flat_map(|shell| {
        let mut markers = vec![
            interpreter(shell).boxed(),
            comment(shell).boxed(),
            here_string(shell).boxed(),
        ];
        if shell == Shell::PowerShell {
            markers.push(call_operator().boxed());
        }
        let marker = prop::strategy::Union::new(markers);
        (Just(shell), commands(), marker, commands()).prop_map(|(shell, before, marker, after)| {
            let mut line = String::new();
            for (cmd, sep) in &before {
                line.push_str(cmd);
                line.push_str(sep);
            }
            line.push_str(&marker);
            for (cmd, _) in &after {
                // A comment or a here-string runs to its line end: what follows starts a line.
                line.push('\n');
                line.push_str(cmd);
            }
            (shell, line)
        })
    })
}

/// A line of plain commands with one quote character the hook can't read past spliced in at
/// any place: a typographic quote (U+2018–U+201F, which PowerShell reads as a quote) in either
/// shell, or Bash `$'`.
fn unreadable_line() -> impl Strategy<Value = (Shell, String)> {
    (
        shell(),
        command(),
        any::<prop::sample::Index>(),
        0x2018_u32..=0x201F,
    )
        .prop_map(|(shell, cmd, at, quote)| {
            let chars: Vec<char> = cmd.chars().collect();
            let at = at.index(chars.len() + 1);
            let quote = char::from_u32(quote).unwrap().to_string();
            let mut line: String = chars[..at].iter().collect();
            line.push_str(&quote);
            line.extend(&chars[at..]);
            (shell, line)
        })
}

/// Bash `$'…'` quoting as a word of its own.
fn ansi_c_line() -> impl Strategy<Value = String> {
    (command(), "[a-z0-9 ]{0,8}").prop_map(|(cmd, t)| format!("{cmd} $'{t}'"))
}

/// Any line: the grammar's, with or without a marker, or arbitrary text.
fn any_line() -> impl Strategy<Value = (Shell, String)> {
    prop_oneof![
        (shell(), commands(), command()).prop_map(|(shell, before, last)| {
            let mut line: String = before.iter().map(|(c, s)| format!("{c}{s}")).collect();
            line.push_str(&last);
            (shell, line)
        }),
        marked_line(),
        unreadable_line(),
        (shell(), "\\PC{0,48}"),
        (shell(), "[ -~\n]{0,48}"),
    ]
}

fn asks_or_denies(d: &Decision) -> bool {
    matches!(d, Decision::Ask(_) | Decision::Deny(_))
}

proptest! {
    #![proptest_config(config())]

    /// No command line gets `allow`: every answer is one of these four, none of which grants
    /// anything, through the command classifier and through the tool classifier alike.
    #[test] // spec: QA-061, ARCH-105, CORE-116
    fn no_command_line_is_allowed((shell, line) in any_line(), core in any::<bool>()) {
        let tool = if shell == Shell::Bash { "Bash" } else { "PowerShell" };
        for decision in [
            classify_command(shell, &line),
            classify_tool(tool, &json!({"command": line}), core),
        ] {
            match decision {
                Decision::Ask(_) | Decision::Deny(_) | Decision::Warn(_) | Decision::None => {}
            }
        }
    }

    /// A line holding an interpreter word, a comment, a here-string or a call operator where
    /// the shell reads one asks.
    #[test] // spec: QA-061, ARCH-105, CORE-116
    fn a_line_with_a_marker_asks((shell, line) in marked_line()) {
        let got = classify_command(shell, &line);
        prop_assert!(asks_or_denies(&got), "{shell:?} {line:?}: {got:?}");
    }

    /// A typographic quote anywhere, or Bash `$'…'`, asks: the hook can't read such a line.
    #[test] // spec: QA-061, ARCH-105
    fn a_line_the_hook_cant_read_asks((shell, line) in unreadable_line(), ansi in ansi_c_line()) {
        let got = classify_command(shell, &line);
        prop_assert!(asks_or_denies(&got), "{shell:?} {line:?}: {got:?}");
        let got = classify_command(Shell::Bash, &ansi);
        prop_assert!(asks_or_denies(&got), "Bash {ansi:?}: {got:?}");
    }
}
