//! Claude Code project-settings deny patterns (QA-036, QA-079; Task 13).
//!
//! The suite repo's `.claude/settings.json` denies the commands an agent could use to land a
//! change on `main` without `lil dev merge`, to skip the git hooks, or to reach a GitHub login
//! broader than the agent token. This test reads the committed file and checks a table of
//! commands against it: every row in `MUST_MATCH` must be denied, and no row in
//! `MUST_NOT_MATCH` may be. Plan 3's plugin hook-pattern tests (QA-061) extend these tables.
//!
//! **Matching semantics.** [`rule_matches`] and [`subcommands`] reimplement Claude Code's
//! documented rule matching, from <https://code.claude.com/docs/en/permissions> (read
//! 2026-10-13), sections "Wildcard patterns", "Compound commands", "Wrappers",
//! "What a Bash rule doesn't match" and "PowerShell":
//!
//! - A `*` matches any text, spaces included, at any position. Text outside the wildcards
//!   must match as written.
//! - A trailing `:*` is the same as a trailing ` *`. Elsewhere the colon is a literal.
//! - A trailing ` *` that is the rule's only wildcard also matches the bare command
//!   (`Bash(ls *)` matches `ls`). With a second wildcard it does not
//!   (`Bash(* --help *)` doesn't match `npm --help`). That is why `Bash(git push * main:*)`
//!   alone misses `git push -u origin main` and the list also carries `Bash(git push * main)`.
//! - Bash: compound commands are split on `&&`, `||`, `;`, `|`, `|&`, `&` and newlines, and a
//!   deny rule applies when any subcommand matches, including one inside a subshell or a
//!   command substitution. The wrappers `timeout`, `time`, `nice`, `nohup`, `stdbuf`,
//!   `command` (not `command -v`), `builtin`, `noglob` and flag-less `xargs` are stripped, and
//!   deny rules match past any leading `NAME=value` assignment. The docs don't say Bash rules
//!   ignore case, so this helper compares Bash rules case-sensitively.
//! - PowerShell: the same rule shapes; matching is case-insensitive; compound commands are
//!   split on `|`, `;`, `&&` and `||`. (Alias canonicalisation doesn't matter for `git`, `gh`
//!   and `cmdkey`; for `Get-Content` the list names the `gc`, `cat` and `type` aliases itself.)
//!
//! **The owner's key and denylist folder** (`%USERPROFILE%\.lilith`, and the legacy
//! `%USERPROFILE%\.worldwright` while it exists; R38, R4). `Read` and `Edit` deny rules keep
//! agents out of both; every rule that names the legacy folder has a twin that names the new one
//! (Task B5). [`path_rule_matches`] reimplements the docs' section "Read and Edit" (read
//! 2026-10-14):
//!
//! - path rules use gitignore patterns: `*` matches within one path segment, `**` across
//!   segments; `~/path` is anchored at the home directory and `//path` at the filesystem root;
//! - on Windows, paths are normalised to POSIX form before matching (`C:\Users\<name>` becomes
//!   `/c/Users/<name>`), and `//**/name` matches on every drive, so `//**/.lilith/**` (and
//!   `//**/.worldwright/**`) also covers a profile that isn't the home directory Claude Code
//!   sees. It also covers a workspace's own `.lilith\` (and `.worldwright\`) data folder;
//! - a `Read` deny rule also blocks the Edit and Write tools on the same path; `Edit` rules
//!   cover every built-in editing tool (NotebookEdit included). A `Write(path)` rule is never
//!   consulted (Claude Code warns about it at startup), so there is none: `Read` and `Edit`
//!   cover writes;
//! - `Read` and `Edit` deny rules also apply to file commands Claude Code recognises in Bash
//!   (`cat`, `head`, `tail`, `sed`, `tee`) and to redirection targets, but not to commands that
//!   read files without naming them or to subprocesses that open files themselves.
//!
//! The Bash and PowerShell rules `cat`, `type`, `Get-Content` and `gc` of any path containing
//! `.lilith` (or `.worldwright`) are a second, text-only layer on top.
//!
//! **Known limits of pattern guards.** A deny rule matches the command text the agent writes,
//! not the program it runs, so none of this is a security boundary. Commands the patterns
//! don't stop include, among others:
//!
//! - other spellings: `git 'push' origin main`, `/usr/bin/git`,
//!   `& 'C:\Program Files\Git\cmd\git.exe' push ...`, a mixed-case config key in Bash
//!   (`core.HooksPath`; git config keys ignore case), a push to `main` with options before
//!   `push` and a force flag bundled three deep (`git push -uvf`);
//! - indirection: `env git push ...` (`env` isn't a stripped wrapper), `cmd /c ...`,
//!   `bash -c '...'`, `sh -c '...'`, `pwsh -c '...'`, `bash script.sh`, a gh alias defined
//!   before these rules (`gh alias set` itself is denied), a git or shell alias, a Makefile or
//!   cargo alias, `git -c alias.p=push p origin main`, environment variables (`GIT_CONFIG_*`)
//!   or a file edit that changes `.git/config`;
//! - pushes that never name `main`: `git push` with an upstream of `main`, `git push origin
//!   HEAD` while on `main`, `git push --all`, `git push --mirror`;
//! - API calls the patterns can't read: `gh api --input body.json graphql`, `curl` with the
//!   token;
//! - reads of `%USERPROFILE%\.lilith` (or the legacy `%USERPROFILE%\.worldwright`) from a shell
//!   remain leaky: `Select-String`, `more`, `less`, `copy`/`cp` elsewhere and then a read,
//!   `cd ~/.lilith` and then a relative name, a path built from variables or quotes
//!   (`~/.lil''ith`), a mixed-case `.Lilith` or `.WorldWright` (NTFS ignores case; the path
//!   rules here are matched as written), and any script or program (`python`, `node`,
//!   `pwsh -c`) that opens the files itself. The private denylist and the encrypted reviewer key
//!   must not rely on these rules alone.
//!
//! **Direct `gh` (QA-087).** The owner's full-access GitHub login stays on this PC, because other
//! projects need it. Inside this repo, git uses only the agent token (a repo-local credential
//! helper), and agents reach GitHub's CLI only through `lil dev gh -- <args>`, which runs `gh` with
//! the agent token in the child process and refuses merge, auth and write API calls. A plain `gh`
//! run by an agent would use the owner's login, so every spelling the matcher can read is denied:
//! `gh *` (bare `gh` too), `gh.exe *`, and a path to either, unquoted (`*/gh *`, `*/gh.exe *`,
//! `*\gh.exe *`) or quoted (`*gh.exe" *`, `*/gh' *`, and so on). The older narrow `gh auth ...`,
//! `gh pr merge` and `gh api *merge*` rules stay: they are redundant while `gh *` is there and
//! document what matters most if it is ever loosened. `lil dev gh` isn't hit, because its text has
//! `gh` as an argument after `lil dev`, never as the command or after a `/` or `\`.
//!
//! Known limits of the direct-`gh` rules, stated plainly:
//!
//! - scripts and subprocesses that call `gh`: `bash -c 'gh ...'`, `sh -c`, `pwsh -c`,
//!   `cmd /c gh ...`, `env gh ...`, Python (`subprocess`), a Makefile or cargo alias, any program
//!   that spawns it;
//! - other spellings: `& gh ...` or `Start-Process gh` in PowerShell, a quoted command name
//!   (`"gh" pr list`), a copy or link of `gh.exe` under another name, a bare full path with no
//!   arguments (it only prints help);
//! - reading gh's stored login directly (its `hosts.yml` or the Windows Credential Manager entry)
//!   or calling the GitHub API with it (`curl`, a script). The `.lilith` and `.worldwright` rules
//!   protect only the suite's own folders, not gh's;
//! - a false positive: a path argument that has `/gh ` in it (`git add dev/gh x`) is denied too.
//!
//! So the owner's login remains reachable to a determined process running as the same Windows
//! user. These rules stop an agent from doing it by habit or by a plain command; they don't stop
//! one that goes around them. The full fix is a separate Windows account for agent sessions, with
//! no GitHub login in that account (see `docs/dev/merge-guard.md`, section 2.7). GitHub's ruleset
//! on `main` (no bypass) still blocks a push to `main` or a merge without checks, for anyone.
//!
//! **The credential helper.** `* dev git-credential*` denies running `lil dev git-credential`
//! (or `lil dev git-credential`) by hand, which would print the agent token. The committed
//! script `tools/git-hooks/credential-helper` runs that command, so `*credential-helper*`
//! (RN6) denies any command line that names it: `sh`, `bash`, `./`, a full path to `sh.exe`,
//! a path built with `$(git rev-parse --show-toplevel)`, in a pipe or after `cd`. And
//! `git * credential fill*` adds the spelling with options before `credential`
//! (`git -C . credential fill`) to `git credential fill*`. Pushes still work: git runs the
//! helper itself (from `credential.helper`, as a child of `git`), never through Claude Code's
//! Bash or PowerShell tool, so these rules never see that call.
//!
//! Limits of the credential-helper rules:
//!
//! - they match the helper's name anywhere in the command, so they also deny staging it by its
//!   exact path (`git add` naming the file). Stage it by its folder instead:
//!   `git add tools/git-hooks/`;
//! - they don't cover a glob spelling of the name (`sh tools/git-hooks/cred*`), `sh -c '...'`
//!   (or `bash -c`, `pwsh -c`) with the command inside the script, or git's own tracing
//!   (`GIT_TRACE_CURL`) on a push.
//!
//! The git hooks (QA-034), `lil dev merge`'s own checks and the post-merge CI check (QA-035)
//! back these patterns up, and the agent token (QA-079) limits what a bypass can reach.

use std::path::PathBuf;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shell {
    Bash,
    PowerShell,
}

/// The committed `.claude/settings.json` at the workspace root.
fn settings_text() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(".claude")
        .join("settings.json");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("can't read {}: {e}", path.display()))
}

/// `permissions.deny`, split into the Bash and the PowerShell rule bodies.
fn deny_rules() -> (Vec<String>, Vec<String>) {
    let v: Value = serde_json::from_str(&settings_text()).expect(".claude/settings.json is JSON");
    let deny = v["permissions"]["deny"]
        .as_array()
        .expect("permissions.deny is an array");
    let mut bash = Vec::new();
    let mut ps = Vec::new();
    for rule in deny {
        let rule = rule.as_str().expect("every deny rule is a string");
        if let Some(body) = rule.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')) {
            bash.push(body.to_string());
        } else if let Some(body) = rule
            .strip_prefix("PowerShell(")
            .and_then(|r| r.strip_suffix(')'))
        {
            ps.push(body.to_string());
        } else if rule.starts_with("Read(") || rule.starts_with("Edit(") {
            // Path rules: see path_rules().
        } else {
            panic!(
                "unexpected deny rule {rule:?}: only Bash(...), PowerShell(...), Read(...) and Edit(...) are used (Claude Code never consults Write(path) rules)"
            );
        }
    }
    (bash, ps)
}

// ---------------------------------------------------------------------------------------------
// The matcher.
// ---------------------------------------------------------------------------------------------

/// `*` matches any sequence of characters (including none). Everything else is literal.
fn glob(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// Whether one rule body (the text inside `Bash(...)`) matches one subcommand.
fn rule_matches(rule: &str, command: &str, shell: Shell) -> bool {
    let mut rule = match rule.strip_suffix(":*") {
        Some(head) => format!("{head} *"),
        None => rule.to_string(),
    };
    let mut command = command.to_string();
    if shell == Shell::PowerShell {
        rule = rule.to_lowercase();
        command = command.to_lowercase();
    }
    let p: Vec<char> = rule.chars().collect();
    let c: Vec<char> = command.chars().collect();
    if glob(&p, &c) {
        return true;
    }
    // A trailing ` *` that is the only wildcard also matches the bare command.
    rule.matches('*').count() == 1
        && rule
            .strip_suffix(" *")
            .is_some_and(|bare| bare == command.as_str())
}

/// Splits a command line into subcommands, outside quotes. Bash also splits at `&`, `|&`,
/// newlines and the edges of subshells and command substitutions (`$(...)` and backticks
/// open a substitution even inside double quotes); a `&` in a redirection (`2>&1`, `<&3`)
/// isn't a separator.
fn subcommands(line: &str, shell: Shell) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    // The quote state outside each open `$(`, restored at its `)`.
    let mut outer: Vec<Option<char>> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let substitution = shell == Shell::Bash && quote != Some('\'');
        if substitution && c == '$' && next == Some('(') {
            outer.push(quote);
            quote = None;
            parts.push(std::mem::take(&mut cur));
            i += 2;
            continue;
        }
        if substitution && c == ')' && quote.is_none() && !outer.is_empty() {
            quote = outer.pop().flatten();
            parts.push(std::mem::take(&mut cur));
            i += 1;
            continue;
        }
        if substitution && c == '`' {
            parts.push(std::mem::take(&mut cur));
            i += 1;
            continue;
        }
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        let sep_len = match (shell, c, next) {
            (_, '\'' | '"', _) => {
                quote = Some(c);
                cur.push(c);
                i += 1;
                continue;
            }
            (_, '&', Some('&')) | (_, '|', Some('|')) => 2,
            (_, '|', _) | (_, ';', _) | (_, '\n', _) => 1,
            (Shell::Bash, '(' | ')', _) => 1,
            (Shell::Bash, '&', _) if !cur.ends_with(['>', '<']) => 1,
            _ => 0,
        };
        if sep_len == 0 {
            cur.push(c);
            i += 1;
        } else {
            parts.push(std::mem::take(&mut cur));
            i += sep_len;
            // `|&` is one separator.
            if c == '|' && sep_len == 1 && chars.get(i) == Some(&'&') {
                i += 1;
            }
        }
    }
    parts.push(cur);
    parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .map(|p| match shell {
            Shell::Bash => strip_bash_wrappers(&p),
            Shell::PowerShell => p,
        })
        .collect()
}

/// Strips leading `NAME=value` assignments and the documented wrappers, repeatedly.
fn strip_bash_wrappers(command: &str) -> String {
    let mut words: Vec<&str> = command.split_whitespace().collect();
    while let Some(&first) = words.first() {
        let is_assignment = first.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        let skip = if is_assignment {
            1
        } else {
            match first {
                "time" | "nohup" | "builtin" | "noglob" => 1,
                "command" if !matches!(words.get(1), Some(&"-v" | &"-V")) => 1,
                "xargs" if words.get(1).is_some_and(|w| !w.starts_with('-')) => 1,
                "nice" | "stdbuf" | "timeout" => {
                    let mut n = 1;
                    while let Some(w) = words.get(n) {
                        if !w.starts_with('-') {
                            break;
                        }
                        // Options that take a separate argument.
                        n += if matches!(*w, "-n" | "-s" | "-k" | "-i" | "-o" | "-e") {
                            2
                        } else {
                            1
                        };
                    }
                    if first == "timeout" {
                        n += 1; // the duration
                    }
                    n
                }
                _ => 0,
            }
        };
        if skip == 0 || skip >= words.len() {
            break;
        }
        words.drain(..skip);
    }
    words.join(" ")
}

/// The first rule that denies `line`, if any.
fn denying_rule<'r>(line: &str, rules: &'r [String], shell: Shell) -> Option<&'r str> {
    subcommands(line, shell).iter().find_map(|sub| {
        rules
            .iter()
            .find(|r| rule_matches(r, sub, shell))
            .map(String::as_str)
    })
}

// ---------------------------------------------------------------------------------------------
// Read and Edit path rules.
// ---------------------------------------------------------------------------------------------

/// `permissions.deny`'s `Read(...)` and `Edit(...)` rule bodies.
fn path_rules() -> (Vec<String>, Vec<String>) {
    let v: Value = serde_json::from_str(&settings_text()).expect(".claude/settings.json is JSON");
    let mut read = Vec::new();
    let mut edit = Vec::new();
    for rule in v["permissions"]["deny"].as_array().unwrap() {
        let rule = rule.as_str().unwrap();
        if let Some(body) = rule.strip_prefix("Read(").and_then(|r| r.strip_suffix(')')) {
            read.push(body.to_string());
        } else if let Some(body) = rule.strip_prefix("Edit(").and_then(|r| r.strip_suffix(')')) {
            edit.push(body.to_string());
        }
    }
    (read, edit)
}

/// A path in the POSIX form Claude Code matches on Windows: `C:\Users\<name>` is `/c/Users/<name>`.
fn posix(path: &str) -> String {
    let p = path.replace('\\', "/");
    let mut chars = p.chars();
    match (chars.next(), chars.next()) {
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic() => {
            format!("/{}{}", drive.to_ascii_lowercase(), &p[2..])
        }
        _ => p,
    }
}

/// gitignore-style segment matching: `**` is any number of segments (none included), `*`
/// anything within one segment.
fn segments_match(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| segments_match(rest, &path[skip..])),
        Some((first, rest)) => path.split_first().is_some_and(|(seg, path_rest)| {
            let p: Vec<char> = first.chars().collect();
            let s: Vec<char> = seg.chars().collect();
            glob(&p, &s) && segments_match(rest, path_rest)
        }),
    }
}

/// Whether a `Read`/`Edit` rule body matches an absolute `path`, for a user whose home
/// directory is `home`. Only the two anchored shapes the settings use are supported: `~/...`
/// (home) and `//...` (filesystem root).
fn path_rule_matches(rule: &str, path: &str, home: &str) -> bool {
    let anchored = if let Some(rest) = rule.strip_prefix("~/") {
        format!("{}/{rest}", posix(home).trim_end_matches('/'))
    } else if let Some(rest) = rule.strip_prefix("//") {
        format!("/{rest}")
    } else {
        panic!("path rule {rule:?}: only ~/ and // anchors are used");
    };
    let pattern: Vec<&str> = anchored.split('/').filter(|s| !s.is_empty()).collect();
    let target = posix(path);
    let path: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
    segments_match(&pattern, &path)
}

/// A home directory with the test placeholder user name (ruling R30).
const HOME: &str = r"C:\Users\Fake";

/// Paths that every Read and every Edit deny list must cover.
const PRIVATE_PATHS: &[&str] = &[
    r"C:\Users\Fake\.lilith\privacy-denylist.txt",
    r"C:\Users\Fake\.lilith\reviewer\app-key.age",
    r"C:\Users\Fake\.lilith\new-file.txt",
    "C:/Users/Fake/.lilith/privacy-denylist.txt",
    "/c/Users/Fake/.lilith/reviewer/app-key.age",
    r"D:\profiles\Fake\.lilith\privacy-denylist.txt",
    // The legacy home folder (R4): its rules stay until the owner says it is gone.
    r"C:\Users\Fake\.worldwright\privacy-denylist.txt",
    r"C:\Users\Fake\.worldwright\reviewer\app-key.age",
    r"C:\Users\Fake\.worldwright\new-file.txt",
    "C:/Users/Fake/.worldwright/privacy-denylist.txt",
    "/c/Users/Fake/.worldwright/reviewer/app-key.age",
    // A profile elsewhere, on another drive.
    r"D:\profiles\Fake\.worldwright\privacy-denylist.txt",
];

/// Paths no Read or Edit deny rule may cover.
const ORDINARY_PATHS: &[&str] = &[
    r"C:\Users\Fake\Desktop\Worldwright-Suite\README.md",
    r"C:\Users\Fake\Desktop\Worldwright-Suite\docs\dev\merge-guard.md",
    r"C:\Users\Fake\notes\worldwright.txt",
    r"C:\Users\Fake\.worldwright-old\notes.txt",
    r"C:\Users\Fake\Desktop\LILITH\README.md",
    r"C:\Users\Fake\Desktop\LILITH\lilith.yaml",
    r"C:\Users\Fake\notes\lilith.txt",
    r"C:\Users\Fake\.lilith-old\notes.txt",
    r"D:\.lilith-local\0123abcd\state.db",
    r"C:\Users\Fake\.claude\settings.json",
];

/// Shell reads of the folder, denied in both shells.
const MUST_MATCH_PRIVATE_READS: &[&str] = &[
    "cat ~/.lilith/privacy-denylist.txt",
    "cat -n $HOME/.lilith/reviewer/app-key.age",
    "type %USERPROFILE%\\.lilith\\privacy-denylist.txt",
    "Get-Content $env:USERPROFILE\\.lilith\\privacy-denylist.txt",
    "gc ~/.lilith/privacy-denylist.txt",
    "git status; cat ~/.lilith/privacy-denylist.txt",
    // The legacy home folder (R4).
    "cat ~/.worldwright/privacy-denylist.txt",
    "cat -n $HOME/.worldwright/reviewer/app-key.age",
    "type %USERPROFILE%\\.worldwright\\privacy-denylist.txt",
    "Get-Content $env:USERPROFILE\\.worldwright\\privacy-denylist.txt",
    "gc ~/.worldwright/privacy-denylist.txt",
    "git status; cat ~/.worldwright/privacy-denylist.txt",
];

/// Ordinary reads, allowed in both shells.
const MUST_NOT_MATCH_READS: &[&str] = &[
    "cat README.md",
    "cat docs/worldwright.md",
    "cat docs/lilith.md",
    "cat lilith.yaml",
    "Get-Content lilith.yaml",
    "type docs\\lilith.md",
    "gc notes/lilith.txt",
    "type cargo",
    "Get-Content docs/dev/merge-guard.md",
    "gc Cargo.toml",
];

// ---------------------------------------------------------------------------------------------
// The tables.
// ---------------------------------------------------------------------------------------------

/// Denied in both shells.
const MUST_MATCH: &[&str] = &[
    // The brief's rows.
    "git push origin main",
    "git push -u origin main",
    "git merge x --no-verify",
    "gh pr merge 3",
    "gh api -X PUT repos/x/y/pulls/3/merge",
    // Direct gh (QA-087): any arguments, bare, `.exe`, a path, and in compound commands.
    "gh pr list",
    "gh api -X PUT repos/o/r/rulesets/1",
    "gh",
    "gh.exe repo view",
    "gh.exe",
    "cd x && gh pr merge 3",
    "git status; gh repo view",
    "echo y | gh pr list",
    "C:\\Program Files\\GitHub CLI\\gh.exe pr list",
    "\"C:\\Program Files\\GitHub CLI\\gh.exe\" pr list",
    "'C:\\Program Files\\GitHub CLI\\gh.exe' pr list",
    "/c/Program Files/GitHub CLI/gh.exe pr list",
    "/c/Program Files/GitHub CLI/gh pr list",
    "/usr/bin/gh pr list",
    "\"/usr/bin/gh\" pr list",
    // Reading GitHub (used to be allowed; now only `lil dev gh` reads).
    "gh pr view 3",
    "gh pr create --fill",
    "gh pr checks 3",
    "gh api repos/x/y/pulls/3",
    "gh auth status",
    "gh auth status -h github.com",
    // Pushes to main written other ways.
    "git push origin main --force",
    "git push --set-upstream origin main",
    "git push origin HEAD:main",
    "git push origin feature/x:main",
    "git push origin HEAD:refs/heads/main",
    "git push origin +main",
    "git push origin HEAD:main --force-with-lease",
    // Force pushes anywhere.
    "git push --force origin feature/x",
    "git push --force-with-lease",
    "git push -f origin feature/x",
    "git push origin feature/x -f",
    "git push origin feature/x --force",
    // Skipping hooks.
    "git commit -n -m x",
    "git commit --no-verify -m x",
    "git push --no-verify origin feature/x",
    "git merge --no-verify main",
    "git config core.hooksPath .git/hooks",
    "git config --unset core.hooksPath",
    "git config --local core.hookspath x",
    "git -c core.hooksPath=/dev/null push origin feature/x",
    // Merging through GitHub.
    "gh pr merge",
    "gh pr merge 3 --squash --admin",
    "gh pr merge --auto 3",
    "gh api repos/x/y/pulls/3/merge",
    "gh api graphql -f query='mutation { mergePullRequest(input: {}) { clientMutationId } }'",
    "gh api graphql -f query='mutation { enablePullRequestAutoMerge(input: {}) { clientMutationId } }'",
    // Reading or replacing a GitHub login.
    "gh auth token",
    "gh auth token --hostname github.com",
    "gh auth status --show-token",
    "gh auth status -h github.com --show-token",
    "gh auth status -t",
    "gh auth login",
    "gh auth login --with-token",
    "gh auth git-credential get",
    "git credential fill",
    "git credential-manager get",
    "git credential-manager erase",
    "cmdkey /list",
    "cmdkey.exe /list",
    "cmdkey /delete:git:https://github.com",
    "git config credential.helper ''",
    "git config --global credential.helper manager",
    "git config --local --get-all credential.helper",
    "git config --unset-all credential.helper",
    "git config --local credential.https://github.com.username x",
    "git -c credential.helper= push origin feature/x",
    // Fix round 1: running the agent's credential helper by hand prints the token.
    "cargo run -q --locked -p lil-cli --bin lil -- dev git-credential get",
    "target/debug/lil.exe dev git-credential get",
    "lil dev git-credential get",
    "echo host=github.com | lil dev git-credential get",
    "git-credential-manager get",
    "git-credential-manager.exe erase",
    "gh auth setup-git",
    "gh alias set m 'pr merge'",
    "gh alias list",
    // Fix round 1: more hook-skipping and force spellings, and options before `push`.
    "git commit -m x -n",
    "git commit -a -n -m x",
    "git push --no-veri origin feature/x",
    "git merge --no-verif feature/x",
    "git push -uf origin feature/x",
    "git push origin feature/x -uf",
    "git push -vf origin feature/x",
    "git push -qf",
    "git push -nf origin feature/x",
    "git push -df origin feature/x",
    "git -C . push origin main",
    "git -C . push -u origin main",
    "git -C . push origin HEAD:main",
    "git -c push.default=current push origin feature/x:main",
    // Compound commands.
    "git status && git push origin main",
    "git status; gh pr merge 3",
    "echo y | gh pr merge 3",
    "git fetch || gh auth token",
    // RN6: the committed credential helper prints the agent token too, however it is started.
    "sh tools/git-hooks/credential-helper get",
    "bash tools/git-hooks/credential-helper get",
    "./tools/git-hooks/credential-helper get",
    "sh ./tools/git-hooks/credential-helper",
    "sh \"$(git rev-parse --show-toplevel)/tools/git-hooks/credential-helper\" get",
    "echo host=github.com | sh tools/git-hooks/credential-helper get",
    "cd x && sh tools/git-hooks/credential-helper get",
    "git -C . credential fill",
    "cargo run -q --locked -p lil-cli --bin lil -- dev git-credential get",
    "target/debug/lil.exe dev git-credential get",
];

/// Denied in Bash only: wrappers, assignments and substitutions are Bash syntax.
const MUST_MATCH_BASH: &[&str] = &[
    "cd repo && git push origin main",
    "timeout 30 git push origin main",
    "nohup gh pr merge 3",
    "FOO=1 git push origin main",
    "FOO=1 gh pr list",
    "timeout 30 gh pr list",
    "& \"C:\\Program Files\\GitHub CLI\\gh.exe\" pr list",
    "GIT_TRACE=1 git commit -n -m x",
    "time git push --force",
    "command git push origin main",
    "printf 'host=github.com\\n' | git credential fill",
    "echo \"$(gh auth token)\"",
    "(git push origin main)",
    "git status & git push origin main",
    "git push origin main 2>&1",
    // RN6.
    "FOO=1 sh tools/git-hooks/credential-helper get",
    "timeout 30 sh tools/git-hooks/credential-helper get",
];

/// Denied in PowerShell only: matching ignores case there.
const MUST_MATCH_POWERSHELL: &[&str] = &[
    "Git Push Origin Main",
    "GH PR MERGE 3",
    "git status; Git Push --Force",
    "CMDKEY /list",
    "GH.EXE repo view",
    "Gh Pr List",
    "& 'C:\\Program Files\\GitHub CLI\\gh.exe' pr list",
    "& \"C:\\Program Files\\GitHub CLI\\GH.EXE\" pr list",
    "Set-Location repo; git push origin main",
    // RN6.
    "& 'C:\\Program Files\\Git\\bin\\sh.exe' tools/git-hooks/credential-helper get",
    "SH .\\TOOLS\\GIT-HOOKS\\CREDENTIAL-HELPER GET",
];

/// Allowed in both shells.
const MUST_NOT_MATCH: &[&str] = &[
    // The brief's rows.
    "git push origin plan-1-foundation",
    "cargo run -p lil-cli -- dev merge 3",
    // Ordinary branch work.
    "git push -u origin plan-1-foundation",
    "git push origin feature/maintenance",
    "git push origin mainline",
    "git push origin feature/main-menu",
    "git push --follow-tags origin feature/x",
    "git push -u origin fix/pdf",
    // Why force bundles are listed one by one, not as `git push * -*f*`: that form would
    // deny any later flag or push option with an `f` in it.
    "git push origin feature/x --follow-tags",
    "git push -o ci.skip=false origin feature/x",
    "git push -u origin feat",
    "git -C . push origin feature/maintenance",
    "git -C . push -u origin plan-1-foundation",
    "git commit -m 'note -n'",
    "git commit -m x --no-edit",
    "git fetch origin main",
    "git log main",
    "git merge feature/x",
    "git commit -m x",
    "git status",
    "git config user.name x",
    "git config --get core.autocrlf",
    // The suite's own tooling. `lil dev gh` is the agents' way to use gh (QA-087).
    "cargo run -p lil-cli --bin lil -- dev agent-token status",
    "cargo run -p lil-cli --bin lil -- dev gh -- pr view 3",
    "cargo run -q -p lil-cli -- dev gh -- pr list",
    ".\\target\\debug\\lil.exe dev gh -- pr view 2",
    "target/debug/lil.exe dev gh -- pr view 2",
    "lil dev gh -- issue list",
    "cd x && lil dev gh -- pr list",
    "cargo run -p lil-cli --bin lil -- dev install-hooks",
    // Not gh at all, or gh only as text or as part of another name.
    "git push origin feature/x",
    "git push origin fix/deny-direct-gh",
    "git push origin feature/ghost",
    "git log --oneline gh-pages",
    "echo gh",
    "echo gh pr list",
    "echo gh.exe",
    "ls gh-notes",
    "cargo test ugh.exe-name",
    // A denied command only as text.
    "echo gh auth token",
    "git commit -m 'gh pr merge is denied'",
    // RN6: the helper's name only in other words, or its folder (stage the helper this way).
    "cargo nextest run -p lil-cli -E 'binary(credential_helper)'",
    "git add tools/git-hooks/",
    "git commit -m 'credential helper is a committed script'",
    "sh tools/git-hooks/pre-push",
];

#[test] // spec: QA-036
fn denied_commands_match_a_rule() {
    let (bash, ps) = deny_rules();
    let mut missed = Vec::new();
    for (shell, rules, rows) in [
        (Shell::Bash, &bash, MUST_MATCH),
        (Shell::PowerShell, &ps, MUST_MATCH),
        (Shell::Bash, &bash, MUST_MATCH_BASH),
        (Shell::PowerShell, &ps, MUST_MATCH_POWERSHELL),
    ] {
        for row in rows {
            if denying_rule(row, rules, shell).is_none() {
                missed.push(format!("{shell:?}: {row}"));
            }
        }
    }
    assert!(missed.is_empty(), "not denied:\n{}", missed.join("\n"));
}

#[test] // spec: QA-036
fn allowed_commands_match_no_rule() {
    let (bash, ps) = deny_rules();
    let mut hit = Vec::new();
    for (shell, rules) in [(Shell::Bash, &bash), (Shell::PowerShell, &ps)] {
        for row in MUST_NOT_MATCH {
            if let Some(rule) = denying_rule(row, rules, shell) {
                hit.push(format!("{shell:?}: {row}  (denied by {rule})"));
            }
        }
    }
    assert!(hit.is_empty(), "wrongly denied:\n{}", hit.join("\n"));
}

/// The brief's rules are present verbatim, and every Bash rule has a PowerShell twin.
#[test] // spec: QA-036
fn brief_rules_are_present_in_both_shells() {
    let (bash, ps) = deny_rules();
    for rule in [
        "git push origin main:*",
        "git push * main:*",
        "git push --force*",
        "git * --no-verify*",
        "git commit -n*",
        "gh pr merge:*",
        "gh *",
        "gh.exe *",
        "*\\gh.exe *",
        "*/gh *",
        "gh api *merge*",
        "git config core.hooksPath*",
        "gh auth token*",
        "gh auth status*--show-token*",
        "gh auth login*",
        "git credential fill*",
        "git * credential fill*",
        "*credential-helper*",
        "git credential-manager*",
        "cmdkey*",
        "git config*credential*",
        // Task B5: the new home folder, as the legacy one.
        "cat *.lilith*",
        "type *.lilith*",
        "Get-Content *.lilith*",
        "gc *.lilith*",
    ] {
        assert!(bash.iter().any(|r| r == rule), "missing Bash({rule})");
    }
    let mut b = bash.clone();
    let mut p = ps.clone();
    b.sort();
    p.sort();
    assert_eq!(b, p, "the Bash and PowerShell rule sets differ");
}

/// The settings file is public: no user-profile paths, no e-mail addresses.
#[test] // spec: QA-036
fn settings_hold_nothing_personal() {
    let text = settings_text();
    let lower = text.to_ascii_lowercase();
    for bad in [":\\\\users", ":/users/", "/c/users/", "appdata", "@"] {
        assert!(
            !lower.contains(bad),
            ".claude/settings.json contains {bad:?}"
        );
    }
}

// ---- the matcher itself, against the documentation's own examples ---------------------------

#[test]
fn matcher_follows_the_documented_examples() {
    let b = |rule: &str, cmd: &str| denying_rule(cmd, &[rule.to_string()], Shell::Bash).is_some();
    // The docs' wildcard table.
    assert!(b("npm run build", "npm run build"));
    assert!(!b("npm run build", "npm run build --watch"));
    assert!(b("npm run *", "npm run test --watch"));
    assert!(b("npm run *", "npm run"));
    assert!(!b("npm run *", "npm install"));
    assert!(b("git log * main", "git log --oneline main"));
    assert!(!b("git log * main", "git log main"));
    assert!(!b("git log * main", "git push origin main"));
    assert!(b("git * main", "git merge main"));
    assert!(b("git * main", "git -c core.fsmonitor=x diff main"));
    assert!(!b("git * main", "git log"));
    assert!(b("* --version", "node --version"));
    assert!(!b("* --version", "node -v"));
    assert!(b("ls *", "ls -la"));
    assert!(b("ls *", "ls"));
    assert!(!b("ls *", "lsof"));
    assert!(b("ls*", "lsof"));
    assert!(b("* --help *", "npm --help x"));
    assert!(!b("* --help *", "npm --help"));
    // `:*` only at the end.
    assert!(b("ls:*", "ls"));
    assert!(b("ls:*", "ls -la"));
    assert!(!b("git:* push", "git push"));
    // Compound commands, wrappers and assignments.
    assert!(b("git clean *", "cd /tmp && git clean -f"));
    assert!(b("git clean *", "echo \"$(git clean -f)\""));
    assert!(b("npm test *", "timeout 30 npm test"));
    assert!(b("rm *", "FOO=bar rm -rf tmp/"));
    assert!(b("grep *", "xargs grep pattern"));
    assert!(!b("grep *", "xargs -n1 grep pattern"));
    assert!(!b("rm *", "command -v rm"));
    // The documented limits: these forms are not stopped.
    assert!(!b("git push *", "git -C . push origin main"));
    assert!(!b("git push *", "git 'push' origin main"));
    assert!(!b("rm *", "/bin/rm -rf build/"));
    // The direct-gh shapes: a trailing ` *` also matches the bare command; a leading `*` matches
    // a path prefix but then the bare path needs its own rule.
    assert!(b("gh *", "gh"));
    assert!(b("gh *", "gh pr list"));
    assert!(!b("gh *", "ghost"));
    assert!(!b("gh *", "echo gh"));
    assert!(b("*/gh *", "/usr/bin/gh pr list"));
    assert!(!b("*/gh *", "/usr/bin/gh"));
    assert!(!b("*/gh *", "lil dev gh -- pr list"));
    // PowerShell ignores case.
    let p =
        |rule: &str, cmd: &str| denying_rule(cmd, &[rule.to_string()], Shell::PowerShell).is_some();
    assert!(p("Remove-Item *", "remove-item x"));
    assert!(p("git push *", "Get-Date; git push"));
    assert!(!b("git push *", "Git Push"));
}

// ---- the owner's key and denylist folder (R38) ----------------------------------------------

#[test] // spec: QA-036
fn private_folder_is_denied_to_read_and_edit() {
    let (read, edit) = path_rules();
    for rule in ["~/.lilith/**", "//**/.lilith/**"] {
        assert!(read.iter().any(|r| r == rule), "missing Read({rule})");
        assert!(edit.iter().any(|r| r == rule), "missing Edit({rule})");
    }
    let mut missed = Vec::new();
    for (tool, rules) in [("Read", &read), ("Edit", &edit)] {
        for path in PRIVATE_PATHS {
            if !rules.iter().any(|r| path_rule_matches(r, path, HOME)) {
                missed.push(format!("{tool}: {path}"));
            }
        }
        for path in ORDINARY_PATHS {
            if let Some(r) = rules.iter().find(|r| path_rule_matches(r, path, HOME)) {
                missed.push(format!("{tool}: {path} wrongly denied by {r}"));
            }
        }
    }
    assert!(missed.is_empty(), "{}", missed.join("\n"));
}

#[test] // spec: QA-036
fn shell_reads_of_the_private_folder_are_denied() {
    let (bash, ps) = deny_rules();
    let mut wrong = Vec::new();
    for (shell, rules) in [(Shell::Bash, &bash), (Shell::PowerShell, &ps)] {
        for row in MUST_MATCH_PRIVATE_READS {
            if denying_rule(row, rules, shell).is_none() {
                wrong.push(format!("{shell:?}: not denied: {row}"));
            }
        }
        for row in MUST_NOT_MATCH_READS {
            if let Some(rule) = denying_rule(row, rules, shell) {
                wrong.push(format!("{shell:?}: wrongly denied: {row} (by {rule})"));
            }
        }
    }
    // PowerShell ignores case.
    for row in [
        "GET-CONTENT ~/.LILITH/privacy-denylist.txt",
        "GET-CONTENT ~/.WORLDWRIGHT/privacy-denylist.txt",
    ] {
        if denying_rule(row, &ps, Shell::PowerShell).is_none() {
            wrong.push(format!(
                "PowerShell: upper-case Get-Content not denied: {row}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The legacy home folder's rules stay, verbatim, until the owner says the folder is gone
/// (Task B5 adds the new folder's rules beside them; PR-D at the earliest removes these).
#[test] // spec: QA-036
fn worldwright_rules_are_kept() {
    let (bash, ps) = deny_rules();
    let (read, edit) = path_rules();
    for rule in [
        "cat *.worldwright*",
        "type *.worldwright*",
        "Get-Content *.worldwright*",
        "gc *.worldwright*",
    ] {
        assert!(bash.iter().any(|r| r == rule), "missing Bash({rule})");
        assert!(ps.iter().any(|r| r == rule), "missing PowerShell({rule})");
    }
    for rule in ["~/.worldwright/**", "//**/.worldwright/**"] {
        assert!(read.iter().any(|r| r == rule), "missing Read({rule})");
        assert!(edit.iter().any(|r| r == rule), "missing Edit({rule})");
    }
}

/// Every deny rule that names the legacy home folder has a twin, of the same kind, that names
/// the new one: the new folder is kept from agents exactly as the old one is (Task B5).
#[test] // spec: QA-036
fn every_legacy_home_rule_has_a_lilith_twin() {
    let v: Value = serde_json::from_str(&settings_text()).expect(".claude/settings.json is JSON");
    let deny: Vec<&str> = v["permissions"]["deny"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    let legacy: Vec<&str> = deny
        .iter()
        .copied()
        .filter(|r| r.contains(".worldwright"))
        .collect();
    assert_eq!(legacy.len(), 12, "{legacy:?}");
    for rule in legacy {
        let twin = rule.replace(".worldwright", ".lilith");
        assert!(deny.contains(&twin.as_str()), "{rule} has no twin {twin}");
    }
}

#[test]
fn path_matcher_follows_the_documented_examples() {
    let m = |rule: &str, path: &str| path_rule_matches(rule, path, "/home/alice");
    // The docs' anchors and Windows normalisation.
    assert!(m("~/.zshrc", "/home/alice/.zshrc"));
    assert!(!m("~/.zshrc", "/home/bob/.zshrc"));
    assert!(m("//tmp/scratch.txt", "/tmp/scratch.txt"));
    assert!(m("//c/**/.env", r"C:\Users\Fake\project\.env"));
    assert!(m("//**/.env", r"D:\x\.env"));
    assert!(!m("//c/**/.env", r"D:\x\.env"));
    // `*` stays within a segment; `**` crosses them.
    assert!(m("~/Documents/*.pdf", "/home/alice/Documents/a.pdf"));
    assert!(!m("~/Documents/*.pdf", "/home/alice/Documents/sub/a.pdf"));
    assert!(m("~/Documents/**", "/home/alice/Documents/sub/a.pdf"));
}
