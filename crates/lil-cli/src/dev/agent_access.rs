//! Granular agent access to GitHub (QA-036, QA-079; ruling R28).
//!
//! Agents never use the owner's full GitHub login. They use one fine-grained token, limited to
//! the suite repository, that the owner stores once in Windows Credential Manager at target
//! [`AGENT_TOKEN_TARGET`]. A token stored before the rename, at [`LEGACY_AGENT_TOKEN_TARGET`],
//! is still read for one release (R4): the new target answers first, and the legacy one only
//! when the new one holds no token ([`stored_token`]).
//!
//! - `lil dev agent-token set|status|clear [--legacy]` manages it. `set` and `clear` only run at
//!   a real terminal, and `set` reads the token without echo. `set` writes only the new target;
//!   `clear` removes both, and `clear --legacy` only the legacy one. Nothing here ever writes the
//!   legacy target, and the credential store refuses that write too. `status` names the target
//!   that answered and shows the token's first four characters and the expiry the owner
//!   entered, never the token.
//! - `lil dev git-credential <get|store|erase>` is git's credential helper for this repository
//!   (`lil dev install-hooks` writes [`GIT_CREDENTIAL_HELPER`] after an empty entry that resets
//!   inherited helpers such as Git Credential Manager). `get` answers only for
//!   `https://github.com`; `store` and `erase` do nothing.
//! - `lil dev gh -- <args>` runs an allow-listed `gh` command ([`gh_refusal`]) with `GH_TOKEN`
//!   set in the child's environment only.
//! - Every `lil dev` GitHub call, and the helper's `get`, resolves its token with
//!   [`resolve_token`]: `GITHUB_TOKEN` (CI), otherwise the stored agent token under either
//!   name, otherwise a refusal that explains `lil dev agent-token set`. There is no fallback to
//!   `gh auth token`.
//!
//! The token is never printed, logged or written to a file by this module. The one place it
//! leaves the process is the helper's answer to git on stdout, which is the helper protocol.
//! It is kept in [`Zeroizing`] strings where that is cheap. An agent that reads the token
//! through the helper gains nothing beyond what the token itself allows; that is the design.

use std::io::Write;
use std::process::Command;

use anyhow::{Result, anyhow, bail};
use lil_platform::credentials::{Credential, CredentialStore, MAX_SECRET_BYTES};
use zeroize::Zeroizing;

use super::reviewer::Prompt;

/// Where the agent token lives in Windows Credential Manager. `set` writes only this target.
pub const AGENT_TOKEN_TARGET: &str = "LILITH-dev/github/agent-token";
/// Where the agent token lived before the rename (R4). It is read when [`AGENT_TOKEN_TARGET`]
/// holds no token, and deleted by `clear`; it is never written.
pub const LEGACY_AGENT_TOKEN_TARGET: &str = "Worldwright-dev/github/agent-token";
/// The user name GitHub accepts with a token over HTTPS.
pub const AGENT_TOKEN_USERNAME: &str = "x-access-token";
/// The repository-local `credential.helper` entry `lil dev install-hooks` writes after the
/// empty reset entry. Git appends the action (`get`, `store`, `erase`).
///
/// It runs the committed script `tools/git-hooks/credential-helper` of the worktree git runs
/// in, and the script runs `dev git-credential` from whichever CLI crate that checkout has
/// (`lil-cli`, or the CLI crate of a checkout from before the rename). Every worktree of a clone shares this entry, so it survives the
/// crate rename. A worktree on a commit from before the script has no helper until it is
/// rebased.
pub const GIT_CREDENTIAL_HELPER: &str =
    r#"!sh "$(git rev-parse --show-toplevel)/tools/git-hooks/credential-helper""#;
/// Fine-grained personal access tokens start with this.
const FINE_GRAINED_PREFIX: &str = "github_pat_";
/// The stored credential's comment: `expires=<YYYY-MM-DD or none>`.
const EXPIRY_KEY: &str = "expires=";
/// Where the token is read from, in order (R4).
const READ_ORDER: [&str; 2] = [AGENT_TOKEN_TARGET, LEGACY_AGENT_TOKEN_TARGET];

pub const SET_NOT_INTERACTIVE: &str = "lil dev agent-token set only runs at a real terminal (stdin and stderr must both be one); nothing was stored";
pub const CLEAR_NOT_INTERACTIVE: &str = "lil dev agent-token clear only runs at a real terminal (stdin and stderr must both be one); nothing was removed";
const NO_TOKEN: &str = "No GitHub token: GITHUB_TOKEN isn't set and no agent token is stored. At a terminal, the owner runs `lil dev agent-token set` to store a fine-grained token limited to this repository.";

// ---------------------------------------------------------------------------------------------
// Token resolution.
// ---------------------------------------------------------------------------------------------

/// The token for a `lil dev` GitHub call: `github_token` (the `GITHUB_TOKEN` variable) when it
/// is set and not blank, otherwise the stored agent token ([`stored_token`]: the new target,
/// then the legacy one), otherwise an error explaining `lil dev agent-token set`. The store is
/// not read when `github_token` is usable.
pub fn resolve_token(
    github_token: Option<Zeroizing<String>>,
    store: &dyn CredentialStore,
) -> Result<Zeroizing<String>> {
    if let Some(t) = github_token {
        let t = t.trim();
        if !t.is_empty() {
            return Ok(Zeroizing::new(t.to_string()));
        }
    }
    match stored_token(store) {
        Ok(Some((token, _))) => Ok(token),
        Ok(None) => Err(anyhow!(NO_TOKEN)),
        Err(e) => Err(anyhow!(
            "GITHUB_TOKEN isn't set, and reading the agent token from Windows Credential Manager failed: {e}"
        )),
    }
}

/// The stored agent token, trimmed, and the target that answered: [`AGENT_TOKEN_TARGET`], or
/// [`LEGACY_AGENT_TOKEN_TARGET`] when the new target is missing or blank. `None` when neither
/// holds a token. A failed read is an error, never a reason to try the next target.
fn stored_token(store: &dyn CredentialStore) -> Result<Option<(Zeroizing<String>, &'static str)>> {
    Ok(stored_credential(store)?
        .map(|(c, target)| (Zeroizing::new(c.secret.trim().to_string()), target)))
}

/// The credential [`stored_token`] reads, with the target that answered.
fn stored_credential(
    store: &dyn CredentialStore,
) -> Result<Option<(Credential, &'static str)>, lil_platform::credentials::CredentialError> {
    for target in READ_ORDER {
        if let Some(c) = store.get(target)?
            && !c.secret.trim().is_empty()
        {
            return Ok(Some((c, target)));
        }
    }
    Ok(None)
}

/// [`resolve_token`] with the real environment and Windows Credential Manager.
pub fn github_token() -> Result<Zeroizing<String>> {
    let env = std::env::var("GITHUB_TOKEN").ok().map(Zeroizing::new);
    resolve_token(env, &lil_platform::credentials::WindowsCredentialStore)
}

// ---------------------------------------------------------------------------------------------
// lil dev agent-token set|status|clear
// ---------------------------------------------------------------------------------------------

/// `lil dev agent-token set`. Returns the line to print on success; the error is a plain
/// sentence. The token is read with `prompt.passphrase` (no echo) and never shown again. It is
/// written only to [`AGENT_TOKEN_TARGET`]; a token under the legacy name is neither read nor
/// changed here, and "already stored" means stored under the new name.
pub fn set_token(prompt: &dyn Prompt, store: &dyn CredentialStore) -> Result<String, String> {
    if !prompt.is_interactive() {
        return Err(SET_NOT_INTERACTIVE.to_string());
    }
    let store_failed = |e: &dyn std::fmt::Display| {
        format!("Windows Credential Manager failed ({e}); nothing was stored")
    };
    let existing = store
        .get(AGENT_TOKEN_TARGET)
        .map_err(|e| store_failed(&e))?;
    if existing.is_some() && !prompt.confirm("An agent token is already stored. Replace it? (y/N)")
    {
        return Err("nothing was stored; the agent token already stored stays".to_string());
    }
    let entered = Zeroizing::new(prompt.passphrase(
        "Agent token (a fine-grained token limited to this repository; input is hidden): ",
    ));
    let token = Zeroizing::new(entered.trim().to_string());
    check_token(&token)?;
    let expiry = check_expiry(
        &prompt.line("Expiry date GitHub showed for the token (YYYY-MM-DD, or `none`):"),
    )?;
    store
        .set(
            AGENT_TOKEN_TARGET,
            &Credential {
                username: AGENT_TOKEN_USERNAME.to_string(),
                secret: token.clone(),
                comment: Some(format!("{EXPIRY_KEY}{expiry}")),
            },
        )
        .map_err(|e| store_failed(&e))?;
    Ok(format!(
        "Stored the agent token in Windows Credential Manager as {AGENT_TOKEN_TARGET} (starts with {}, expiry {expiry}).",
        first_four(&token)
    ))
}

/// A fine-grained token: `github_pat_` and then only printable ASCII without spaces, short
/// enough for Credential Manager. Errors never contain the token.
fn check_token(token: &str) -> Result<(), String> {
    if token.is_empty() {
        return Err("no token was entered; nothing was stored".to_string());
    }
    if !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(
            "the token has spaces or characters a GitHub token never has; nothing was stored"
                .to_string(),
        );
    }
    if !token.starts_with(FINE_GRAINED_PREFIX) {
        return Err(format!(
            "that isn't a fine-grained personal access token (those start with {FINE_GRAINED_PREFIX}); nothing was stored. Create one on GitHub limited to this repository, without administration or workflow permissions."
        ));
    }
    if token.len() > MAX_SECRET_BYTES {
        return Err("the token is too long for Credential Manager; nothing was stored".to_string());
    }
    Ok(())
}

/// `YYYY-MM-DD` (month 01-12, day 01-31) or `none`, as entered by the owner.
fn check_expiry(entered: &str) -> Result<String, String> {
    let e = entered.trim();
    if e.eq_ignore_ascii_case("none") {
        return Ok("none".to_string());
    }
    let b = e.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let shaped = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && digits(0..4)
        && digits(5..7)
        && digits(8..10);
    let in_range = shaped
        && matches!(e[5..7].parse::<u8>(), Ok(1..=12))
        && matches!(e[8..10].parse::<u8>(), Ok(1..=31));
    if in_range {
        Ok(e.to_string())
    } else {
        Err("the expiry must be a date like 2027-01-31, or `none`; nothing was stored".to_string())
    }
}

fn first_four(token: &str) -> String {
    token.chars().take(4).collect()
}

/// `lil dev agent-token status`: `(stored, text)`. The text names the target that answered (the
/// one [`resolve_token`] uses), the token's first four characters and the expiry entered at
/// `set`, never the token. A token found only under the legacy name counts as stored, since it
/// still works, and the text says how to store it under the new name.
pub fn status(store: &dyn CredentialStore) -> Result<(bool, String), String> {
    match stored_credential(store) {
        Ok(Some((c, target))) => {
            let expiry = c
                .comment
                .as_deref()
                .and_then(|c| c.strip_prefix(EXPIRY_KEY))
                .unwrap_or("not recorded");
            let mut text = format!(
                "Agent token: stored in Windows Credential Manager as {target}. Starts with: {}. Expiry entered at `set`: {expiry}.",
                first_four(c.secret.trim())
            );
            if target == LEGACY_AGENT_TOKEN_TARGET {
                text.push_str(" It is stored under the legacy name; run `lil dev agent-token set` at a terminal to store it under the new name.");
            }
            Ok((true, text))
        }
        Ok(None) => Ok((
            false,
            format!(
                "Agent token: none stored ({AGENT_TOKEN_TARGET}, or the legacy {LEGACY_AGENT_TOKEN_TARGET}). Run `lil dev agent-token set` at a terminal to store one."
            ),
        )),
        Err(e) => Err(format!(
            "couldn't read the agent token from Windows Credential Manager: {e}"
        )),
    }
}

/// `lil dev agent-token clear [--legacy]`: removes the token under both names, or with `legacy`
/// only the one under [`LEGACY_AGENT_TOKEN_TARGET`]. Like `set`, it only runs at a real
/// terminal. It only deletes; it never writes.
pub fn clear(
    prompt: &dyn Prompt,
    store: &dyn CredentialStore,
    legacy: bool,
) -> Result<String, String> {
    if !prompt.is_interactive() {
        return Err(CLEAR_NOT_INTERACTIVE.to_string());
    }
    let targets: &[&str] = if legacy {
        &[LEGACY_AGENT_TOKEN_TARGET]
    } else {
        &READ_ORDER
    };
    let mut removed = Vec::new();
    for &target in targets {
        match store.delete(target) {
            Ok(true) => removed.push(target),
            Ok(false) => {}
            Err(e) => {
                let done = if removed.is_empty() {
                    String::new()
                } else {
                    format!(" (already removed: {})", removed.join(", "))
                };
                return Err(format!(
                    "couldn't remove the agent token {target} from Windows Credential Manager: {e}{done}"
                ));
            }
        }
    }
    let which = if legacy { " under the legacy name" } else { "" };
    Ok(if removed.is_empty() {
        format!(
            "No agent token was stored{which} ({}); nothing changed.",
            targets.join(", ")
        )
    } else {
        format!(
            "Agent token{which} removed from Windows Credential Manager ({}).",
            removed.join(", ")
        )
    })
}

// ---------------------------------------------------------------------------------------------
// lil dev git-credential <get|store|erase>
// ---------------------------------------------------------------------------------------------

/// Git's credential helper protocol. `input` is what git writes on stdin. For `get` with
/// `protocol=https` and `host=github.com` it writes `username=x-access-token` and the token;
/// any other host, protocol or action gets an empty reply, and `token` isn't called.
pub fn git_credential(
    action: &str,
    input: &str,
    token: &dyn Fn() -> Result<Zeroizing<String>>,
    out: &mut dyn Write,
) -> Result<()> {
    if action != "get" {
        return Ok(());
    }
    let mut protocol = None;
    let mut host = None;
    for line in input.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }
        match line.split_once('=') {
            Some(("protocol", v)) => protocol = Some(v),
            Some(("host", v)) => host = Some(v),
            _ => {}
        }
    }
    let for_github =
        protocol == Some("https") && host.is_some_and(|h| h.eq_ignore_ascii_case("github.com"));
    if !for_github {
        return Ok(());
    }
    let token = token()?;
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
        bail!(
            "the GitHub token has characters git's credential protocol can't carry, so no credential was given"
        );
    }
    let answer = Zeroizing::new(format!(
        "username={AGENT_TOKEN_USERNAME}\npassword={}\n",
        token.as_str()
    ));
    out.write_all(answer.as_bytes())?;
    out.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// lil dev gh -- <args>
// ---------------------------------------------------------------------------------------------

/// Runs a prepared command and returns its exit code. Tests inject a recorder.
pub trait CommandRunner {
    fn run(&self, command: &mut Command) -> std::io::Result<i32>;
}

/// Runs the command for real, with inherited stdin, stdout and stderr.
#[derive(Debug)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, command: &mut Command) -> std::io::Result<i32> {
        command.status().map(|s| s.code().unwrap_or(1))
    }
}

/// Why `gh <args>` isn't run through `lil dev gh`, if it isn't. Everything is allow-listed:
///
/// - Top-level commands: [`GH_ALLOWED`]. `auth`, `alias`, `extension`, `config`, `label`,
///   `cache`, `browse` and the rest are refused: they read or change gh's own login and
///   settings, let an alias stand in for a refused command, or aren't needed for PR work.
/// - `pr`: any subcommand except `merge` (refused wherever `merge` appears among the
///   arguments, since gh accepts flags before the subcommand).
/// - `repo`, `workflow`, `release` and `run`: only the subcommands in [`GH_SUBCOMMANDS`], given
///   right after the command.
/// - `api`: read-only. Exactly one relative endpoint, never `graphql`, never one containing
///   `merge`; only the flags in [`api_refusal`]'s allow-list, each spelled out on its own.
///   Every other argument that starts with `-` is refused, so bundled short flags such as
///   `-iX` or `-iF` can't smuggle in a method or a field.
///
/// This narrows what an agent does by habit; the token's own permissions are the boundary.
pub fn gh_refusal(args: &[String]) -> Option<String> {
    let Some(first) = args.first() else {
        return Some(
            "give gh's arguments after `--`, for example `lil dev gh -- pr view 3`".to_string(),
        );
    };
    let first = first.to_ascii_lowercase();
    if !GH_ALLOWED.contains(&first.as_str()) {
        return Some(format!(
            "`gh {first}` isn't run through lil dev gh; only these are: {}",
            GH_ALLOWED.join(", ")
        ));
    }
    let rest = &args[1..];
    if first == "pr" && rest.iter().any(|a| a.eq_ignore_ascii_case("merge")) {
        return Some(
            "`gh pr merge` is refused: agents merge only through `lil dev merge <PR>`".to_string(),
        );
    }
    if first == "api" {
        return api_refusal(rest);
    }
    if let Some((_, subs)) = GH_SUBCOMMANDS.iter().find(|(c, _)| *c == first) {
        let sub = rest.first().map(|s| s.to_ascii_lowercase());
        if !sub.as_deref().is_some_and(|s| subs.contains(&s)) {
            return Some(format!(
                "only `gh {first} {}` run through lil dev gh",
                subs.join("|")
            ));
        }
    }
    None
}

/// The top-level `gh` commands `lil dev gh` runs.
pub const GH_ALLOWED: &[&str] = &[
    "pr",
    "issue",
    "search",
    "status",
    "api",
    "repo",
    "workflow",
    "release",
    "run",
    "help",
    "version",
    "--help",
    "--version",
];

/// Commands limited to some subcommands, which must come right after the command.
pub const GH_SUBCOMMANDS: &[(&str, &[&str])] = &[
    ("repo", &["view", "clone"]),
    ("workflow", &["list", "view"]),
    ("release", &["list", "view"]),
    ("run", &["list", "view", "watch"]),
];

/// Why `gh api <rest>` is refused, if it is. Allowed, each as its own argument:
///
/// - with a value (`--flag value` or `--flag=value`; short forms only as `-q value`):
///   `--jq`/`-q`, `--template`/`-t`, `--cache`, `--hostname` (only `github.com`) and
///   `--header`/`-H` (only `Accept:` and `X-GitHub-Api-Version:` headers);
/// - without one: `--paginate`, `--slurp`, `--include`/`-i`, `--silent`, `--verbose`.
///
/// With no method, field or input flag, gh makes a GET request.
fn api_refusal(rest: &[String]) -> Option<String> {
    let read_only = |why: &str| {
        Some(format!(
            "lil dev gh only makes read-only REST calls: {why}. Allowed api flags: --jq/-q, --template/-t, --paginate, --slurp, --include/-i, --silent, --verbose, --cache, --hostname github.com, -H/--header with Accept: or X-GitHub-Api-Version:"
        ))
    };
    let mut endpoints = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let arg = rest[i].as_str();
        i += 1;
        if !arg.starts_with('-') {
            endpoints.push(arg);
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v)),
            _ => (arg, None),
        };
        let takes_value = match flag {
            "--paginate" | "--slurp" | "--include" | "-i" | "--silent" | "--verbose" => false,
            "--jq" | "-q" | "--template" | "-t" | "--cache" | "--hostname" | "--header" | "-H" => {
                true
            }
            _ => return read_only(&format!("`{arg}` isn't an allowed api flag")),
        };
        if !takes_value {
            if inline.is_some() {
                return read_only(&format!("`{arg}` takes no value"));
            }
            continue;
        }
        let value = match inline {
            Some(v) => v,
            None => match rest.get(i) {
                Some(v) => {
                    i += 1;
                    v.as_str()
                }
                None => return read_only(&format!("`{arg}` needs a value")),
            },
        };
        match flag {
            "--hostname" if !value.eq_ignore_ascii_case("github.com") => {
                return read_only("the host must be github.com");
            }
            "--header" | "-H" => {
                let name = value
                    .split_once(':')
                    .map(|(n, _)| n.trim().to_ascii_lowercase());
                if !matches!(name.as_deref(), Some("accept" | "x-github-api-version")) {
                    return read_only("only Accept: and X-GitHub-Api-Version: headers are allowed");
                }
            }
            _ => {}
        }
    }
    let [endpoint] = endpoints.as_slice() else {
        return read_only("give exactly one endpoint");
    };
    let lower = endpoint.to_ascii_lowercase();
    if lower == "graphql" || lower.trim_start_matches('/') == "graphql" {
        return read_only("the GraphQL endpoint is refused, since a query can be a mutation");
    }
    if lower.contains("://") {
        return read_only("the endpoint must be a path on api.github.com");
    }
    if lower.contains("merge") {
        return Some(
            "merge API calls are refused: agents merge only through `lil dev merge <PR>`"
                .to_string(),
        );
    }
    None
}

/// `gh <args>` with `GH_TOKEN` set for the child only, and the caller's `GH_HOST` and
/// enterprise tokens and git's repository variables removed (gh runs git; R39). Never
/// `Debug`-print the result: it holds the token.
pub fn gh_command(args: &[String], token: &str) -> Command {
    let mut command = Command::new("gh");
    super::root::scrub_git_env(&mut command)
        .args(args)
        .env("GH_TOKEN", token)
        .env_remove("GH_HOST")
        .env_remove("GH_ENTERPRISE_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN");
    command
}

/// `lil dev gh -- <args>`: the exit code of `gh`, or a plain refusal.
pub fn run_gh_with(
    args: &[String],
    token: &dyn Fn() -> Result<Zeroizing<String>>,
    runner: &dyn CommandRunner,
) -> Result<i32, String> {
    if let Some(why) = gh_refusal(args) {
        return Err(why);
    }
    let token = token().map_err(|e| format!("{e:#}"))?;
    let mut command = gh_command(args, &token);
    runner
        .run(&mut command)
        .map_err(|e| format!("couldn't run gh: {e}"))
}

// ---------------------------------------------------------------------------------------------
// CLI glue.
// ---------------------------------------------------------------------------------------------

fn print_result(what: &str, result: Result<String, String>) -> i32 {
    match result {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("{what}: {e}");
            1
        }
    }
}

/// `lil dev agent-token set`.
pub fn run_set() -> i32 {
    print_result(
        "lil dev agent-token set",
        set_token(
            &super::reviewer::TerminalPrompt,
            &lil_platform::credentials::WindowsCredentialStore,
        ),
    )
}

/// `lil dev agent-token status`: exit 0 when a token is stored, 1 when not, 2 on an error.
pub fn run_status() -> i32 {
    match status(&lil_platform::credentials::WindowsCredentialStore) {
        Ok((stored, text)) => {
            println!("{text}");
            if stored { 0 } else { 1 }
        }
        Err(e) => {
            eprintln!("lil dev agent-token status: {e}");
            2
        }
    }
}

/// `lil dev agent-token clear [--legacy]`.
pub fn run_clear(legacy: bool) -> i32 {
    print_result(
        "lil dev agent-token clear",
        clear(
            &super::reviewer::TerminalPrompt,
            &lil_platform::credentials::WindowsCredentialStore,
            legacy,
        ),
    )
}

/// `lil dev git-credential <action>`: reads git's request on stdin, answers on stdout.
pub fn run_git_credential(action: &str) -> i32 {
    let input = match std::io::read_to_string(std::io::stdin()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lil dev git-credential: couldn't read git's request: {e}");
            return 1;
        }
    };
    let mut out = std::io::stdout().lock();
    match git_credential(action, &input, &github_token, &mut out) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("lil dev git-credential: {e:#}");
            1
        }
    }
}

/// `lil dev gh -- <args>`.
pub fn run_gh(args: &[String]) -> i32 {
    match run_gh_with(args, &github_token, &SystemRunner) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("lil dev gh: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lil_platform::credentials::{
        CredentialError, MemoryCredentialStore, TargetUse, check_target,
    };
    use lil_platform::names::{DEV_CREDENTIAL_PREFIX, LEGACY_DEV_CREDENTIAL_PREFIX};
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::ffi::OsString;

    const TOKEN: &str = "github_pat_11FAKEFAKE0123456789_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ";

    fn stored(store: &MemoryCredentialStore, secret: &str, comment: Option<&str>) {
        store
            .set(
                AGENT_TOKEN_TARGET,
                &Credential {
                    username: AGENT_TOKEN_USERNAME.into(),
                    secret: Zeroizing::new(secret.into()),
                    comment: comment.map(str::to_string),
                },
            )
            .unwrap();
    }

    /// A store whose every call fails, to show errors are refusals.
    struct BrokenStore;

    impl CredentialStore for BrokenStore {
        fn get(&self, _: &str) -> Result<Option<Credential>, CredentialError> {
            Err(CredentialError::Windows {
                call: "CredReadW",
                message: "access denied".into(),
            })
        }
        fn set(&self, _: &str, _: &Credential) -> Result<(), CredentialError> {
            Err(CredentialError::Windows {
                call: "CredWriteW",
                message: "access denied".into(),
            })
        }
        fn delete(&self, _: &str) -> Result<bool, CredentialError> {
            Err(CredentialError::Windows {
                call: "CredDeleteW",
                message: "access denied".into(),
            })
        }
    }

    /// A store that panics when touched: proves a path never reads it.
    struct Untouchable;

    impl CredentialStore for Untouchable {
        fn get(&self, _: &str) -> Result<Option<Credential>, CredentialError> {
            panic!("the store must not be read")
        }
        fn set(&self, _: &str, _: &Credential) -> Result<(), CredentialError> {
            panic!("the store must not be written")
        }
        fn delete(&self, _: &str) -> Result<bool, CredentialError> {
            panic!("the store must not be changed")
        }
    }

    /// A store that can hold entries under either name, including the legacy one the real
    /// store no longer writes. Seeding bypasses the checks; every call made by the code under
    /// test goes through `check_target` like the real store, and is recorded.
    #[derive(Default)]
    struct FakeStore {
        items: RefCell<std::collections::BTreeMap<String, Credential>>,
        writes: RefCell<Vec<String>>,
        deletes: RefCell<Vec<String>>,
    }

    impl FakeStore {
        fn with(entries: &[(&str, &str, Option<&str>)]) -> Self {
            let store = Self::default();
            for (target, secret, comment) in entries {
                store.items.borrow_mut().insert(
                    target.to_ascii_lowercase(),
                    Credential {
                        username: AGENT_TOKEN_USERNAME.into(),
                        secret: Zeroizing::new(secret.to_string()),
                        comment: comment.map(str::to_string),
                    },
                );
            }
            store
        }
        fn secret(&self, target: &str) -> Option<String> {
            self.items
                .borrow()
                .get(&target.to_ascii_lowercase())
                .map(|c| c.secret.to_string())
        }
    }

    impl CredentialStore for FakeStore {
        fn get(&self, target: &str) -> Result<Option<Credential>, CredentialError> {
            check_target(target, TargetUse::Read)?;
            Ok(self
                .items
                .borrow()
                .get(&target.to_ascii_lowercase())
                .cloned())
        }
        fn set(&self, target: &str, credential: &Credential) -> Result<(), CredentialError> {
            self.writes.borrow_mut().push(target.to_string());
            check_target(target, TargetUse::Write)?;
            self.items
                .borrow_mut()
                .insert(target.to_ascii_lowercase(), credential.clone());
            Ok(())
        }
        fn delete(&self, target: &str) -> Result<bool, CredentialError> {
            self.deletes.borrow_mut().push(target.to_string());
            check_target(target, TargetUse::Delete)?;
            Ok(self
                .items
                .borrow_mut()
                .remove(&target.to_ascii_lowercase())
                .is_some())
        }
    }

    const OLD_TOKEN: &str = "github_pat_22OLDOLDOLD0123456789_legacylegacylegacylegacylegacyle";

    /// The agent token's two targets sit under the new and the legacy owner-tooling prefix.
    #[test] // spec: QA-079
    fn agent_token_targets_are_the_new_and_legacy_dev_names() {
        assert_eq!(
            AGENT_TOKEN_TARGET,
            format!("{DEV_CREDENTIAL_PREFIX}github/agent-token")
        );
        assert_eq!(
            LEGACY_AGENT_TOKEN_TARGET,
            format!("{LEGACY_DEV_CREDENTIAL_PREFIX}github/agent-token")
        );
        // The real store writes the new target and refuses to write the legacy one.
        assert_eq!(check_target(AGENT_TOKEN_TARGET, TargetUse::Write), Ok(()));
        assert!(check_target(LEGACY_AGENT_TOKEN_TARGET, TargetUse::Write).is_err());
        for use_ in [TargetUse::Read, TargetUse::Delete] {
            assert_eq!(check_target(LEGACY_AGENT_TOKEN_TARGET, use_), Ok(()));
        }
    }

    // ---- a scripted prompt -----------------------------------------------------------------

    struct FakePrompt {
        interactive: bool,
        secrets: RefCell<VecDeque<String>>,
        lines: RefCell<VecDeque<String>>,
        confirms: RefCell<VecDeque<bool>>,
        asked: RefCell<Vec<String>>,
    }

    impl FakePrompt {
        fn new(secrets: &[&str], lines: &[&str], confirms: &[bool]) -> Self {
            Self {
                interactive: true,
                secrets: RefCell::new(secrets.iter().map(|s| s.to_string()).collect()),
                lines: RefCell::new(lines.iter().map(|s| s.to_string()).collect()),
                confirms: RefCell::new(confirms.iter().copied().collect()),
                asked: RefCell::default(),
            }
        }
        fn not_a_terminal() -> Self {
            Self {
                interactive: false,
                ..Self::new(&[], &[], &[])
            }
        }
    }

    impl Prompt for FakePrompt {
        fn is_interactive(&self) -> bool {
            self.interactive
        }
        fn passphrase(&self, label: &str) -> String {
            self.asked.borrow_mut().push(label.to_string());
            self.secrets
                .borrow_mut()
                .pop_front()
                .expect("unexpected hidden prompt")
        }
        fn confirm(&self, question: &str) -> bool {
            self.asked.borrow_mut().push(question.to_string());
            self.confirms
                .borrow_mut()
                .pop_front()
                .expect("unexpected confirmation")
        }
        fn line(&self, question: &str) -> String {
            self.asked.borrow_mut().push(question.to_string());
            self.lines
                .borrow_mut()
                .pop_front()
                .expect("unexpected line prompt")
        }
    }

    // ---- token resolution ------------------------------------------------------------------

    #[test] // spec: QA-079
    fn github_token_variable_comes_first_and_the_store_is_not_read() {
        let t = resolve_token(Some(Zeroizing::new(" ci-token \n".into())), &Untouchable).unwrap();
        assert_eq!(t.as_str(), "ci-token");
    }

    #[test] // spec: QA-079
    fn stored_agent_token_is_next() {
        let store = MemoryCredentialStore::new();
        stored(&store, TOKEN, Some("expires=2027-01-31"));
        for env in [None, Some(""), Some("   ")] {
            let t = resolve_token(env.map(|e| Zeroizing::new(e.to_string())), &store).unwrap();
            assert_eq!(t.as_str(), TOKEN);
        }
    }

    #[test] // spec: QA-079
    fn no_token_anywhere_explains_agent_token_set() {
        let e = resolve_token(None, &MemoryCredentialStore::new()).unwrap_err();
        let text = format!("{e:#}");
        assert!(text.contains("lil dev agent-token set"), "{text}");
        assert!(!text.contains("gh auth"), "{text}");
        // A blank stored token counts as none.
        let store = MemoryCredentialStore::new();
        stored(&store, "  ", None);
        assert!(resolve_token(None, &store).is_err());
    }

    #[test] // spec: QA-079
    fn a_store_error_is_an_error_not_a_missing_token() {
        let e = resolve_token(None, &BrokenStore).unwrap_err();
        assert!(format!("{e:#}").contains("access denied"), "{e:#}");
    }

    #[test] // spec: QA-079
    fn resolve_token_prefers_the_new_target() {
        let store = FakeStore::with(&[
            (LEGACY_AGENT_TOKEN_TARGET, OLD_TOKEN, None),
            (AGENT_TOKEN_TARGET, TOKEN, None),
        ]);
        assert_eq!(resolve_token(None, &store).unwrap().as_str(), TOKEN);
        let (token, target) = stored_token(&store).unwrap().unwrap();
        assert_eq!(token.as_str(), TOKEN);
        assert_eq!(target, AGENT_TOKEN_TARGET);
        // Reading never writes or deletes anything.
        assert!(store.writes.borrow().is_empty());
        assert!(store.deletes.borrow().is_empty());
    }

    /// Review Focus 1: a token stored only under the legacy name still authenticates `merge`,
    /// `gh` and `git push`.
    #[test] // spec: QA-079
    fn resolve_token_falls_back_to_the_legacy_target() {
        let padded = format!(" {OLD_TOKEN}\n");
        let store = FakeStore::with(&[(LEGACY_AGENT_TOKEN_TARGET, &padded, None)]);
        for env in [None, Some(""), Some("  ")] {
            let t = resolve_token(env.map(|e| Zeroizing::new(e.to_string())), &store).unwrap();
            assert_eq!(t.as_str(), OLD_TOKEN);
        }
        let (token, target) = stored_token(&store).unwrap().unwrap();
        assert_eq!(token.as_str(), OLD_TOKEN);
        assert_eq!(target, LEGACY_AGENT_TOKEN_TARGET);
        assert!(store.writes.borrow().is_empty());
        assert!(store.deletes.borrow().is_empty());
        // Any case of the legacy name is the same Credential Manager entry.
        let upper = LEGACY_AGENT_TOKEN_TARGET.to_ascii_uppercase();
        let store = FakeStore::with(&[(&upper, OLD_TOKEN, None)]);
        assert_eq!(resolve_token(None, &store).unwrap().as_str(), OLD_TOKEN);
        // GITHUB_TOKEN still comes first, and then nothing is read.
        let t = resolve_token(Some(Zeroizing::new("ci".into())), &Untouchable).unwrap();
        assert_eq!(t.as_str(), "ci");
        // Neither name: the same refusal as before.
        let e = resolve_token(None, &FakeStore::default()).unwrap_err();
        assert!(
            format!("{e:#}").contains("lil dev agent-token set"),
            "{e:#}"
        );
        assert!(stored_token(&FakeStore::default()).unwrap().is_none());
    }

    #[test] // spec: QA-079
    fn blank_new_target_falls_back() {
        for blank in ["", "   ", "\r\n"] {
            let store = FakeStore::with(&[
                (AGENT_TOKEN_TARGET, blank, None),
                (LEGACY_AGENT_TOKEN_TARGET, OLD_TOKEN, None),
            ]);
            assert_eq!(resolve_token(None, &store).unwrap().as_str(), OLD_TOKEN);
            assert_eq!(
                stored_token(&store).unwrap().unwrap().1,
                LEGACY_AGENT_TOKEN_TARGET
            );
            // Both blank: no token.
            let store = FakeStore::with(&[
                (AGENT_TOKEN_TARGET, blank, None),
                (LEGACY_AGENT_TOKEN_TARGET, blank, None),
            ]);
            assert!(resolve_token(None, &store).is_err());
            assert!(stored_token(&store).unwrap().is_none());
        }
    }

    /// A failed read of the new target is an error; it never quietly falls back to the legacy
    /// token.
    #[test] // spec: QA-079
    fn a_store_error_on_the_new_target_is_not_a_fallback_to_the_legacy_token() {
        struct NewTargetBroken(FakeStore);
        impl CredentialStore for NewTargetBroken {
            fn get(&self, target: &str) -> Result<Option<Credential>, CredentialError> {
                if target == AGENT_TOKEN_TARGET {
                    return BrokenStore.get(target);
                }
                self.0.get(target)
            }
            fn set(&self, t: &str, c: &Credential) -> Result<(), CredentialError> {
                self.0.set(t, c)
            }
            fn delete(&self, t: &str) -> Result<bool, CredentialError> {
                self.0.delete(t)
            }
        }
        let store = NewTargetBroken(FakeStore::with(&[(
            LEGACY_AGENT_TOKEN_TARGET,
            OLD_TOKEN,
            None,
        )]));
        let e = resolve_token(None, &store).unwrap_err();
        assert!(format!("{e:#}").contains("access denied"), "{e:#}");
        assert!(status(&store).is_err());
    }

    // ---- set -------------------------------------------------------------------------------

    #[test] // spec: QA-079
    fn set_refuses_without_a_terminal_and_touches_nothing() {
        let e = set_token(&FakePrompt::not_a_terminal(), &Untouchable).unwrap_err();
        assert_eq!(e, SET_NOT_INTERACTIVE);
        assert!(e.contains("terminal"));
    }

    #[test] // spec: QA-079
    fn set_stores_the_token_and_the_expiry() {
        let store = MemoryCredentialStore::new();
        let prompt = FakePrompt::new(&[TOKEN], &["2027-01-31"], &[]);
        let said = set_token(&prompt, &store).unwrap();
        assert!(!said.contains(TOKEN), "{said}");
        assert!(said.contains("2027-01-31"), "{said}");
        let c = store.get(AGENT_TOKEN_TARGET).unwrap().unwrap();
        assert_eq!(c.secret.as_str(), TOKEN);
        assert_eq!(c.username, AGENT_TOKEN_USERNAME);
        assert_eq!(c.comment.as_deref(), Some("expires=2027-01-31"));
        // The token was read from the hidden prompt, not an echoed line.
        assert_eq!(prompt.secrets.borrow().len(), 0);
        for q in prompt.asked.borrow().iter() {
            assert!(!q.contains(TOKEN));
        }
    }

    #[test] // spec: QA-079
    fn set_accepts_none_as_the_expiry_and_trims_the_token() {
        let store = MemoryCredentialStore::new();
        let prompt = FakePrompt::new(&[&format!("  {TOKEN}\r\n")], &["None"], &[]);
        set_token(&prompt, &store).unwrap();
        let c = store.get(AGENT_TOKEN_TARGET).unwrap().unwrap();
        assert_eq!(c.secret.as_str(), TOKEN);
        assert_eq!(c.comment.as_deref(), Some("expires=none"));
    }

    #[test] // spec: QA-079
    fn set_refuses_tokens_that_are_not_fine_grained_and_never_echoes_them() {
        for bad in [
            "",
            "ghp_classicclassicclassic",
            "gho_ownersloginoauthtoken",
            "github_pat_has space",
            "github_pat_has\u{7}bell",
        ] {
            let store = MemoryCredentialStore::new();
            let prompt = FakePrompt::new(&[bad], &["2027-01-31"], &[]);
            let e = set_token(&prompt, &store).unwrap_err();
            assert!(e.contains("nothing was stored"), "{e}");
            if bad.len() > 4 {
                assert!(!e.contains(bad), "{e}");
            }
            assert_eq!(store.get(AGENT_TOKEN_TARGET).unwrap(), None);
        }
        let long = format!("{FINE_GRAINED_PREFIX}{}", "a".repeat(MAX_SECRET_BYTES));
        let prompt = FakePrompt::new(&[&long], &["2027-01-31"], &[]);
        assert!(set_token(&prompt, &MemoryCredentialStore::new()).is_err());
    }

    #[test] // spec: QA-079
    fn set_refuses_a_malformed_expiry() {
        for bad in [
            "",
            "soon",
            "2027-13-01",
            "2027-1-31",
            "27-01-31",
            "2027-01-32",
        ] {
            let store = MemoryCredentialStore::new();
            let prompt = FakePrompt::new(&[TOKEN], &[bad], &[]);
            let e = set_token(&prompt, &store).unwrap_err();
            assert!(e.contains("nothing was stored"), "{bad:?}: {e}");
            assert_eq!(store.get(AGENT_TOKEN_TARGET).unwrap(), None);
        }
    }

    #[test] // spec: QA-079
    fn set_over_an_existing_token_asks_and_defaults_to_keeping_it() {
        let store = MemoryCredentialStore::new();
        stored(&store, "github_pat_old", Some("expires=2026-12-31"));
        // No: nothing is asked after the question, and the old token stays.
        let prompt = FakePrompt::new(&[], &[], &[false]);
        assert!(set_token(&prompt, &store).is_err());
        let c = store.get(AGENT_TOKEN_TARGET).unwrap().unwrap();
        assert_eq!(c.secret.as_str(), "github_pat_old");
        // Yes: replaced.
        let prompt = FakePrompt::new(&[TOKEN], &["2027-01-31"], &[true]);
        set_token(&prompt, &store).unwrap();
        let c = store.get(AGENT_TOKEN_TARGET).unwrap().unwrap();
        assert_eq!(c.secret.as_str(), TOKEN);
    }

    /// Review Focus 1: `set` writes the new name only. A token under the legacy name is left
    /// exactly as it was, and no write ever names it.
    #[test] // spec: QA-079
    fn set_writes_only_the_new_target() {
        let store = FakeStore::with(&[(
            LEGACY_AGENT_TOKEN_TARGET,
            OLD_TOKEN,
            Some("expires=2026-12-31"),
        )]);
        // Only a token under the new name counts as "already stored": nothing is asked.
        let prompt = FakePrompt::new(&[TOKEN], &["2027-01-31"], &[]);
        let said = set_token(&prompt, &store).unwrap();
        assert!(said.contains(AGENT_TOKEN_TARGET), "{said}");
        assert!(!said.contains(TOKEN), "{said}");
        assert_eq!(*store.writes.borrow(), vec![AGENT_TOKEN_TARGET.to_string()]);
        assert!(store.deletes.borrow().is_empty());
        assert_eq!(store.secret(AGENT_TOKEN_TARGET).as_deref(), Some(TOKEN));
        assert_eq!(
            store.secret(LEGACY_AGENT_TOKEN_TARGET).as_deref(),
            Some(OLD_TOKEN)
        );
        // From now on the new token answers.
        let (token, target) = stored_token(&store).unwrap().unwrap();
        assert_eq!((token.as_str(), target), (TOKEN, AGENT_TOKEN_TARGET));
        // Replacing it later writes the new name again, and only it.
        let prompt = FakePrompt::new(&[TOKEN], &["none"], &[true]);
        set_token(&prompt, &store).unwrap();
        assert_eq!(
            *store.writes.borrow(),
            vec![AGENT_TOKEN_TARGET.to_string(); 2]
        );
        assert!(store.deletes.borrow().is_empty());
        assert_eq!(
            store.secret(LEGACY_AGENT_TOKEN_TARGET).as_deref(),
            Some(OLD_TOKEN)
        );
    }

    #[test] // spec: QA-079
    fn set_reports_a_store_failure() {
        let prompt = FakePrompt::new(&[TOKEN], &["2027-01-31"], &[]);
        let e = set_token(&prompt, &BrokenStore).unwrap_err();
        assert!(e.contains("access denied"), "{e}");
        assert!(!e.contains(TOKEN), "{e}");
    }

    // ---- status and clear ------------------------------------------------------------------

    #[test] // spec: QA-079
    fn status_shows_presence_prefix_and_expiry_but_never_the_token() {
        let store = MemoryCredentialStore::new();
        stored(&store, TOKEN, Some("expires=2027-01-31"));
        let (present, text) = status(&store).unwrap();
        assert!(present);
        assert!(text.contains("gith"), "{text}");
        assert!(text.contains("2027-01-31"), "{text}");
        assert!(text.contains(AGENT_TOKEN_TARGET), "{text}");
        assert!(!text.contains(TOKEN), "{text}");
        // The target name contains "github", so look past it.
        assert!(
            !text.replace(AGENT_TOKEN_TARGET, "").contains(&TOKEN[..5]),
            "more than four characters shown: {text}"
        );
    }

    #[test] // spec: QA-079
    fn status_without_a_token_says_how_to_store_one() {
        let (present, text) = status(&MemoryCredentialStore::new()).unwrap();
        assert!(!present);
        assert!(text.contains("lil dev agent-token set"), "{text}");
        // A token stored without an expiry still never shows itself.
        let store = MemoryCredentialStore::new();
        stored(&store, TOKEN, None);
        let (present, text) = status(&store).unwrap();
        assert!(present);
        assert!(!text.contains(TOKEN), "{text}");
        assert!(status(&BrokenStore).is_err());
    }

    #[test] // spec: QA-079
    fn clear_removes_the_token() {
        let at_terminal = FakePrompt::new(&[], &[], &[]);
        let store = MemoryCredentialStore::new();
        stored(&store, TOKEN, None);
        assert!(
            clear(&at_terminal, &store, false)
                .unwrap()
                .contains("removed")
        );
        assert_eq!(store.get(AGENT_TOKEN_TARGET).unwrap(), None);
        assert!(
            clear(&at_terminal, &store, false)
                .unwrap()
                .contains("No agent token")
        );
        assert!(clear(&at_terminal, &BrokenStore, false).is_err());
        assert!(clear(&at_terminal, &BrokenStore, true).is_err());
    }

    #[test] // spec: QA-079
    fn clear_refuses_without_a_terminal_and_touches_nothing() {
        for legacy in [false, true] {
            let e = clear(&FakePrompt::not_a_terminal(), &Untouchable, legacy).unwrap_err();
            assert_eq!(e, CLEAR_NOT_INTERACTIVE);
            assert!(e.contains("terminal"));
        }
    }

    #[test] // spec: QA-079
    fn status_names_the_target_that_answered() {
        // The new name answers: no word about the legacy one.
        let store = FakeStore::with(&[
            (AGENT_TOKEN_TARGET, TOKEN, Some("expires=2027-01-31")),
            (
                LEGACY_AGENT_TOKEN_TARGET,
                OLD_TOKEN,
                Some("expires=2026-12-31"),
            ),
        ]);
        let (present, text) = status(&store).unwrap();
        assert!(present);
        assert!(text.contains(AGENT_TOKEN_TARGET), "{text}");
        assert!(text.contains("2027-01-31"), "{text}");
        assert!(!text.contains(LEGACY_AGENT_TOKEN_TARGET), "{text}");
        assert!(!text.contains("legacy"), "{text}");
        // Only the legacy name holds a token: it is named, and the owner is told how to move it.
        let store = FakeStore::with(&[(
            LEGACY_AGENT_TOKEN_TARGET,
            OLD_TOKEN,
            Some("expires=2026-12-31"),
        )]);
        let (present, text) = status(&store).unwrap();
        assert!(
            present,
            "a legacy token still works, so it counts as stored"
        );
        assert!(text.contains(LEGACY_AGENT_TOKEN_TARGET), "{text}");
        assert!(text.contains("2026-12-31"), "{text}");
        assert!(
            text.contains(
                "stored under the legacy name; run `lil dev agent-token set` at a terminal to store it under the new name"
            ),
            "{text}"
        );
        assert!(!text.contains(OLD_TOKEN), "{text}");
        let without_names = text
            .replace(LEGACY_AGENT_TOKEN_TARGET, "")
            .replace(AGENT_TOKEN_TARGET, "");
        assert!(
            !without_names.contains(&OLD_TOKEN[..5]),
            "more than four characters shown: {text}"
        );
        assert!(store.writes.borrow().is_empty());
        assert!(store.deletes.borrow().is_empty());
        // A blank new entry doesn't hide the legacy token.
        let store = FakeStore::with(&[
            (AGENT_TOKEN_TARGET, " ", None),
            (LEGACY_AGENT_TOKEN_TARGET, OLD_TOKEN, None),
        ]);
        let (present, text) = status(&store).unwrap();
        assert!(present);
        assert!(text.contains(LEGACY_AGENT_TOKEN_TARGET), "{text}");
        // Neither: both names are given in the "none stored" text.
        let (present, text) = status(&FakeStore::default()).unwrap();
        assert!(!present);
        assert!(text.contains(AGENT_TOKEN_TARGET), "{text}");
        assert!(text.contains(LEGACY_AGENT_TOKEN_TARGET), "{text}");
        assert!(text.contains("lil dev agent-token set"), "{text}");
    }

    #[test] // spec: QA-079
    fn clear_removes_both_targets() {
        let at_terminal = FakePrompt::new(&[], &[], &[]);
        let store = FakeStore::with(&[
            (AGENT_TOKEN_TARGET, TOKEN, None),
            (LEGACY_AGENT_TOKEN_TARGET, OLD_TOKEN, None),
        ]);
        let said = clear(&at_terminal, &store, false).unwrap();
        assert!(said.contains(AGENT_TOKEN_TARGET), "{said}");
        assert!(said.contains(LEGACY_AGENT_TOKEN_TARGET), "{said}");
        assert_eq!(store.secret(AGENT_TOKEN_TARGET), None);
        assert_eq!(store.secret(LEGACY_AGENT_TOKEN_TARGET), None);
        assert!(store.writes.borrow().is_empty());
        let mut deleted = store.deletes.borrow().clone();
        deleted.sort();
        let mut both = vec![
            AGENT_TOKEN_TARGET.to_string(),
            LEGACY_AGENT_TOKEN_TARGET.to_string(),
        ];
        both.sort();
        assert_eq!(deleted, both);
        assert!(resolve_token(None, &store).is_err());
        // Only one of them stored: it goes, and nothing answers afterwards.
        for only in [AGENT_TOKEN_TARGET, LEGACY_AGENT_TOKEN_TARGET] {
            let store = FakeStore::with(&[(only, TOKEN, None)]);
            let said = clear(&at_terminal, &store, false).unwrap();
            assert!(said.contains("removed"), "{said}");
            assert!(said.contains(only), "{said}");
            assert!(stored_token(&store).unwrap().is_none());
        }
        // Nothing stored: nothing changed, and both names are given.
        let said = clear(&at_terminal, &FakeStore::default(), false).unwrap();
        assert!(said.contains("No agent token"), "{said}");
        assert!(said.contains(AGENT_TOKEN_TARGET), "{said}");
        assert!(said.contains(LEGACY_AGENT_TOKEN_TARGET), "{said}");
    }

    /// Review Focus 1: `clear --legacy` removes the old entry and nothing else.
    #[test] // spec: QA-079
    fn clear_legacy_removes_only_the_legacy_target() {
        let at_terminal = FakePrompt::new(&[], &[], &[]);
        let store = FakeStore::with(&[
            (AGENT_TOKEN_TARGET, TOKEN, Some("expires=2027-01-31")),
            (LEGACY_AGENT_TOKEN_TARGET, OLD_TOKEN, None),
        ]);
        let said = clear(&at_terminal, &store, true).unwrap();
        assert!(said.contains(LEGACY_AGENT_TOKEN_TARGET), "{said}");
        assert!(said.contains("removed"), "{said}");
        assert_eq!(
            *store.deletes.borrow(),
            vec![LEGACY_AGENT_TOKEN_TARGET.to_string()]
        );
        assert!(store.writes.borrow().is_empty());
        assert_eq!(store.secret(LEGACY_AGENT_TOKEN_TARGET), None);
        assert_eq!(store.secret(AGENT_TOKEN_TARGET).as_deref(), Some(TOKEN));
        // The new token still answers.
        assert_eq!(resolve_token(None, &store).unwrap().as_str(), TOKEN);
        // Again: nothing to remove, and still only the legacy name is touched.
        let said = clear(&at_terminal, &store, true).unwrap();
        assert!(said.contains("nothing changed"), "{said}");
        assert_eq!(
            *store.deletes.borrow(),
            vec![LEGACY_AGENT_TOKEN_TARGET.to_string(); 2]
        );
        assert_eq!(store.secret(AGENT_TOKEN_TARGET).as_deref(), Some(TOKEN));
    }

    // ---- git-credential --------------------------------------------------------------------

    fn helper(action: &str, input: &str) -> (Result<()>, String, usize) {
        let calls = Cell::new(0);
        let token = || {
            calls.set(calls.get() + 1);
            Ok(Zeroizing::new(TOKEN.to_string()))
        };
        let mut out = Vec::new();
        let r = git_credential(action, input, &token, &mut out);
        (r, String::from_utf8(out).unwrap(), calls.get())
    }

    #[test] // spec: QA-079
    fn helper_answers_github_com_with_the_token() {
        let (r, out, calls) = helper("get", "protocol=https\nhost=github.com\n\n");
        r.unwrap();
        assert_eq!(out, format!("username=x-access-token\npassword={TOKEN}\n"));
        assert_eq!(calls, 1);
        // Extra attributes and a path don't matter; the host compares case-insensitively.
        let (r, out, _) = helper(
            "get",
            "protocol=https\nhost=GitHub.com\npath=xenofei/repo.git\nwwwauth[]=Basic\n",
        );
        r.unwrap();
        assert!(out.contains(&format!("password={TOKEN}")));
    }

    #[test] // spec: QA-079
    fn helper_gives_other_hosts_and_protocols_nothing() {
        for input in [
            "protocol=https\nhost=example.com\n\n",
            "protocol=https\nhost=github.com.evil.example\n\n",
            "protocol=https\nhost=gist.github.com\n\n",
            "protocol=https\nhost=github.com:8443\n\n",
            "protocol=http\nhost=github.com\n\n",
            "host=github.com\n\n",
            "protocol=https\n\n",
            "",
            "protocol=https\n\nhost=github.com\n",
        ] {
            let (r, out, calls) = helper("get", input);
            r.unwrap();
            assert_eq!(out, "", "{input:?}");
            assert_eq!(calls, 0, "the token was looked up for {input:?}");
        }
    }

    #[test] // spec: QA-079
    fn helper_store_and_erase_are_no_ops() {
        for action in ["store", "erase", "capability", "something-new"] {
            let (r, out, calls) = helper(
                action,
                &format!("protocol=https\nhost=github.com\nusername=x\npassword={TOKEN}\n\n"),
            );
            r.unwrap();
            assert_eq!(out, "", "{action}");
            assert_eq!(calls, 0, "{action}");
        }
    }

    #[test] // spec: QA-079
    fn helper_without_a_token_answers_nothing_and_errors() {
        let token = || -> Result<Zeroizing<String>> { Err(anyhow!(NO_TOKEN)) };
        let mut out = Vec::new();
        let e = git_credential("get", "protocol=https\nhost=github.com\n", &token, &mut out)
            .unwrap_err();
        assert!(format!("{e:#}").contains("lil dev agent-token set"));
        assert!(out.is_empty());
    }

    #[test] // spec: QA-079
    fn git_credential_get_falls_back_to_the_legacy_target() {
        let store = FakeStore::with(&[(LEGACY_AGENT_TOKEN_TARGET, OLD_TOKEN, None)]);
        let token = || resolve_token(None, &store);
        let mut out = Vec::new();
        git_credential(
            "get",
            "protocol=https\nhost=github.com\n\n",
            &token,
            &mut out,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("username=x-access-token\npassword={OLD_TOKEN}\n")
        );
        assert!(store.writes.borrow().is_empty());
        assert!(store.deletes.borrow().is_empty());
    }

    #[test] // spec: QA-079
    fn helper_refuses_a_token_that_would_break_the_protocol() {
        for bad in ["a\nusername=evil", "a b", ""] {
            let token = || Ok(Zeroizing::new(bad.to_string()));
            let mut out = Vec::new();
            assert!(
                git_credential("get", "protocol=https\nhost=github.com\n", &token, &mut out)
                    .is_err()
            );
            assert!(out.is_empty(), "{bad:?}");
        }
    }

    // ---- gh --------------------------------------------------------------------------------

    /// A recorded command: program, arguments and environment changes.
    type Ran = (OsString, Vec<OsString>, Vec<(OsString, Option<OsString>)>);

    /// Records what would have run, without running anything.
    #[derive(Default)]
    struct Recorder {
        ran: RefCell<Vec<Ran>>,
    }

    impl CommandRunner for Recorder {
        fn run(&self, command: &mut Command) -> std::io::Result<i32> {
            self.ran.borrow_mut().push((
                command.get_program().to_owned(),
                command.get_args().map(ToOwned::to_owned).collect(),
                command
                    .get_envs()
                    .map(|(k, v)| (k.to_owned(), v.map(ToOwned::to_owned)))
                    .collect(),
            ));
            Ok(3)
        }
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn fake_token() -> Result<Zeroizing<String>> {
        Ok(Zeroizing::new(TOKEN.to_string()))
    }

    #[test] // spec: QA-079
    fn gh_runs_with_gh_token_in_the_child_only() {
        let rec = Recorder::default();
        let code = run_gh_with(&args(&["pr", "view", "3"]), &fake_token, &rec).unwrap();
        assert_eq!(code, 3);
        let ran = rec.ran.borrow();
        let (program, a, envs) = &ran[0];
        assert_eq!(program, "gh");
        assert_eq!(a, &["pr", "view", "3"]);
        let gh_token = envs
            .iter()
            .find(|(k, _)| k == "GH_TOKEN")
            .and_then(|(_, v)| v.clone());
        assert!(gh_token.as_deref() == Some(std::ffi::OsStr::new(TOKEN)));
        // Hosts and enterprise tokens from the caller's environment don't reach gh.
        for removed in ["GH_HOST", "GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"] {
            assert!(
                envs.iter().any(|(k, v)| k == removed && v.is_none()),
                "{removed}"
            );
        }
        // This process's environment is untouched.
        assert_ne!(
            std::env::var_os("GH_TOKEN").as_deref(),
            Some(std::ffi::OsStr::new(TOKEN))
        );
    }

    #[test] // spec: QA-036
    fn gh_refuses_merges_logins_and_writes_through_the_api() {
        for bad in [
            &[][..],
            &["pr", "merge", "3"],
            &["pr", "--repo", "x/y", "merge", "3"],
            &["pr", "MERGE", "3"],
            &["auth", "token"],
            &["auth", "status", "--show-token"],
            &["alias", "set", "m", "pr merge"],
            &["extension", "install", "x/y"],
            &["config", "set", "git_protocol", "ssh"],
            &["api", "-X", "PUT", "repos/x/y/pulls/3/merge"],
            &["api", "repos/x/y/pulls/3/merge"],
            &["api", "--method", "PATCH", "repos/x/y/git/refs/heads/main"],
            &["api", "--method=DELETE", "repos/x/y/git/refs/heads/x"],
            &["api", "-XPATCH", "repos/x/y/git/refs/heads/main"],
            &["api", "graphql", "-f", "query=mutation{}"],
            &["api", "repos/x/y/issues", "-F", "title=x"],
            &["api", "repos/x/y/issues", "--field", "title=x"],
            &["api", "repos/x/y/issues", "--raw-field", "title=x"],
            &["api", "graphql", "--input", "body.json"],
            &["repo", "sync"],
            // Fix round 1 (I1): bundled short flags and every spelling of a write flag.
            &["api", "-iX", "DELETE", "repos/x/y/git/refs/heads/main"],
            &["api", "-iXPUT", "repos/x/y/pulls/3"],
            &["api", "-iF", "query=@q.graphql", "graphql"],
            &["api", "-iF", "query=@q.graphql", "repos/x/y/issues"],
            &["api", "-Xi", "GET", "repos/x/y"],
            &["api", "-XGET", "repos/x/y"],
            &["api", "-fx=1", "repos/x/y"],
            &["api", "-Fx=1", "repos/x/y"],
            &["api", "--method", "GET", "repos/x/y"],
            &["api", "--method=GET", "repos/x/y"],
            &["api", "--METHOD", "GET", "repos/x/y"],
            &["api", "--field", "a=b", "repos/x/y"],
            &["api", "--field=a=b", "repos/x/y"],
            &["api", "--raw-field", "a=b", "repos/x/y"],
            &["api", "--raw-field=a=b", "repos/x/y"],
            &["api", "--input", "body.json", "repos/x/y"],
            &["api", "--input=body.json", "repos/x/y"],
            &["api", "--preview", "x", "repos/x/y"],
            &["api", "-p", "x", "repos/x/y"],
            &["api", "-iq", ".x", "repos/x/y"],
            // The GraphQL endpoint, in any case, and any query through it.
            &["api", "graphql"],
            &["api", "GraphQL", "-q", ".data"],
            // Only Accept and X-GitHub-Api-Version headers; only github.com.
            &["api", "-H", "Authorization: token x", "repos/x/y"],
            &[
                "api",
                "--header=X-HTTP-Method-Override: DELETE",
                "repos/x/y",
            ],
            &["api", "-HAccept:x", "repos/x/y"],
            &["api", "--hostname", "evil.example", "repos/x/y"],
            &["api", "--hostname=evil.example", "repos/x/y"],
            // Exactly one relative endpoint.
            &["api"],
            &["api", "repos/x/y", "repos/x/z"],
            &["api", "https://evil.example/x"],
            &["api", "--", "repos/x/y"],
            &["api", "--jq"],
            // Narrowed subcommands.
            &["repo", "edit", "--visibility", "private"],
            &["repo", "delete", "x/y"],
            &["repo"],
            &["repo", "-R", "x/y", "view"],
            &["workflow", "run", "ci.yml"],
            &["workflow", "enable", "ci.yml"],
            &["release", "create", "v1"],
            &["release", "delete", "v1"],
            &["run", "rerun", "5"],
            &["run", "cancel", "5"],
            &["run", "delete", "5"],
            &["label", "create", "x"],
            &["cache", "delete", "--all"],
            &["browse"],
        ] {
            let rec = Recorder::default();
            let e = run_gh_with(&args(bad), &fake_token, &rec).unwrap_err();
            assert!(!e.is_empty(), "{bad:?}");
            assert!(rec.ran.borrow().is_empty(), "{bad:?} ran");
        }
        let e = gh_refusal(&args(&["pr", "merge", "3"])).unwrap();
        assert!(e.contains("lil dev merge"), "{e}");
    }

    #[test] // spec: QA-079
    fn gh_allows_ordinary_pr_issue_run_and_read_only_api_work() {
        for ok in [
            &["pr", "create", "--fill"][..],
            &["pr", "view", "3"],
            &["pr", "checks", "3"],
            &["pr", "create", "--title", "merge guard docs"],
            &["issue", "list"],
            &["run", "list"],
            &["api", "repos/x/y/pulls/3"],
            &["api", "--paginate", "repos/x/y/commits/abc/check-runs"],
            &["repo", "view"],
            &["--version"],
            // Fix round 1: the read-only api flags, and the narrowed subcommands.
            &["api", "repos/o/r/pulls/3", "--jq", ".head.sha"],
            &["api", "repos/o/r/pulls/3", "--jq=.mergeable"],
            &["api", "-q", ".x", "repos/o/r"],
            &["api", "-t", "{{.name}}", "repos/o/r"],
            &["api", "--template={{.name}}", "repos/o/r"],
            &["api", "--paginate", "--slurp", "repos/o/r/pulls"],
            &["api", "-i", "repos/o/r"],
            &["api", "--include", "--silent", "--verbose", "repos/o/r"],
            &["api", "--cache", "1h", "repos/o/r"],
            &["api", "--cache=1h", "repos/o/r"],
            &["api", "--hostname", "github.com", "repos/o/r"],
            &["api", "--hostname=GitHub.com", "repos/o/r"],
            &[
                "api",
                "-H",
                "Accept: application/vnd.github+json",
                "repos/o/r",
            ],
            &[
                "api",
                "--header",
                "X-GitHub-Api-Version: 2022-11-28",
                "repos/o/r",
            ],
            &["api", "--header=accept: application/json", "repos/o/r"],
            &["api", "/repos/o/r/commits/abc/check-runs"],
            &["pr", "list"],
            &["pr", "diff", "3"],
            &["pr", "comment", "3", "--body", "x"],
            &["pr", "edit", "3", "--title", "x"],
            &["pr", "ready", "3"],
            &["pr", "review", "3", "--comment", "--body", "x"],
            &["pr", "status"],
            &["pr", "close", "3"],
            &["pr", "checkout", "3"],
            &["issue", "create", "--title", "x", "--body", "y"],
            &["issue", "view", "3"],
            &["search", "prs", "x"],
            &["status"],
            &["repo", "clone", "o/r"],
            &["workflow", "list"],
            &["workflow", "view", "ci.yml"],
            &["release", "list"],
            &["release", "view", "v1"],
            &["run", "view", "5", "--log"],
            &["run", "watch", "5"],
            &["help"],
            &["--help"],
        ] {
            assert_eq!(gh_refusal(&args(ok)), None, "{ok:?}");
        }
    }

    #[test] // spec: QA-079
    fn gh_without_a_token_runs_nothing() {
        let rec = Recorder::default();
        let none = || -> Result<Zeroizing<String>> { Err(anyhow!(NO_TOKEN)) };
        let e = run_gh_with(&args(&["pr", "view", "3"]), &none, &rec).unwrap_err();
        assert!(e.contains("lil dev agent-token set"), "{e}");
        assert!(rec.ran.borrow().is_empty());
    }

    #[test] // spec: QA-079
    fn gh_command_is_gh_with_the_given_args() {
        let c = gh_command(&args(&["issue", "list"]), "t");
        assert_eq!(c.get_program(), "gh");
        assert_eq!(c.get_args().collect::<Vec<_>>(), ["issue", "list"]);
    }

    /// `tests/credential_helper.rs` runs this same entry through git, from every worktree.
    #[test]
    fn helper_entry_runs_the_committed_script() {
        assert_eq!(
            GIT_CREDENTIAL_HELPER,
            r#"!sh "$(git rev-parse --show-toplevel)/tools/git-hooks/credential-helper""#
        );
    }
}
