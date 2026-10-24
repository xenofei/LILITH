//! `lil dev reviewer-setup` and `lil dev approve` (QA-020 as amended).
//!
//! Changes to protected files merge only with an APPROVED review from the reviewer GitHub App
//! (`reviewer_app_slug` in `dev/merge-guard.toml`) on the PR's exact head commit. This module is the only thing that
//! posts that review, and it does so only for the owner, at a real terminal, after the owner has
//! read the guard's summary, said yes, and typed the passphrase that unlocks the App's key.
//! Agents have no terminal, so they can never approve.
//!
//! **The key.** `lil dev reviewer-setup` encrypts the App's downloaded `.pem` with an age scrypt
//! passphrase and writes it, with the App id and installation id, to
//! `%USERPROFILE%\<home>\reviewer\app-key.age` ([`key_path_in`]). `approve` reads the key from
//! there ([`find_key_in`]) and prints the path. It is outside the repo and outside the suite's
//! `%APPDATA%\<App>*` / `%LOCALAPPDATA%\<App>*` data, and the repo's `.claude` deny rules keep
//! agents out of the home folder. The plaintext `.pem` is never
//! deleted or modified here; the owner deletes it.
//!
//! **Approving** ([`approve`]): refuse unless at a terminal; refuse when nothing protected
//! changed; show the summary and ask "(y/N)"; read the passphrase; decrypt the key (a wrong
//! passphrase stops here, before any network call); mint a 9-minute RS256 App JWT
//! ([`app_jwt`]); ask GitHub for the App's live slug with it (`GET /app`), print it, and refuse
//! unless it is `reviewer_app_slug` (plan 2026-10-17, D6), so a key for any other App approves
//! nothing; exchange the JWT for an installation token; re-read the
//! PR and refuse unless it is
//! still open, not a draft, into the base branch and at the exact head the summary was computed
//! for; then post exactly one `APPROVE` review pinned to that head; then re-run the guard check.
//!
//! **Re-running the guard check** (ruling R40). `ci-guard.yml` runs only on `pull_request`
//! (`opened`, `synchronize`, `reopened`), and its runs from before the approval failed. Every
//! workflow run is its own check suite, and a commit can have more than one (a close and
//! reopen), so after the review is posted `approve` re-runs every finished, non-green
//! `pull_request` run of that workflow for this PR at the approved head ([`guard_runs_for`]:
//! this repository, not a fork; `action_required` runs are never re-run). A re-run keeps the
//! original event payload and the guard reads the reviews live, so it passes. GitHub re-runs only
//! finished runs, so unfinished ones are awaited for up to [`RERUN_WAIT`]. Nothing here can undo
//! the approval: when no run is found, one is still running, the PR head moved, or GitHub
//! refuses (for example because the App lacks Actions write), `approve` still succeeds and
//! returns a [`Rerun::Warning`] saying where to re-run the check by hand.
//!
//! **The token** is requested for this repo only, with `pull_requests: write` and
//! `actions: write` ([`TokenScope::ReviewAndRerun`]). If GitHub refuses that with 403 or 422
//! (usually because the installation hasn't granted Actions write yet), a `pull_requests: write`
//! token is requested instead and the re-run is skipped with a warning quoting GitHub's
//! refusal. The token is held in a [`RevokeOnDrop`], so it is revoked
//! (`DELETE /installation/token`) after the post and the re-runs on every path, including an
//! error or a panic; a failed revoke is only a warning, since the token expires within an hour
//! anyway. A killed process (Ctrl+C) doesn't unwind, so then the token lives until it expires.
//!
//! **Secrets.** The passphrase, the decrypted PEM, the JWT and the installation token are never
//! printed or logged, are held in [`Zeroizing`] buffers where this code owns them, and live only
//! for the one approval. `Debug` on [`ReviewerKeyFile`] redacts the key material.
//!
//! **Run it from a trusted build.** Build the `lil` that runs `lil dev approve` from a clean
//! worktree of `origin/main` that agents can't write to. A binary built from a tree an agent can
//! change could capture the passphrase as it is typed.
//!
//! **What the owner reads** is escaped: control characters in paths (ESC sequences that move the
//! cursor or erase lines) are printed as `\u{1b}` and so on, and such paths always need approval
//! (ruling R23).
//!
//! **Windows Hello and the merge flow** (plan 4a Task 28: QA-088, QA-036, P4a-R38). When you
//! enrolled Windows Hello (`lil dev reviewer-hello`, `reviewer_hello.rs`), `approve` unlocks the
//! key with it first and asks for the passphrase only when Hello is cancelled or fails; the
//! passphrase file stays. The key file and its encryption are in `reviewer_key.rs`.
//! [`with_owner_authenticator`] is the authentication every `lil dev merge` and every
//! `lil dev merge-flow` asks for, and [`run_merge_flow`] wires the flow (`merge_flow.rs`) to git
//! and GitHub; the flow posts the approval with [`approve_with_key`] after you authenticated.

use super::github::{API_BASE, CheckState, GitHubApi, PrInfo, WorkflowRun};
use super::guard;
use super::merge::{
    self, MergeGuardConfig, blocker_lines, check_origin, committed_config, explain, fetch,
    is_commit_sha, one_line, short,
};
use super::merge_flow::{Facts, MergeFlowSteps, MergeGate, merge_flow};
use super::reviewer_hello::{
    Authenticator, KeyAuthenticator, PassphraseUnlocker, Unlocker, WindowsHello, enrolled_hello,
    unlock_reviewer_key,
};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::Method;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue, USER_AGENT};
use serde::Serialize;
use serde_json::Value;
use std::fmt;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroize::Zeroizing;

/// The shortest passphrase `reviewer-setup` accepts, in characters.
pub const MIN_PASSPHRASE_CHARS: usize = 12;

pub const NOT_INTERACTIVE: &str = "lil dev approve only runs at a real terminal (stdin and stderr must both be one); nothing was approved";
pub const WRONG_PASSPHRASE: &str = "wrong passphrase; nothing was posted";
pub const HEAD_MOVED: &str = "the PR head moved since the summary; nothing was approved";
pub const NOTHING_PROTECTED: &str = "nothing protected to approve";

const SETUP_NOT_INTERACTIVE: &str = "lil dev reviewer-setup only runs at a real terminal (stdin and stderr must both be one); nothing was written";

// The key file and its encryption live in `reviewer_key` (Task 28), so that they compile on
// their own in `tests/reviewer_hello.rs`; every name stays reachable from here.
pub use super::reviewer_key::{
    DAMAGED_KEY, KeyError, ReviewerKeyFile, WORK_FACTOR, encrypt_key_with_work_factor,
    setup_key_path,
};
#[cfg(test)]
pub use super::reviewer_key::{
    MAX_WORK_FACTOR, decrypt_key, encrypt_key, find_key_in, key_path_in, setup_key_path_in,
};
use super::reviewer_key::{load_key_in, user_profile};
#[cfg(test)]
use {super::reviewer_key::locate_key_in, lil_platform::names};

// ---------------------------------------------------------------------------------------------
// The App JWT.
// ---------------------------------------------------------------------------------------------

#[derive(Serialize)]
struct AppClaims {
    iat: i64,
    exp: i64,
    iss: String,
}

/// An RS256 GitHub App JWT: `iat` = now - 60 (clock drift), `exp` = now + 540 (GitHub's limit is
/// 10 minutes), `iss` = the App id.
pub fn app_jwt(app_id: u64, pem: &[u8], now_unix: i64) -> Result<String> {
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(pem)
        .map_err(|_| anyhow!("the key is not an RSA private key in PEM form"))?;
    let claims = AppClaims {
        iat: now_unix - 60,
        exp: now_unix + 540,
        iss: app_id.to_string(),
    };
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
        &claims,
        &key,
    )
    .map_err(|e| anyhow!("signing the App JWT failed ({e}); is the key an RSA private key?"))
}

// ---------------------------------------------------------------------------------------------
// The installation-token exchange.
// ---------------------------------------------------------------------------------------------

/// What an installation token is requested for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScope {
    /// `pull_requests: write` and `actions: write`: post the review, then re-run the guard
    /// check (ruling R40). Always asked for first.
    ReviewAndRerun,
    /// `pull_requests: write` only: the fallback when the installation hasn't granted Actions
    /// write, so an approval still works (without the re-run).
    ReviewOnly,
}

impl TokenScope {
    /// The `permissions` object of the token request.
    pub fn permissions(self) -> Value {
        match self {
            TokenScope::ReviewAndRerun => {
                serde_json::json!({"pull_requests": "write", "actions": "write"})
            }
            TokenScope::ReviewOnly => serde_json::json!({"pull_requests": "write"}),
        }
    }
}

/// GitHub refused the installation-token exchange. 403 or 422 for a [`TokenScope::ReviewAndRerun`]
/// request means the installation hasn't granted a permission asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRefused {
    pub status: u16,
    pub message: String,
}

impl fmt::Display for TokenRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "GitHub refused the installation-token exchange with HTTP {}: {}",
            self.status, self.message
        )
    }
}

impl std::error::Error for TokenRefused {}

/// `POST /app/installations/{id}/access_tokens` (relative to [`API_BASE`]), asking for a token
/// limited to `repo` and to `scope`'s permissions.
pub fn build_token_request(
    installation_id: u64,
    repo: &str,
    scope: TokenScope,
) -> (Method, String, Value) {
    (
        Method::POST,
        format!("/app/installations/{installation_id}/access_tokens"),
        serde_json::json!({
            "repositories": [repo],
            "permissions": scope.permissions()
        }),
    )
}

/// `DELETE /installation/token`, authenticated with the token being revoked.
pub fn build_revoke_request() -> (Method, String) {
    (Method::DELETE, "/installation/token".to_string())
}

/// The warning printed when the revoke fails. The token is cut out of the error text, in case
/// anything below ever echoes it.
pub(crate) fn revoke_warning(e: &anyhow::Error, token: &str) -> String {
    let mut detail = one_line(&format!("{e:#}"));
    if !token.is_empty() {
        detail = detail.replace(token, "<redacted>");
    }
    format!(
        "warning: couldn't revoke the reviewer app's installation token ({}); it expires on its own within an hour",
        guard::escape_controls(&detail)
    )
}

/// The network seam for installation tokens: [`GitHubAppTokens`] for real, a fake in tests.
pub trait InstallationTokens {
    /// Exchanges the App JWT for an installation token scoped to `repo` and `scope`. A refusal
    /// from GitHub is a [`TokenRefused`].
    fn exchange(
        &self,
        jwt: &str,
        installation_id: u64,
        repo: &str,
        scope: TokenScope,
    ) -> Result<String>;
    /// Revokes an installation token.
    fn revoke(&self, token: &str) -> Result<()>;
}

/// GitHub's REST API.
pub struct GitHubAppTokens;

impl InstallationTokens for GitHubAppTokens {
    fn exchange(
        &self,
        jwt: &str,
        installation_id: u64,
        repo: &str,
        scope: TokenScope,
    ) -> Result<String> {
        exchange_installation_token(jwt, installation_id, repo, scope)
    }

    fn revoke(&self, token: &str) -> Result<()> {
        let (method, path) = build_revoke_request();
        let url = format!("{API_BASE}{path}");
        let resp = http_client()?
            .request(method.clone(), &url)
            .header(AUTHORIZATION, bearer(token)?)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header(USER_AGENT, "lil-dev")
            .send()
            .with_context(|| format!("{method} {url}"))?;
        let status = resp.status().as_u16();
        if status != 204 {
            bail!("GitHub answered HTTP {status} to {method} {url}");
        }
        Ok(())
    }
}

fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("building the HTTP client")
}

/// `Bearer <secret>`, marked sensitive so reqwest never shows it.
fn bearer(secret: &str) -> Result<HeaderValue> {
    let text = Zeroizing::new(format!("Bearer {secret}"));
    let mut value = HeaderValue::from_str(&text)
        .map_err(|_| anyhow!("a credential is not valid in an HTTP header"))?;
    value.set_sensitive(true);
    Ok(value)
}

/// The `token` from a 201 response; anything else is a [`TokenRefused`] carrying GitHub's
/// message.
pub fn parse_token_response(status: u16, body: &Value) -> Result<String> {
    if status != 201 {
        let message = body["message"].as_str().unwrap_or("no message").to_owned();
        return Err(TokenRefused { status, message }.into());
    }
    match body["token"].as_str() {
        Some(t) if !t.is_empty() => Ok(t.to_owned()),
        _ => bail!("GitHub's installation-token response has no token"),
    }
}

/// The real exchange: the JWT is sent as a bearer token and the installation token returned.
/// Neither is ever printed or put in an error.
pub fn exchange_installation_token(
    jwt: &str,
    installation_id: u64,
    repo: &str,
    scope: TokenScope,
) -> Result<String> {
    let (method, path, body) = build_token_request(installation_id, repo, scope);
    let url = format!("{API_BASE}{path}");
    let resp = http_client()?
        .request(method.clone(), &url)
        .header(AUTHORIZATION, bearer(jwt)?)
        .json(&body)
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header(USER_AGENT, "lil-dev")
        .send()
        .with_context(|| format!("{method} {url}"))?;
    let status = resp.status().as_u16();
    let text = Zeroizing::new(
        resp.text()
            .with_context(|| format!("reading the response of {method} {url}"))?,
    );
    // The parsed copy of the token inside `body` is dropped (not zeroed) when this returns.
    let body = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    parse_token_response(status, &body)
}

// ---------------------------------------------------------------------------------------------
// Prompting.
// ---------------------------------------------------------------------------------------------

pub trait Prompt {
    /// Whether a person is at a terminal. The real check is stdin and stderr both being one.
    fn is_interactive(&self) -> bool;
    /// Reads a passphrase without echoing it.
    fn passphrase(&self, label: &str) -> String;
    /// A yes/no question whose default is no.
    fn confirm(&self, question: &str) -> bool;
    /// Reads one line of ordinary (echoed) input, trimmed.
    fn line(&self, question: &str) -> String;
    /// Tells the person something (stderr at a terminal).
    fn say(&self, text: &str) {
        eprintln!("{text}");
    }
}

/// The real terminal: questions go to stderr, answers come from stdin, passphrases are read
/// without echo.
#[derive(Debug)]
pub struct TerminalPrompt;

impl TerminalPrompt {
    fn read_line(question: &str) -> String {
        eprint!("{question} ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        if std::io::stdin().lock().read_line(&mut answer).is_err() {
            return String::new();
        }
        answer.trim().to_string()
    }
}

impl Prompt for TerminalPrompt {
    fn is_interactive(&self) -> bool {
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }

    fn passphrase(&self, label: &str) -> String {
        // An unreadable passphrase is an empty one, which never unlocks the key or passes setup.
        rpassword::prompt_password(label).unwrap_or_default()
    }

    fn confirm(&self, question: &str) -> bool {
        matches!(
            Self::read_line(question).to_ascii_lowercase().as_str(),
            "y" | "yes"
        )
    }

    fn line(&self, question: &str) -> String {
        Self::read_line(question)
    }
}

// ---------------------------------------------------------------------------------------------
// Approving.
// ---------------------------------------------------------------------------------------------

/// Why the PR can't be approved (closed, a draft, or into another branch), if it can't.
fn pr_problem(pr: u64, info: &PrInfo, cfg: &MergeGuardConfig) -> Option<String> {
    if info.state != "open" {
        return Some(format!(
            "PR #{pr} is {}, not open; nothing was approved",
            info.state
        ));
    }
    if info.draft {
        return Some(format!("PR #{pr} is a draft; nothing was approved"));
    }
    if info.base_ref != cfg.base_branch {
        return Some(format!(
            "PR #{pr} targets {}, not {}; nothing was approved",
            info.base_ref, cfg.base_branch
        ));
    }
    None
}

/// The non-empty lines of a summary, each with its control and invisible characters escaped.
fn summary_lines(summary: &str) -> impl Iterator<Item = String> + '_ {
    summary
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(guard::escape_controls)
}

/// What the owner reads before the (y/N) question. Control characters are escaped (R23).
pub(crate) fn summary_display(head: &str, summary: &str) -> String {
    let mut out = format!(
        "Commit {} changes protected files:\n",
        guard::escape_controls(head)
    );
    for line in summary_lines(summary) {
        out.push_str(&format!("  - {line}\n"));
    }
    out
}

/// The approval review's body. Control characters are escaped (R23).
pub(crate) fn review_body(head: &str, summary: &str) -> String {
    let mut body = format!(
        "Approved by the repo owner at a terminal with `lil dev approve`, for commit {} only.\n\nProtected changes approved:\n",
        guard::escape_controls(head)
    );
    for line in summary_lines(summary) {
        body.push_str(&format!("- {line}\n"));
    }
    body
}

/// Posts the reviewer App's `APPROVE` review on PR `pr`, pinned to `expected_head`: the head the
/// guard `summary` was computed for. See the module docs for the order of the steps; every
/// refusal posts nothing. `hello` is Windows Hello when you enrolled it (QA-088): it is tried
/// first, and the passphrase is asked for only when it is cancelled or fails.
#[allow(
    clippy::too_many_arguments,
    reason = "the brief's signature plus expected_head (ruling R20), pause (R40) and hello (Task 28)"
)]
pub fn approve(
    pr: u64,
    cfg: &MergeGuardConfig,
    gh: &dyn GitHubApi,
    prompt: &dyn Prompt,
    key_file: &ReviewerKeyFile,
    hello: Option<&dyn Unlocker>,
    tokens: &dyn InstallationTokens,
    summary: &str,
    now_unix: i64,
    expected_head: &str,
    pause: &dyn Fn(Duration),
) -> Result<Rerun> {
    if !prompt.is_interactive() {
        bail!(NOT_INTERACTIVE);
    }
    if summary.trim().is_empty() {
        bail!(NOTHING_PROTECTED);
    }

    eprintln!("PR #{pr}:");
    eprint!("{}", summary_display(expected_head, summary));
    let question = format!(
        "Approve these protected changes on {}? (y/N)",
        short(expected_head)
    );
    if !prompt.confirm(&question) {
        bail!("you said no; nothing was approved");
    }

    // Unlock before anything touches the network: Windows Hello first when enrolled, then the
    // passphrase. A wrong passphrase stops here, and a damaged file is refused before the owner
    // types the passphrase.
    let ask = |label: &str| Zeroizing::new(prompt.passphrase(label));
    let passphrase = PassphraseUnlocker::new(&key_file.encrypted_pem, &ask);
    let say = |text: &str| prompt.say(text);
    let pem = unlock_reviewer_key(hello, &passphrase, &say).map_err(|e| match e {
        KeyError::WrongPassphrase => anyhow!(WRONG_PASSPHRASE),
        _ => anyhow!("{DAMAGED_KEY}; run `lil dev reviewer-setup` again; nothing was posted"),
    })?;
    approve_with_key(
        pr,
        cfg,
        gh,
        &say,
        key_file,
        &pem,
        tokens,
        summary,
        now_unix,
        expected_head,
        pause,
    )
}

/// What [`approve`] does once the key is unlocked: the App JWT, the live slug, the installation
/// token, the pinned review and the guard re-run. The merge flow calls it after your
/// authentication (P4a-R38). `pem` is only read, to sign the JWT.
#[allow(
    clippy::too_many_arguments,
    reason = "approve's arguments, the unlocked key in place of the prompt"
)]
pub(crate) fn approve_with_key(
    pr: u64,
    cfg: &MergeGuardConfig,
    gh: &dyn GitHubApi,
    say: &dyn Fn(&str),
    key_file: &ReviewerKeyFile,
    pem: &[u8],
    tokens: &dyn InstallationTokens,
    summary: &str,
    now_unix: i64,
    expected_head: &str,
    pause: &dyn Fn(Duration),
) -> Result<Rerun> {
    let jwt = Zeroizing::new(app_jwt(key_file.app_id, pem, now_unix)?);
    // Which App the key belongs to, as GitHub sees it now (D6): only the configured slug may
    // approve. Asked before any installation token exists.
    let live = gh.app_slug(&jwt).context(
        "asking GitHub for the reviewer App's slug (GET /app) failed; nothing was approved",
    )?;
    say(&format!(
        "reviewer App slug: {}",
        guard::escape_controls(&live)
    ));
    if live != cfg.reviewer_app_slug {
        bail!(
            "GitHub says the reviewer key belongs to the App `{}`, but {} names `{}`; nothing was approved",
            guard::escape_controls(&live),
            merge::CONFIG_PATH,
            cfg.reviewer_app_slug
        );
    }
    let exchange = |scope| {
        tokens
            .exchange(&jwt, key_file.installation_id, &cfg.repo, scope)
            .context("exchanging the App JWT for an installation token; nothing was approved")
    };
    // Ask for Actions write too, so the guard check can be re-run (R40). An installation that
    // hasn't granted it yet makes GitHub refuse the whole request (403 or 422); then a
    // review-only token still lets the approval through, without the re-run. From here on the
    // token is revoked when `token` is dropped, on every path, including a panic.
    let (token, refusal) = match exchange(TokenScope::ReviewAndRerun) {
        Ok(t) => (RevokeOnDrop::new(tokens, t), None),
        Err(e) => match e.downcast_ref::<TokenRefused>() {
            Some(r) if matches!(r.status, 403 | 422) => {
                let refusal = r.to_string();
                let t = exchange(TokenScope::ReviewOnly)?;
                (RevokeOnDrop::new(tokens, t), Some(refusal))
            }
            _ => return Err(e),
        },
    };
    drop(jwt);

    let outcome =
        post_if_unchanged(pr, cfg, gh, summary, expected_head, token.secret()).map(|()| {
            match &refusal {
                None => rerun_guard_check(pr, cfg, gh, expected_head, token.secret(), pause),
                Some(refusal) => Rerun::Warning {
                    text: rerun_warning(
                        &format!("{refusal}; {NO_ACTIONS_PERMISSION}"),
                        &checks_url(cfg, pr),
                        token.secret(),
                    ),
                    requested: Vec::new(),
                },
            }
        });
    // Revoked now, after the post and the re-runs. Best effort: the token is useless once the
    // review is posted.
    drop(token);
    outcome
}

/// An installation token that is revoked when this is dropped: after a normal return, an
/// early `?` and a panic alike. A failed revoke is only a warning ([`revoke_warning`]). A
/// process that is killed (Ctrl+C) doesn't unwind, so then the token lives until it expires,
/// within an hour.
pub(crate) struct RevokeOnDrop<'a> {
    tokens: &'a dyn InstallationTokens,
    token: Zeroizing<String>,
}

impl<'a> RevokeOnDrop<'a> {
    pub(crate) fn new(tokens: &'a dyn InstallationTokens, token: String) -> Self {
        Self {
            tokens,
            token: Zeroizing::new(token),
        }
    }

    pub(crate) fn secret(&self) -> &str {
        &self.token
    }
}

impl Drop for RevokeOnDrop<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.tokens.revoke(&self.token) {
            eprintln!("{}", revoke_warning(&e, &self.token));
        }
    }
}

/// What `approve` did about the guard check after posting the review (ruling R40).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rerun {
    /// GitHub accepted re-runs of these guard runs (oldest first).
    Requested { run_ids: Vec<u64> },
    /// The newest guard run for the head already passed, and no older one needed a re-run.
    AlreadyPassed { run_id: u64 },
    /// The approval is posted, but the check may stay red. `text` says why and where to
    /// re-run it by hand, and never holds a token; `requested` lists the re-runs GitHub did
    /// accept before that.
    Warning { text: String, requested: Vec<u64> },
}

/// The guard's workflow file name, as the runs listing takes it.
pub const GUARD_WORKFLOW: &str = "ci-guard.yml";
/// The guard's workflow path, as a run reports it.
pub const GUARD_WORKFLOW_PATH: &str = ".github/workflows/ci-guard.yml";
/// The one event that starts the guard workflow.
pub const GUARD_EVENT: &str = "pull_request";
/// How long `approve` waits for unfinished guard runs before re-running what it can.
pub const RERUN_WAIT: Duration = Duration::from_secs(60);
/// How often it looks again while it waits.
pub const RERUN_POLL: Duration = Duration::from_secs(5);

/// The likely cause when GitHub refuses a token with Actions write.
const NO_ACTIONS_PERMISSION: &str = "the reviewer app's installation has probably not granted Actions: Read and write; see docs/dev/merge-guard.md, section 2.5";

/// The guard runs that belong to PR `pr` at `head`, oldest first: `pull_request` runs of
/// [`GUARD_WORKFLOW_PATH`] for that commit, from this repository (not a fork), whose
/// `pull_requests` list this PR. Everything else is ignored, even if GitHub returned it.
pub(crate) fn guard_runs_for(
    runs: &[WorkflowRun],
    cfg: &MergeGuardConfig,
    pr: u64,
    head: &str,
) -> Vec<WorkflowRun> {
    let repo = format!("{}/{}", cfg.owner, cfg.repo);
    let mut mine: Vec<WorkflowRun> = runs
        .iter()
        .filter(|r| {
            r.event == GUARD_EVENT
                && r.head_sha == head
                && r.path == GUARD_WORKFLOW_PATH
                && r.head_repo.eq_ignore_ascii_case(&repo)
                && r.pr_numbers.contains(&pr)
        })
        .cloned()
        .collect();
    mine.sort_by_key(|r| r.id);
    mine
}

fn finished(run: &WorkflowRun) -> bool {
    run.status == "completed"
}

/// A finished run that isn't green. `action_required` runs wait for a maintainer's approval to
/// run at all and are never re-run.
fn needs_rerun(run: &WorkflowRun) -> bool {
    finished(run)
        && !matches!(
            run.conclusion.as_deref(),
            Some("success") | Some("action_required")
        )
}

/// "Approval posted, but ... (<reason>). Re-run it in the browser: <url>", on one line, with
/// control characters escaped and the token cut out.
pub(crate) fn rerun_warning(reason: &dyn fmt::Display, url: &str, token: &str) -> String {
    let mut reason = one_line(&format!("{reason:#}"));
    if !token.is_empty() {
        reason = reason.replace(token, "<redacted>");
    }
    format!(
        "Approval posted, but the guard check couldn't be re-run automatically ({}). Re-run it in the browser: {}",
        guard::escape_controls(&reason),
        guard::escape_controls(url)
    )
}

/// The PR's checks page.
fn checks_url(cfg: &MergeGuardConfig, pr: u64) -> String {
    format!(
        "https://github.com/{}/{}/pull/{pr}/checks",
        cfg.owner, cfg.repo
    )
}

/// A workflow run's page. Built here rather than taken from GitHub's `html_url`.
fn run_url(cfg: &MergeGuardConfig, run_id: u64) -> String {
    format!(
        "https://github.com/{}/{}/actions/runs/{run_id}",
        cfg.owner, cfg.repo
    )
}

/// Re-runs every finished, non-green guard run of PR `pr` at `head` (ruling R40). Each
/// workflow run is its own check suite, and the ruleset refuses the merge while any of them is
/// red, so all of them are re-run, not just the newest. A re-run keeps the run's original event
/// payload, and the guard reads the reviews live, so it now sees the approval. Never fails:
/// whatever goes wrong becomes a [`Rerun::Warning`].
///
/// GitHub re-runs only finished runs. While any of the PR's runs is unfinished, they are
/// looked at again every [`RERUN_POLL`] for up to [`RERUN_WAIT`]; then the finished ones are
/// re-run and the warning gives the unfinished run's address instead of cancelling it (it will
/// most likely read the approval itself). The PR is re-read before each look: if its head moved,
/// the approval no longer counts, so nothing is re-run.
fn rerun_guard_check(
    pr: u64,
    cfg: &MergeGuardConfig,
    gh: &dyn GitHubApi,
    head: &str,
    token: &str,
    pause: &dyn Fn(Duration),
) -> Rerun {
    let checks = checks_url(cfg, pr);
    let warn = |reason: &dyn fmt::Display, url: &str, requested: Vec<u64>| Rerun::Warning {
        text: rerun_warning(reason, url, token),
        requested,
    };
    let mut waited = Duration::ZERO;
    let runs = loop {
        match gh.pr(pr) {
            Ok(info) if info.head_sha == head => {}
            Ok(_) => {
                return Rerun::Warning {
                    text: format!(
                        "Approval posted, but the PR head moved after it, so it no longer counts and the guard check was not re-run. Run `lil dev approve {pr}` again for the new head."
                    ),
                    requested: Vec::new(),
                };
            }
            Err(e) => {
                return warn(
                    &format!("re-reading PR #{pr} failed: {e:#}"),
                    &checks,
                    Vec::new(),
                );
            }
        }
        let runs = match gh.workflow_runs_as_app(GUARD_WORKFLOW, GUARD_EVENT, head, token) {
            Ok(runs) => guard_runs_for(&runs, cfg, pr, head),
            Err(e) => {
                return warn(
                    &format!("listing the guard's runs failed: {e:#}"),
                    &checks,
                    Vec::new(),
                );
            }
        };
        if runs.is_empty() {
            return warn(
                &format!(
                    "no `{GUARD_EVENT}` run of {GUARD_WORKFLOW} was found for PR #{pr} at {}",
                    short(head)
                ),
                &checks,
                Vec::new(),
            );
        }
        if runs.iter().all(finished) || waited >= RERUN_WAIT {
            break runs;
        }
        if waited.is_zero() {
            eprintln!(
                "The guard check for {} is still running; waiting up to {} s for it to finish so it can be re-run if needed.",
                short(head),
                RERUN_WAIT.as_secs()
            );
        }
        pause(RERUN_POLL);
        waited += RERUN_POLL;
    };

    let mut requested = Vec::new();
    for run in runs.iter().filter(|r| needs_rerun(r)) {
        if let Err(e) = gh.rerun_as_app(run.id, token) {
            return warn(
                &format!("GitHub refused to re-run guard run {}: {e:#}", run.id),
                &run_url(cfg, run.id),
                requested,
            );
        }
        requested.push(run.id);
    }
    if let Some(running) = runs.iter().rev().find(|r| !finished(r)) {
        return Rerun::Warning {
            text: format!(
                "Approval posted, but the guard check is still running; it will most likely read your approval when it finishes. If it ends red, re-run it: {}",
                guard::escape_controls(&run_url(cfg, running.id))
            ),
            requested,
        };
    }
    if !requested.is_empty() {
        return Rerun::Requested { run_ids: requested };
    }
    // Nothing needed a re-run: the newest run passed, or only `action_required` runs are left.
    let newest = runs.last().expect("checked non-empty above");
    match newest.conclusion.as_deref() {
        Some("success") => Rerun::AlreadyPassed { run_id: newest.id },
        other => warn(
            &format!(
                "guard run {} ended as {} and isn't re-run",
                newest.id,
                other.unwrap_or("none")
            ),
            &run_url(cfg, newest.id),
            Vec::new(),
        ),
    }
}

/// Re-reads the PR immediately before posting and posts only when it is still open, not a
/// draft, into the base branch and at `expected_head`.
fn post_if_unchanged(
    pr: u64,
    cfg: &MergeGuardConfig,
    gh: &dyn GitHubApi,
    summary: &str,
    expected_head: &str,
    token: &str,
) -> Result<()> {
    let info = gh
        .pr(pr)
        .with_context(|| format!("re-reading PR #{pr} failed; nothing was approved"))?;
    if let Some(problem) = pr_problem(pr, &info, cfg) {
        bail!(problem);
    }
    if info.head_sha != expected_head {
        bail!(HEAD_MOVED);
    }
    gh.post_review_as_app(
        pr,
        expected_head,
        &review_body(expected_head, summary),
        token,
    )
    .with_context(|| {
        format!(
            "posting the approval on PR #{pr} failed; check the PR on GitHub before trying again"
        )
    })
}

// ---------------------------------------------------------------------------------------------
// Setup.
// ---------------------------------------------------------------------------------------------

fn ask_id(prompt: &dyn Prompt, question: &str, what: &str) -> Result<u64> {
    let answer = prompt.line(question);
    match answer.trim().parse::<u64>() {
        Ok(n) if n > 0 => Ok(n),
        _ => bail!("{what} must be a positive whole number, not {answer:?}; nothing was written"),
    }
}

/// `lil dev reviewer-setup` at [`WORK_FACTOR`].
pub fn reviewer_setup(prompt: &dyn Prompt, key_path: &Path) -> Result<()> {
    setup_with_work_factor(prompt, key_path, WORK_FACTOR)
}

/// Asks for the App id, the installation id and the `.pem` path; checks the `.pem` is an RSA
/// private key; asks for the passphrase twice; encrypts the key and writes [`ReviewerKeyFile`]
/// to `key_path`. The `.pem` is only read.
pub fn setup_with_work_factor(prompt: &dyn Prompt, key_path: &Path, log_n: u8) -> Result<()> {
    if !prompt.is_interactive() {
        bail!(SETUP_NOT_INTERACTIVE);
    }
    if key_path.exists()
        && !prompt.confirm(&format!(
            "A reviewer key already exists at {}. Overwrite it? (y/N)",
            key_path.display()
        ))
    {
        bail!("kept the existing reviewer key; nothing was written");
    }

    let app_id = ask_id(prompt, "GitHub App id:", "The App id")?;
    let installation_id = ask_id(prompt, "Installation id:", "The installation id")?;
    let pem_answer = prompt.line("Path to the downloaded .pem file:");
    let pem_path = PathBuf::from(pem_answer.trim().trim_matches('"'));
    let pem = Zeroizing::new(
        std::fs::read(&pem_path)
            .with_context(|| format!("reading {}; nothing was written", pem_path.display()))?,
    );
    // Signing a throwaway JWT proves the file is a usable RSA private key.
    if app_jwt(app_id, &pem, unix_now()).is_err() {
        bail!(
            "{} is not an RSA private key in PEM form; nothing was written",
            pem_path.display()
        );
    }

    let first = Zeroizing::new(prompt.passphrase(&format!(
        "New passphrase (at least {MIN_PASSPHRASE_CHARS} characters): "
    )));
    let second = Zeroizing::new(prompt.passphrase("Same passphrase again: "));
    if *first != *second {
        bail!("the passphrases don't match; nothing was written");
    }
    if first.chars().count() < MIN_PASSPHRASE_CHARS {
        bail!(
            "the passphrase must be at least {MIN_PASSPHRASE_CHARS} characters; nothing was written"
        );
    }

    let key_file = ReviewerKeyFile {
        app_id,
        installation_id,
        encrypted_pem: encrypt_key_with_work_factor(&pem, &first, log_n)?,
    };
    key_file.save(key_path)?;
    eprintln!(
        "Saved the encrypted reviewer key to {}.",
        key_path.display()
    );
    eprintln!(
        "Now delete the plaintext key yourself: {}. lil never deletes or changes it.",
        pem_path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// CLI wiring.
// ---------------------------------------------------------------------------------------------

/// What `lil dev approve` shows and pins: the fresh committed settings, the GitHub client built
/// from them, the PR's head commit and the guard's blocker lines for that commit.
pub(crate) struct Prepared<'g> {
    pub cfg: MergeGuardConfig,
    pub gh: Box<dyn GitHubApi + 'g>,
    pub head: String,
    pub summary: String,
}

/// Fetches `origin/main`, re-reads the committed settings (which must name the repo `origin`
/// was checked against), reads the PR, fetches its head and judges `origin/main...<head>` with
/// the guard. Refuses closed, draft and wrong-base PRs and PRs that change nothing protected.
pub(crate) fn prepare_approval<'g>(
    repo: &Path,
    pr: u64,
    checked: &MergeGuardConfig,
    github: &dyn Fn(&MergeGuardConfig) -> Result<Box<dyn GitHubApi + 'g>>,
) -> Result<Prepared<'g>> {
    let base_branch = &checked.base_branch;
    fetch(
        repo,
        &[&format!(
            "+refs/heads/{base_branch}:{}",
            merge::remote_base_ref(base_branch)
        )],
    )
    .with_context(|| format!("fetching origin/{base_branch} failed"))?;
    let cfg = committed_config(repo).map_err(|r| anyhow!(explain(&[r])))?;
    if !cfg.owner.eq_ignore_ascii_case(&checked.owner)
        || !cfg.repo.eq_ignore_ascii_case(&checked.repo)
    {
        bail!(
            "the freshly fetched {} names github.com/{}/{}, but origin was checked against github.com/{}/{}",
            merge::CONFIG_PATH,
            cfg.owner,
            cfg.repo,
            checked.owner,
            checked.repo
        );
    }
    let gh = github(&cfg).context("no GitHub access")?;
    let info = gh
        .pr(pr)
        .with_context(|| format!("reading PR #{pr} from GitHub failed"))?;
    if let Some(problem) = pr_problem(pr, &info, &cfg) {
        bail!(problem);
    }
    let head = info.head_sha;
    if !is_commit_sha(&head) {
        bail!("GitHub reported a head commit for PR #{pr} that is not a commit SHA: {head:?}");
    }
    fetch(repo, &[&format!("refs/pull/{pr}/head")])
        .with_context(|| format!("fetching PR #{pr}'s head failed"))?;
    // The full ref: a local branch named `origin/main` must not stand in for it.
    let base = merge::remote_base_ref(&cfg.base_branch);
    let verdict = guard::verdict(repo, &base, &head).with_context(|| {
        format!(
            "the test-diff guard could not judge {} against {base}",
            short(&head)
        )
    })?;
    if verdict.needs_approval.is_empty() {
        bail!(NOTHING_PROTECTED);
    }
    Ok(Prepared {
        summary: blocker_lines(&verdict).join("\n"),
        cfg,
        gh,
        head,
    })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// What `lil dev approve` and the merge flow print about the guard re-run.
fn print_rerun(rerun: Rerun) {
    let ids = |ids: &[u64]| {
        ids.iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    match rerun {
        Rerun::Requested { run_ids } => println!(
            "Asked GitHub to re-run protected-test-check (workflow runs {}); it reads the approval and turns green when it finishes.",
            ids(&run_ids)
        ),
        Rerun::AlreadyPassed { run_id } => println!(
            "protected-test-check already passed for this commit (workflow run {run_id}); nothing to re-run."
        ),
        Rerun::Warning { text, requested } => {
            if !requested.is_empty() {
                println!(
                    "Asked GitHub to re-run protected-test-check (workflow runs {}).",
                    ids(&requested)
                );
            }
            eprintln!("{text}");
        }
    }
}

/// Prints `text` to stderr, each line with its control characters escaped (R23).
fn say_escaped(text: &str) {
    for line in text.lines() {
        eprintln!("{}", guard::escape_controls(line));
    }
}

/// Runs `f` with your authentication for `what` (QA-036, P4a-R38): the reviewer key loaded from
/// `app-key.age`, and an [`Authenticator`] that shows the summary and the audit, then asks
/// Windows Hello when you enrolled it, and the passphrase at the terminal otherwise or when
/// Hello is cancelled or fails. Local only: a missing key, or neither Hello nor a terminal, is
/// refused before any network call.
pub(crate) fn with_owner_authenticator<T>(
    what: &str,
    f: impl FnOnce(&ReviewerKeyFile, &dyn Authenticator) -> T,
) -> Result<T> {
    let profile = user_profile()?;
    let (_, key_file) = load_key_in(&profile)
        .context("every approval and merge needs your authentication with the reviewer key")?;
    let prompt = TerminalPrompt;
    let platform = WindowsHello;
    let message = format!("LILITH: {what}");
    let hello = enrolled_hello(&profile, &platform, &message, &say_escaped);
    if hello.is_none() && !prompt.is_interactive() {
        bail!(
            "Windows Hello isn't enrolled and this isn't a real terminal, so you can't authenticate to {what}; nothing was approved or merged"
        );
    }
    let ask = |label: &str| Zeroizing::new(prompt.passphrase(label));
    let label = format!("Reviewer key passphrase (to {what}): ");
    let at_terminal = PassphraseUnlocker::new(&key_file.encrypted_pem, &ask).labelled(&label);
    // Without a terminal nobody can type the passphrase, so a failed Hello ends there; the
    // console is never read (rpassword opens it directly, where no one may be looking).
    let passphrase: &dyn Unlocker = if prompt.is_interactive() {
        &at_terminal
    } else {
        &NoTerminal
    };
    let auth = KeyAuthenticator::new(
        &say_escaped,
        hello.as_ref().map(|h| h as &dyn Unlocker),
        passphrase,
        &say_escaped,
    );
    Ok(f(&key_file, &auth))
}

/// The passphrase path when there is no terminal: it unlocks nothing.
struct NoTerminal;

impl Unlocker for NoTerminal {
    fn unlock(&self) -> std::result::Result<Zeroizing<Vec<u8>>, KeyError> {
        say_escaped("There is no terminal here to type the passphrase into.");
        Err(KeyError::Unavailable)
    }
}

/// `lil dev approve <pr>`: exit 0 when the approval was posted, 1 otherwise.
pub fn run_approve(pr: u64) -> i32 {
    let refuse = |message: &str| {
        eprintln!(
            "lil dev approve refused PR #{pr}: {}",
            guard::escape_controls(&one_line(message))
        );
        1
    };
    let prompt = TerminalPrompt;
    // Local checks first, before any network call.
    if !prompt.is_interactive() {
        return refuse(NOT_INTERACTIVE);
    }
    let found = user_profile().and_then(|profile| Ok((load_key_in(&profile)?, profile)));
    let (key_file, profile) = match found {
        Ok(((path, key), profile)) => {
            println!("Reviewer key: {}", path.display());
            (key, profile)
        }
        Err(e) => return refuse(&format!("{e:#}")),
    };
    // Windows Hello, when you enrolled it, is tried first (QA-088).
    let platform = WindowsHello;
    let message = format!("LILITH: approve PR #{pr}'s protected changes");
    let hello = enrolled_hello(&profile, &platform, &message, &say_escaped);
    let repo = match super::root::repo_root() {
        Ok(r) => r,
        Err(e) => return refuse(&e),
    };
    let checked = match committed_config(&repo) {
        Ok(c) => c,
        Err(r) => return refuse(&explain(&[r])),
    };
    if let Err(r) = check_origin(&repo, &checked) {
        return refuse(&explain(&[r]));
    }
    let github = |cfg: &MergeGuardConfig| -> Result<Box<dyn GitHubApi>> {
        Ok(Box::new(super::github::RestClient::new(
            &cfg.owner,
            &cfg.repo,
            &super::github::github_token()?,
        )?))
    };
    let prepared = match prepare_approval(&repo, pr, &checked, &github) {
        Ok(p) => p,
        Err(e) => return refuse(&format!("{e:#}")),
    };
    match approve(
        pr,
        &prepared.cfg,
        prepared.gh.as_ref(),
        &prompt,
        &key_file,
        hello.as_ref().map(|h| h as &dyn Unlocker),
        &GitHubAppTokens,
        &prepared.summary,
        unix_now(),
        &prepared.head,
        &std::thread::sleep,
    ) {
        Ok(rerun) => {
            println!(
                "Approved PR #{pr} at {} as the reviewer App.",
                short(&prepared.head)
            );
            print_rerun(rerun);
            0
        }
        Err(e) => refuse(&format!("{e:#}")),
    }
}

// ---------------------------------------------------------------------------------------------
// `lil dev merge-flow` (P4a-R38): the real steps behind `merge_flow::MergeFlowSteps`.
// ---------------------------------------------------------------------------------------------

/// The longest summary or audit file the flow reads.
const MAX_FLOW_TEXT: u64 = 64 * 1024;
/// How long the flow waits for CI on the head (after posting the approval, the guard re-runs).
const CHECKS_WAIT: Duration = Duration::from_secs(20 * 60);
/// How often it looks.
const CHECKS_POLL: Duration = Duration::from_secs(15);
/// The check that the approval's guard re-run turns green (when protected files changed); a
/// failure of any other one stops the wait at once.
const GUARD_CHECK: &str = "protected-test-check";

/// A summary or audit file: UTF-8, not empty, at most [`MAX_FLOW_TEXT`] bytes.
fn read_flow_text(path: &Path, what: &str) -> Result<String> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("reading the {what} file {}", path.display()))?
        .len();
    if size > MAX_FLOW_TEXT {
        bail!(
            "the {what} file {} is {size} bytes; at most {MAX_FLOW_TEXT} are read",
            path.display()
        );
    }
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "reading the {what} file {} (it must be UTF-8)",
            path.display()
        )
    })?;
    if text.trim().is_empty() {
        bail!("the {what} file {} is empty", path.display());
    }
    Ok(text)
}

fn check_state_name(state: &CheckState) -> String {
    match state {
        CheckState::Success => "success".into(),
        CheckState::Pending => "pending".into(),
        CheckState::Missing => "missing".into(),
        CheckState::Failure(c) => c.clone(),
    }
}

type GitHubFor<'g> = dyn Fn(&MergeGuardConfig) -> Result<Box<dyn GitHubApi + 'g>> + 'g;

/// The flow's git and GitHub steps.
struct FlowSteps<'a, 'g> {
    repo: &'a Path,
    pr: u64,
    checked: &'a MergeGuardConfig,
    hooks: Option<String>,
    key_file: &'a ReviewerKeyFile,
    github: &'a GitHubFor<'g>,
    /// The fresh settings and the client `facts` built, for the later steps.
    state: std::cell::RefCell<Option<(MergeGuardConfig, Box<dyn GitHubApi + 'g>)>>,
}

impl FlowSteps<'_, '_> {
    fn with_gh<T>(
        &self,
        f: impl FnOnce(&MergeGuardConfig, &dyn GitHubApi) -> Result<T>,
    ) -> Result<T> {
        let state = self.state.borrow();
        let (cfg, gh) = state
            .as_ref()
            .ok_or_else(|| anyhow!("the PR wasn't read first"))?;
        f(cfg, gh.as_ref())
    }

    /// `git diff --name-only <base>...<head>`, escaped for display.
    fn files(&self, base: &str, head: &str) -> Result<Vec<String>> {
        let out = super::root::git_command()
            .arg("-C")
            .arg(self.repo)
            .args(["diff", "--name-only", "-z", &format!("{base}...{head}")])
            .output()
            .context("could not run git diff")?;
        if !out.status.success() {
            bail!(
                "git diff failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .split_terminator('\0')
            .map(guard::escape_controls)
            .collect())
    }
}

impl MergeFlowSteps for FlowSteps<'_, '_> {
    fn facts(&self) -> Result<Facts> {
        let base_branch = &self.checked.base_branch;
        fetch(
            self.repo,
            &[&format!(
                "+refs/heads/{base_branch}:{}",
                merge::remote_base_ref(base_branch)
            )],
        )
        .with_context(|| format!("fetching origin/{base_branch} failed"))?;
        let cfg = committed_config(self.repo).map_err(|r| anyhow!(explain(&[r])))?;
        if !cfg.owner.eq_ignore_ascii_case(&self.checked.owner)
            || !cfg.repo.eq_ignore_ascii_case(&self.checked.repo)
        {
            bail!(
                "the freshly fetched {} names github.com/{}/{}, but origin was checked against github.com/{}/{}",
                merge::CONFIG_PATH,
                cfg.owner,
                cfg.repo,
                self.checked.owner,
                self.checked.repo
            );
        }
        let gh = (self.github)(&cfg).context("no GitHub access")?;
        let info = gh
            .pr(self.pr)
            .with_context(|| format!("reading PR #{} from GitHub failed", self.pr))?;
        if let Some(problem) = pr_problem(self.pr, &info, &cfg) {
            bail!(problem.replace("nothing was approved", "nothing was approved or merged"));
        }
        let head = info.head_sha;
        if !is_commit_sha(&head) {
            bail!(
                "GitHub reported a head commit for PR #{} that is not a commit SHA: {head:?}",
                self.pr
            );
        }
        fetch(self.repo, &[&format!("refs/pull/{}/head", self.pr)])
            .with_context(|| format!("fetching PR #{}'s head failed", self.pr))?;
        // The full ref: a local branch named `origin/main` must not stand in for it.
        let base = merge::remote_base_ref(&cfg.base_branch);
        let verdict = guard::verdict(self.repo, &base, &head).with_context(|| {
            format!(
                "the test-diff guard could not judge {} against {base}",
                short(&head)
            )
        })?;
        let protected = blocker_lines(&verdict);
        let already_approved = !protected.is_empty()
            && merge::app_approved(&gh.reviews(self.pr)?, &cfg.reviewer_login(), &head);
        let mut checks = Vec::new();
        for name in &cfg.required_checks {
            let state = gh
                .check(&head, name)
                .with_context(|| format!("reading CI check {name} on {} failed", short(&head)))?;
            checks.push((name.clone(), check_state_name(&state)));
        }
        let files = self.files(&base, &head)?;
        *self.state.borrow_mut() = Some((cfg, gh));
        Ok(Facts {
            pr: self.pr,
            head,
            files,
            protected,
            checks,
            already_approved,
        })
    }

    fn post_approval(&self, pem: &[u8], facts: &Facts) -> Result<()> {
        let rerun = self.with_gh(|cfg, gh| {
            approve_with_key(
                self.pr,
                cfg,
                gh,
                &say_escaped,
                self.key_file,
                pem,
                &GitHubAppTokens,
                &facts.protected.join("\n"),
                unix_now(),
                &facts.head,
                &std::thread::sleep,
            )
        })?;
        println!(
            "Approved PR #{} at {} as the reviewer App.",
            self.pr,
            short(&facts.head)
        );
        print_rerun(rerun);
        Ok(())
    }

    fn wait_for_checks(&self, facts: &Facts) -> Result<()> {
        self.with_gh(|cfg, gh| {
            let mut waited = Duration::ZERO;
            loop {
                let mut waiting = Vec::new();
                for name in &cfg.required_checks {
                    match gh.check(&facts.head, name)? {
                        CheckState::Success => {}
                        CheckState::Failure(c) if name != GUARD_CHECK || facts.protected.is_empty() => bail!(
                            "CI check {name} finished as {c} on {}; nothing was merged",
                            short(&facts.head)
                        ),
                        other => waiting.push(format!("{name}: {}", check_state_name(&other))),
                    }
                }
                if waiting.is_empty() {
                    return Ok(());
                }
                if waited >= CHECKS_WAIT {
                    bail!(
                        "CI isn't green on {} after {} minutes ({}); nothing was merged. Run the merge flow again once it is",
                        short(&facts.head),
                        CHECKS_WAIT.as_secs() / 60,
                        waiting.join(", ")
                    );
                }
                if waited.as_secs().is_multiple_of(60) {
                    eprintln!(
                        "Waiting for CI on {}: {}",
                        short(&facts.head),
                        waiting.join(", ")
                    );
                }
                std::thread::sleep(CHECKS_POLL);
                waited += CHECKS_POLL;
            }
        })
    }

    fn merge(&self, gate: &dyn MergeGate) -> Result<String> {
        merge::merge_in_repo(
            self.repo,
            self.pr,
            self.checked,
            self.hooks.clone(),
            self.github,
            gate,
        )
        .map_err(|refusals| anyhow!(explain(&refusals)))
    }
}

/// `lil dev merge-flow <pr> --summary <file> --audit <file>` (P4a-R38): exit 0 when merged, 1
/// otherwise.
pub fn run_merge_flow(pr: u64, summary_path: &Path, audit_path: &Path) -> i32 {
    let refuse = |message: &str| {
        eprintln!(
            "lil dev merge-flow refused PR #{pr}: {}",
            guard::escape_controls(&one_line(message))
        );
        1
    };
    // Local checks first, before any network call.
    let texts = read_flow_text(summary_path, "summary")
        .and_then(|summary| Ok((summary, read_flow_text(audit_path, "audit")?)));
    let (summary, audit) = match texts {
        Ok(t) => t,
        Err(e) => return refuse(&format!("{e:#}")),
    };
    let repo = match super::root::repo_root() {
        Ok(r) => r,
        Err(e) => return refuse(&e),
    };
    let (checked, hooks) = match merge::preflight(&repo) {
        Ok(x) => x,
        Err(refusals) => return refuse(&explain(&refusals)),
    };
    if let Err(r) = check_origin(&repo, &checked) {
        return refuse(&explain(&[r]));
    }
    let github = |cfg: &MergeGuardConfig| -> Result<Box<dyn GitHubApi>> {
        Ok(Box::new(super::github::RestClient::new(
            &cfg.owner,
            &cfg.repo,
            &super::github::github_token()?,
        )?))
    };
    let outcome = with_owner_authenticator(&format!("merge PR #{pr}"), |key_file, auth| {
        let steps = FlowSteps {
            repo: &repo,
            pr,
            checked: &checked,
            hooks,
            key_file,
            github: &github,
            state: std::cell::RefCell::new(None),
        };
        merge_flow(&summary, &audit, &steps, auth)
    });
    match outcome {
        Ok(Ok(merge_sha)) => {
            println!("Merged PR #{pr} (merge commit {}).", short(&merge_sha));
            0
        }
        Ok(Err(e)) | Err(e) => refuse(&format!("{e:#}")),
    }
}

/// `lil dev reviewer-setup`: exit 0 when the key file was written, 1 otherwise.
pub fn run_reviewer_setup() -> i32 {
    let result = setup_key_path().and_then(|p| reviewer_setup(&TerminalPrompt, &p));
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("lil dev reviewer-setup: {}", one_line(&format!("{e:#}")));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev::github::GitHubApi;
    use crate::dev::github_fake::FakeGitHub;
    use crate::dev::merge::{CONFIG_PATH, app_approved};
    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::process::Command;
    use std::sync::OnceLock;

    const PR: u64 = 42;
    const HEAD: &str = "1234567890abcdef1234567890abcdef12345678";
    const NEW: &str = "0000000000000000000000000000000000000abc";
    const PASS: &str = "correct horse battery";
    const SUMMARY: &str = "the protected file `crates/lil-spec-tests/tests/smoke.rs` was modified.";
    const NOW: i64 = 1_800_000_000;
    /// Cheap scrypt for tests; production uses [`WORK_FACTOR`].
    const TEST_WORK_FACTOR: u8 = 10;
    const COMMITTED: &str = include_str!("../../../../dev/merge-guard.toml");

    // ---- test key material (generated once per test process) ---------------------------------

    struct TestKey {
        pkcs8_pem: Vec<u8>,
        pkcs1_pem: Vec<u8>,
        public_pkcs1_der: Vec<u8>,
    }

    fn pem_wrap(label: &str, der: &[u8]) -> Vec<u8> {
        let b64 = STANDARD.encode(der);
        let mut out = format!("-----BEGIN {label}-----\n");
        for chunk in b64.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(chunk).unwrap());
            out.push('\n');
        }
        out.push_str(&format!("-----END {label}-----\n"));
        out.into_bytes()
    }

    /// Reads one DER TLV at `at`: (tag, content start, content end).
    fn tlv(der: &[u8], at: usize) -> (u8, usize, usize) {
        let tag = der[at];
        let first = der[at + 1] as usize;
        let (len, start) = if first < 0x80 {
            (first, at + 2)
        } else {
            let n = first & 0x7f;
            let len = der[at + 2..at + 2 + n]
                .iter()
                .fold(0usize, |acc, b| (acc << 8) | *b as usize);
            (len, at + 2 + n)
        };
        (tag, start, start + len)
    }

    /// PKCS#8 PrivateKeyInfo -> the PKCS#1 RSAPrivateKey inside its OCTET STRING.
    fn pkcs8_to_pkcs1(der: &[u8]) -> Vec<u8> {
        let (_, inner, _) = tlv(der, 0); // SEQUENCE
        let (_, _, after_version) = tlv(der, inner); // INTEGER version
        let (_, _, after_alg) = tlv(der, after_version); // AlgorithmIdentifier
        let (tag, start, end) = tlv(der, after_alg);
        assert_eq!(tag, 0x04, "expected the OCTET STRING holding the RSA key");
        der[start..end].to_vec()
    }

    fn test_key() -> &'static TestKey {
        static KEY: OnceLock<TestKey> = OnceLock::new();
        KEY.get_or_init(|| {
            use aws_lc_rs::encoding::AsDer;
            use aws_lc_rs::rsa::{KeyPair, KeySize};
            use aws_lc_rs::signature::KeyPair as _;
            let kp = KeyPair::generate(KeySize::Rsa2048).expect("generate a test RSA key");
            let pkcs8 = kp.as_der().expect("PKCS#8 DER");
            let pkcs8: &[u8] = pkcs8.as_ref();
            TestKey {
                pkcs8_pem: pem_wrap("PRIVATE KEY", pkcs8),
                pkcs1_pem: pem_wrap("RSA PRIVATE KEY", &pkcs8_to_pkcs1(pkcs8)),
                public_pkcs1_der: kp.public_key().as_ref().to_vec(),
            }
        })
    }

    fn test_pem() -> &'static [u8] {
        &test_key().pkcs8_pem
    }

    fn key_file() -> ReviewerKeyFile {
        ReviewerKeyFile {
            app_id: 123,
            installation_id: 456,
            encrypted_pem: encrypt_key_with_work_factor(test_pem(), PASS, TEST_WORK_FACTOR)
                .unwrap(),
        }
    }

    // ---- a scripted prompt ---------------------------------------------------------------------

    struct FakePrompt {
        interactive: bool,
        lines: RefCell<VecDeque<String>>,
        passphrases: RefCell<VecDeque<String>>,
        confirms: RefCell<VecDeque<bool>>,
        asked: RefCell<Vec<String>>,
        said: RefCell<Vec<String>>,
    }

    impl FakePrompt {
        fn new() -> Self {
            Self {
                interactive: true,
                lines: RefCell::default(),
                passphrases: RefCell::default(),
                confirms: RefCell::default(),
                asked: RefCell::default(),
                said: RefCell::default(),
            }
        }
        fn not_a_terminal() -> Self {
            Self {
                interactive: false,
                ..Self::new()
            }
        }
        fn lines(self, l: &[&str]) -> Self {
            self.lines
                .borrow_mut()
                .extend(l.iter().map(|s| s.to_string()));
            self
        }
        fn passphrases(self, p: &[&str]) -> Self {
            self.passphrases
                .borrow_mut()
                .extend(p.iter().map(|s| s.to_string()));
            self
        }
        fn confirms(self, c: &[bool]) -> Self {
            self.confirms.borrow_mut().extend(c.iter().copied());
            self
        }
        fn asked(&self) -> Vec<String> {
            self.asked.borrow().clone()
        }
        fn said(&self) -> Vec<String> {
            self.said.borrow().clone()
        }
    }

    impl Prompt for FakePrompt {
        fn is_interactive(&self) -> bool {
            self.interactive
        }
        fn passphrase(&self, label: &str) -> String {
            self.asked.borrow_mut().push(label.to_string());
            self.passphrases
                .borrow_mut()
                .pop_front()
                .expect("unexpected passphrase prompt")
        }
        fn confirm(&self, question: &str) -> bool {
            self.asked.borrow_mut().push(question.to_string());
            self.confirms
                .borrow_mut()
                .pop_front()
                .expect("unexpected confirmation prompt")
        }
        fn line(&self, question: &str) -> String {
            self.asked.borrow_mut().push(question.to_string());
            self.lines
                .borrow_mut()
                .pop_front()
                .expect("unexpected line prompt")
        }
        fn say(&self, text: &str) {
            self.said.borrow_mut().push(text.to_string());
        }
    }

    // ---- helpers -------------------------------------------------------------------------------

    fn cfg() -> MergeGuardConfig {
        MergeGuardConfig::parse(COMMITTED).unwrap()
    }

    fn open_pr() -> FakeGitHub {
        let fake = FakeGitHub::new();
        fake.add_pr(PR, HEAD, "main");
        fake
    }

    const TOKEN: &str = "ghs_FAKEINSTALLATIONTOKEN";

    /// The token seam: records every exchange and revoke; answers with [`TOKEN`].
    #[derive(Default)]
    struct Exchange<'a> {
        calls: RefCell<Vec<(String, u64)>>,
        repos: RefCell<Vec<String>>,
        scopes: RefCell<Vec<TokenScope>>,
        revoked: RefCell<Vec<String>>,
        fail_exchange: bool,
        /// Refuses exchanges for these scopes with these HTTP statuses (as GitHub refuses a
        /// permission the installation hasn't granted).
        refuse: Vec<(TokenScope, u16)>,
        fail_revoke: bool,
        on_exchange: Option<Box<dyn Fn() + 'a>>,
        on_revoke: Option<Box<dyn Fn() + 'a>>,
    }

    impl InstallationTokens for Exchange<'_> {
        fn exchange(
            &self,
            jwt: &str,
            installation_id: u64,
            repo: &str,
            scope: TokenScope,
        ) -> Result<String> {
            self.calls
                .borrow_mut()
                .push((jwt.to_string(), installation_id));
            self.repos.borrow_mut().push(repo.to_string());
            self.scopes.borrow_mut().push(scope);
            if let Some(f) = &self.on_exchange {
                f();
            }
            if self.fail_exchange {
                return Err(TokenRefused {
                    status: 401,
                    message: "Bad credentials".into(),
                }
                .into());
            }
            if let Some(&(_, status)) = self.refuse.iter().find(|(s, _)| *s == scope) {
                return Err(TokenRefused {
                    status,
                    message: "The permissions requested are not granted to this installation."
                        .into(),
                }
                .into());
            }
            Ok(TOKEN.to_string())
        }
        fn revoke(&self, token: &str) -> Result<()> {
            self.revoked.borrow_mut().push(token.to_string());
            if let Some(f) = &self.on_revoke {
                f();
            }
            if self.fail_revoke {
                // A badly behaved error that echoes the token: the warning must hide it.
                bail!("revoking {token} failed with HTTP 500");
            }
            Ok(())
        }
    }

    fn run(
        fake: &FakeGitHub,
        prompt: &FakePrompt,
        exchange: &Exchange,
        summary: &str,
        expected_head: &str,
    ) -> Result<Rerun> {
        run_with_pause(fake, prompt, exchange, summary, expected_head, &|_| {
            panic!("approve paused, but no guard run was unfinished")
        })
    }

    fn run_with_pause(
        fake: &FakeGitHub,
        prompt: &FakePrompt,
        exchange: &Exchange,
        summary: &str,
        expected_head: &str,
        pause: &dyn Fn(Duration),
    ) -> Result<Rerun> {
        approve(
            PR,
            &cfg(),
            fake,
            prompt,
            &key_file(),
            None,
            exchange,
            summary,
            NOW,
            expected_head,
            pause,
        )
    }

    /// A `pull_request` run of the guard for PR [`PR`] in this repository, at `sha`.
    fn guard_run(id: u64, sha: &str, status: &str, conclusion: Option<&str>) -> WorkflowRun {
        WorkflowRun {
            id,
            event: "pull_request".into(),
            head_sha: sha.into(),
            status: status.into(),
            conclusion: conclusion.map(str::to_owned),
            path: GUARD_WORKFLOW_PATH.into(),
            head_repo: "xenofei/LILITH".into(),
            pr_numbers: vec![PR],
        }
    }

    fn add_guard_run(fake: &FakeGitHub, run: WorkflowRun) {
        fake.add_workflow_run(GUARD_WORKFLOW, run);
    }

    /// An open PR at [`HEAD`] whose guard has one failed `pull_request` run, id 12.
    fn pr_with_guard_run() -> FakeGitHub {
        let fake = open_pr();
        add_guard_run(&fake, guard_run(12, HEAD, "completed", Some("failure")));
        fake
    }

    fn approved() -> FakePrompt {
        FakePrompt::new().confirms(&[true]).passphrases(&[PASS])
    }

    fn warning(r: Rerun) -> String {
        match r {
            Rerun::Warning { text, .. } => text,
            other => panic!("expected a warning, got {other:?}"),
        }
    }

    const CHECKS_URL: &str = "https://github.com/xenofei/LILITH/pull/42/checks";
    const RUN_12_URL: &str = "https://github.com/xenofei/LILITH/actions/runs/12";
    const WARNING_START: &str =
        "Approval posted, but the guard check couldn't be re-run automatically (";

    fn claims(jwt: &str) -> (serde_json::Value, serde_json::Value) {
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "{jwt}");
        let decode = |s: &str| -> serde_json::Value {
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(s).unwrap()).unwrap()
        };
        (decode(parts[0]), decode(parts[1]))
    }

    fn err_text<T: fmt::Debug>(r: Result<T>) -> String {
        format!("{:#}", r.expect_err("expected a refusal"))
    }

    // ---- the brief's seven tests ---------------------------------------------------------------

    #[test]
    fn encrypt_decrypt_round_trip() {
        let pem = test_pem();
        let blob = encrypt_key(pem, PASS).unwrap();
        assert!(
            blob.starts_with(b"age-encryption.org/v1"),
            "not an age file"
        );
        assert!(
            !blob.windows(20).any(|w| pem.windows(20).any(|p| p == w)),
            "plaintext PEM leaked into the encrypted blob"
        );
        assert_eq!(decrypt_key(&blob, PASS).unwrap(), pem);
        assert!(decrypt_key(&blob, "not the passphrase").is_err());
    }

    #[test]
    fn wrong_passphrase_posts_nothing() {
        let fake = open_pr();
        let prompt = FakePrompt::new()
            .confirms(&[true])
            .passphrases(&["definitely wrong passphrase"]);
        let exchange = Exchange::default();
        let e = err_text(run(&fake, &prompt, &exchange, SUMMARY, HEAD));
        assert_eq!(e, WRONG_PASSPHRASE);
        assert!(!e.contains("definitely wrong"), "{e}");
        assert!(exchange.calls.borrow().is_empty(), "token exchange ran");
        assert!(fake.posted_reviews().is_empty());
    }

    // ---- Windows Hello through approve (Task 28; the pure parts are tests/reviewer_hello.rs) --

    /// Windows Hello for `approve`: the test key, or `KeyError` (cancelled, failed).
    struct HelloStandIn(std::result::Result<(), KeyError>);

    impl Unlocker for HelloStandIn {
        fn unlock(&self) -> std::result::Result<Zeroizing<Vec<u8>>, KeyError> {
            self.0.map(|()| Zeroizing::new(test_pem().to_vec()))
        }
    }

    fn run_with_hello(
        fake: &FakeGitHub,
        prompt: &FakePrompt,
        exchange: &Exchange,
        hello: &dyn Unlocker,
    ) -> Result<Rerun> {
        approve(
            PR,
            &cfg(),
            fake,
            prompt,
            &key_file(),
            Some(hello),
            exchange,
            SUMMARY,
            NOW,
            HEAD,
            &|_| {},
        )
    }

    #[test] // spec: QA-088
    fn approve_with_hello_enrolled_posts_without_asking_the_passphrase() {
        let fake = open_pr();
        // No passphrase is scripted: being asked for one panics.
        let prompt = FakePrompt::new().confirms(&[true]);
        let exchange = Exchange::default();
        run_with_hello(&fake, &prompt, &exchange, &HelloStandIn(Ok(()))).unwrap();
        assert_eq!(fake.posted_reviews().len(), 1);
        assert_eq!(fake.posted_reviews()[0].sha, HEAD);
        assert_eq!(exchange.calls.borrow().len(), 1);
    }

    /// The existing invariant through Windows Hello: whatever Hello does, no App call happens
    /// until a key is unlocked, and a wrong passphrase after it still posts nothing.
    #[test] // spec: QA-088, QA-020
    fn a_failed_hello_then_a_wrong_passphrase_touches_no_network() {
        for failed in [KeyError::Cancelled, KeyError::Unavailable] {
            let fake = open_pr();
            let prompt = FakePrompt::new()
                .confirms(&[true])
                .passphrases(&["definitely wrong passphrase"]);
            let exchange = Exchange::default();
            let e = err_text(run_with_hello(
                &fake,
                &prompt,
                &exchange,
                &HelloStandIn(Err(failed)),
            ));
            assert_eq!(e, WRONG_PASSPHRASE);
            assert!(exchange.calls.borrow().is_empty(), "token exchange ran");
            assert!(fake.app_jwts().is_empty(), "GET /app ran");
            assert!(fake.posted_reviews().is_empty());
            assert!(
                prompt.said().iter().any(|s| s.contains("passphrase")),
                "{:?}",
                prompt.said()
            );

            // The right passphrase after the failed Hello approves.
            let fake = open_pr();
            let prompt = FakePrompt::new().confirms(&[true]).passphrases(&[PASS]);
            let exchange = Exchange::default();
            run_with_hello(&fake, &prompt, &exchange, &HelloStandIn(Err(failed))).unwrap();
            assert_eq!(fake.posted_reviews().len(), 1);
        }
    }

    #[test]
    fn approve_refuses_without_tty() {
        let fake = open_pr();
        let prompt = FakePrompt::not_a_terminal();
        let exchange = Exchange::default();
        let e = err_text(run(&fake, &prompt, &exchange, SUMMARY, HEAD));
        assert!(e.contains("terminal"), "{e}");
        assert!(e.contains("nothing was approved"), "{e}");
        assert!(prompt.asked().is_empty(), "asked {:?}", prompt.asked());
        assert!(exchange.calls.borrow().is_empty());
        assert!(fake.posted_reviews().is_empty());
    }

    #[test]
    fn declined_confirmation_posts_nothing() {
        let fake = open_pr();
        let prompt = FakePrompt::new().confirms(&[false]);
        let exchange = Exchange::default();
        let e = err_text(run(&fake, &prompt, &exchange, SUMMARY, HEAD));
        assert!(e.contains("nothing was approved"), "{e}");
        let asked = prompt.asked();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert_eq!(
            asked[0],
            "Approve these protected changes on 1234567? (y/N)"
        );
        assert!(exchange.calls.borrow().is_empty());
        assert!(fake.posted_reviews().is_empty());
    }

    #[test]
    fn jwt_has_expected_claims() {
        let jwt = app_jwt(123, test_pem(), NOW).unwrap();
        let (header, payload) = claims(&jwt);
        assert_eq!(header["alg"], "RS256");
        assert_eq!(payload["iss"], "123");
        assert_eq!(payload["iat"], NOW - 60);
        assert_eq!(payload["exp"], NOW + 540);

        // The signature verifies against the key's public half.
        use aws_lc_rs::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
        let (signed, sig) = jwt.rsplit_once('.').unwrap();
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &test_key().public_pkcs1_der)
            .verify(signed.as_bytes(), &URL_SAFE_NO_PAD.decode(sig).unwrap())
            .expect("JWT signature verifies");

        // GitHub hands out PKCS#1 ("BEGIN RSA PRIVATE KEY") files; those work too.
        let jwt1 = app_jwt(123, &test_key().pkcs1_pem, NOW).unwrap();
        assert_eq!(claims(&jwt1).1["iss"], "123");

        let garbage = b"-----BEGIN PRIVATE KEY-----\nbm9wZQ==\n-----END PRIVATE KEY-----\n";
        assert!(app_jwt(123, garbage, NOW).is_err());
    }

    #[test]
    fn review_is_pinned_to_head_sha() {
        let fake = open_pr();
        fake.add_review(PR, "lilith-reviewer[bot]", "APPROVED", "an-older-commit");
        let prompt = FakePrompt::new().confirms(&[true]).passphrases(&[PASS]);
        let exchange = Exchange::default();
        run(&fake, &prompt, &exchange, SUMMARY, HEAD).unwrap();

        let posted = fake.posted_reviews();
        assert_eq!(posted.len(), 1, "exactly one review: {posted:?}");
        assert_eq!(posted[0].number, PR);
        assert_eq!(posted[0].sha, HEAD);
        assert!(
            posted[0]
                .body
                .contains("crates/lil-spec-tests/tests/smoke.rs"),
            "{}",
            posted[0].body
        );
        assert!(posted[0].body.contains(HEAD), "{}", posted[0].body);

        // One exchange, for the configured installation, with a JWT for the configured App.
        let calls = exchange.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, 456);
        assert_eq!(claims(&calls[0].0).1["iss"], "123");
        // Scoped to the configured repo, and revoked after the post.
        assert_eq!(*exchange.repos.borrow(), vec!["LILITH".to_string()]);
        assert_eq!(*exchange.revoked.borrow(), vec![TOKEN.to_string()]);

        // The merge guard now counts it.
        let reviews = fake.reviews(PR).unwrap();
        assert!(app_approved(&reviews, &cfg().reviewer_login(), HEAD));
    }

    #[test]
    fn key_path_is_outside_repo_and_suite_data() {
        let profile = tempfile::tempdir().unwrap();
        let profile = profile.path();
        assert_eq!(
            key_path_in(profile),
            profile
                .join(names::HOME_DIR)
                .join("reviewer")
                .join("app-key.age")
        );

        let repo = crate::dev::spec_tests::workspace_root()
            .canonicalize()
            .unwrap();

        /// `true` when `path` is under `root\<App>*` for either name (any channel suffix).
        fn under_suite_data(path: &Path, root: &Path) -> bool {
            path.strip_prefix(root).ok().is_some_and(|rest| {
                rest.components().next().is_some_and(|c| {
                    let c = c.as_os_str().to_string_lossy().to_ascii_lowercase();
                    [names::APP_NAME, names::LEGACY_APP_NAME]
                        .iter()
                        .any(|app| c.starts_with(&app.to_ascii_lowercase()))
                })
            })
        }
        let path = key_path_in(profile);
        assert!(!path.starts_with(&repo), "{path:?} is inside the repo");
        // Windows puts both app-data roots inside the profile; check that layout, injected.
        for root in [
            profile.join("AppData").join("Roaming"),
            profile.join("AppData").join("Local"),
        ] {
            assert!(!under_suite_data(&path, &root), "{path:?} under {root:?}");
        }
        // And against this machine's roots (the variables' values only; nothing under them is
        // resolved or touched).
        for var in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(root) = std::env::var_os(var) {
                assert!(
                    !under_suite_data(&path, Path::new(&root)),
                    "{path:?} under %{var}%"
                );
            }
        }
        // The home folder itself is not a suite data root.
        assert!(!under_suite_data(&path, profile));
    }

    #[test]
    fn key_lookup_finds_the_key_path() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path();
        let p = key_path_in(profile);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "key").unwrap();
        assert_eq!(find_key_in(profile), Some(p.clone()));
        assert_eq!(locate_key_in(profile).unwrap(), p);
    }

    #[test]
    fn missing_key_message_names_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path();
        assert_eq!(find_key_in(profile), None);
        let text = format!("{:#}", locate_key_in(profile).unwrap_err());
        assert!(
            text.contains(&key_path_in(profile).display().to_string()),
            "{text}"
        );
        assert!(text.contains("lil dev reviewer-setup"), "{text}");
        assert!(load_key_in(profile).is_err());
        // Looking never creates the home folder.
        assert!(!profile.join(names::HOME_DIR).exists());
    }

    /// Anything at the key path is the key or a refusal.
    #[test]
    fn occupied_key_path_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path();

        // A folder at the path: chosen, and loading it refuses.
        let path = key_path_in(profile);
        std::fs::create_dir_all(&path).unwrap();
        assert_eq!(find_key_in(profile), Some(path.clone()));
        let text = format!("{:#}", load_key_in(profile).unwrap_err());
        assert!(text.contains(&path.display().to_string()), "{text}");
        std::fs::remove_dir(&path).unwrap();

        // A damaged file at the path: the same.
        std::fs::write(&path, "not a key file").unwrap();
        assert_eq!(find_key_in(profile), Some(path.clone()));
        let text = format!("{:#}", load_key_in(profile).unwrap_err());
        assert!(text.contains(&path.display().to_string()), "{text}");

        // A good file loads.
        key_file().save(&path).unwrap();
        assert_eq!(load_key_in(profile).unwrap().0, path);
    }

    // ---- R20: TOCTOU, nothing protected, PR state ----------------------------------------------

    #[test]
    fn head_moved_since_the_summary_posts_nothing() {
        // Already moved before approve runs.
        let fake = open_pr();
        fake.set_head(PR, NEW);
        let prompt = FakePrompt::new().confirms(&[true]).passphrases(&[PASS]);
        let e = err_text(run(&fake, &prompt, &Exchange::default(), SUMMARY, HEAD));
        assert_eq!(e, HEAD_MOVED);
        assert!(fake.posted_reviews().is_empty());

        // Moves while the passphrase is typed and the token is exchanged.
        let fake = open_pr();
        let prompt = FakePrompt::new().confirms(&[true]).passphrases(&[PASS]);
        let exchange = Exchange {
            on_exchange: Some(Box::new(|| fake.set_head(PR, NEW))),
            ..Exchange::default()
        };
        let e = err_text(run(&fake, &prompt, &exchange, SUMMARY, HEAD));
        assert_eq!(e, HEAD_MOVED);
        assert!(fake.posted_reviews().is_empty());
        // The token it got is revoked anyway.
        assert_eq!(*exchange.revoked.borrow(), vec![TOKEN.to_string()]);
    }

    #[test]
    fn nothing_protected_posts_nothing() {
        for summary in ["", "  \n "] {
            let fake = open_pr();
            let prompt = FakePrompt::new();
            let exchange = Exchange::default();
            let e = err_text(run(&fake, &prompt, &exchange, summary, HEAD));
            assert_eq!(e, NOTHING_PROTECTED);
            assert!(prompt.asked().is_empty(), "{:?}", prompt.asked());
            assert!(exchange.calls.borrow().is_empty());
            assert!(fake.posted_reviews().is_empty());
        }
    }

    #[test]
    fn draft_closed_or_wrong_base_posts_nothing() {
        let draft = open_pr();
        draft.set_draft(PR, true);
        let closed = open_pr();
        closed.set_state(PR, "closed");
        let wrong_base = FakeGitHub::new();
        wrong_base.add_pr(PR, HEAD, "release");
        for (fake, want) in [
            (&draft, "draft"),
            (&closed, "closed"),
            (&wrong_base, "release"),
        ] {
            let prompt = FakePrompt::new().confirms(&[true]).passphrases(&[PASS]);
            let e = err_text(run(fake, &prompt, &Exchange::default(), SUMMARY, HEAD));
            assert!(e.contains(want), "{e}");
            assert!(e.contains("nothing was approved"), "{e}");
            assert!(fake.posted_reviews().is_empty());
        }
    }

    // ---- reviewer-setup ------------------------------------------------------------------------

    fn write_pem(dir: &Path, pem: &[u8]) -> PathBuf {
        let p = dir.join("lilith-reviewer.2026-10-13.private-key.pem");
        std::fs::write(&p, pem).unwrap();
        p
    }

    #[test]
    fn setup_writes_the_encrypted_key_and_keeps_the_pem() {
        let tmp = tempfile::tempdir().unwrap();
        let pem_path = write_pem(tmp.path(), &test_key().pkcs1_pem);
        let key_path = key_path_in(&tmp.path().join("profile"));
        // Quoted, as Explorer's "Copy as path" gives it.
        let quoted = format!("\"{}\"", pem_path.display());
        let prompt = FakePrompt::new()
            .lines(&["123", "456", &quoted])
            .passphrases(&[PASS, PASS]);
        setup_with_work_factor(&prompt, &key_path, TEST_WORK_FACTOR).unwrap();

        let saved = ReviewerKeyFile::load(&key_path).unwrap();
        assert_eq!(saved.app_id, 123);
        assert_eq!(saved.installation_id, 456);
        assert_eq!(
            decrypt_key(&saved.encrypted_pem, PASS).unwrap(),
            test_key().pkcs1_pem
        );
        let on_disk = std::fs::read_to_string(&key_path).unwrap();
        assert!(!on_disk.contains("PRIVATE KEY"), "plaintext key on disk");
        // The plaintext .pem is the owner's to delete.
        assert_eq!(std::fs::read(&pem_path).unwrap(), test_key().pkcs1_pem);
    }

    /// Every file under `dir` with its bytes, sorted; empty when `dir` doesn't exist.
    /// `reviewer-setup` writes the key at the key path, and `approve` then finds it.
    #[test]
    fn reviewer_setup_writes_the_key_path() {
        let tmp = tempfile::tempdir().unwrap();
        let pem_path = write_pem(tmp.path(), &test_key().pkcs1_pem);
        let pem_answer = pem_path.display().to_string();
        let profile = tmp.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let path = setup_key_path_in(&profile);
        assert_eq!(path, key_path_in(&profile));
        let prompt = FakePrompt::new()
            .lines(&["123", "456", &pem_answer])
            .passphrases(&[PASS, PASS]);
        setup_with_work_factor(&prompt, &path, TEST_WORK_FACTOR).unwrap();

        let saved = ReviewerKeyFile::load(&key_path_in(&profile)).unwrap();
        assert_eq!(saved.app_id, 123);
        assert_eq!(find_key_in(&profile), Some(key_path_in(&profile)));
    }

    #[test]
    fn setup_overwrite_declined_keeps_the_existing_key() {
        let tmp = tempfile::tempdir().unwrap();
        let pem_path = write_pem(tmp.path(), test_pem());
        let key_path = key_path_in(&tmp.path().join("profile"));
        std::fs::create_dir_all(key_path.parent().unwrap()).unwrap();
        std::fs::write(&key_path, "existing").unwrap();

        let prompt = FakePrompt::new().confirms(&[false]);
        let e = format!(
            "{:#}",
            setup_with_work_factor(&prompt, &key_path, TEST_WORK_FACTOR).unwrap_err()
        );
        assert!(e.contains("nothing was written"), "{e}");
        assert_eq!(std::fs::read_to_string(&key_path).unwrap(), "existing");
        assert_eq!(prompt.asked().len(), 1, "{:?}", prompt.asked());
        assert!(prompt.asked()[0].contains("(y/N)"), "{:?}", prompt.asked());

        // Saying yes replaces it.
        let prompt = FakePrompt::new()
            .confirms(&[true])
            .lines(&["123", "456", pem_path.to_str().unwrap()])
            .passphrases(&[PASS, PASS]);
        setup_with_work_factor(&prompt, &key_path, TEST_WORK_FACTOR).unwrap();
        assert_eq!(ReviewerKeyFile::load(&key_path).unwrap().app_id, 123);
    }

    #[test]
    fn setup_rejects_mismatched_or_short_passphrases() {
        let tmp = tempfile::tempdir().unwrap();
        let pem_path = write_pem(tmp.path(), test_pem());
        let key_path = key_path_in(&tmp.path().join("profile"));
        let pem_arg = pem_path.to_str().unwrap();
        for (a, b, want) in [
            (PASS, "correct horse battery!", "don't match"),
            ("short pass1", "short pass1", "at least 12 characters"),
        ] {
            let prompt = FakePrompt::new()
                .lines(&["123", "456", pem_arg])
                .passphrases(&[a, b]);
            let e = format!(
                "{:#}",
                setup_with_work_factor(&prompt, &key_path, TEST_WORK_FACTOR).unwrap_err()
            );
            assert!(e.contains(want), "{e}");
            assert!(e.contains("nothing was written"), "{e}");
            assert!(
                !e.contains(a) && !e.contains(b),
                "passphrase in the message: {e}"
            );
            assert!(!key_path.exists());
        }
        // Exactly 12 characters is enough.
        let prompt = FakePrompt::new()
            .lines(&["123", "456", pem_arg])
            .passphrases(&["twelve chars", "twelve chars"]);
        setup_with_work_factor(&prompt, &key_path, TEST_WORK_FACTOR).unwrap();
    }

    #[test]
    fn setup_rejects_a_file_that_is_not_an_rsa_key() {
        let tmp = tempfile::tempdir().unwrap();
        let pem_path = write_pem(tmp.path(), b"not a key at all");
        let key_path = key_path_in(&tmp.path().join("profile"));
        let prompt = FakePrompt::new().lines(&["123", "456", pem_path.to_str().unwrap()]);
        let e = format!(
            "{:#}",
            setup_with_work_factor(&prompt, &key_path, TEST_WORK_FACTOR).unwrap_err()
        );
        assert!(e.contains("RSA private key"), "{e}");
        assert!(!key_path.exists());
        assert!(pem_path.exists());
    }

    #[test]
    fn setup_refuses_without_tty() {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = key_path_in(&tmp.path().join("profile"));
        let prompt = FakePrompt::not_a_terminal();
        let e = format!(
            "{:#}",
            setup_with_work_factor(&prompt, &key_path, TEST_WORK_FACTOR).unwrap_err()
        );
        assert!(e.contains("terminal"), "{e}");
        assert!(prompt.asked().is_empty());
        assert!(!key_path.exists());
    }

    // ---- secrets, file format, token exchange --------------------------------------------------

    #[test]
    fn debug_redacts_secrets() {
        let kf = key_file();
        let shown = format!("{kf:?}");
        assert!(shown.contains("123") && shown.contains("456"), "{shown}");
        assert!(shown.contains("redacted"), "{shown}");
        assert!(
            !shown.contains(&STANDARD.encode(&kf.encrypted_pem)),
            "{shown}"
        );
        assert!(
            !shown.contains(&format!("{:?}", &kf.encrypted_pem[..8])),
            "{shown}"
        );

        let shown = format!("{:?}", TerminalPrompt);
        assert_eq!(shown, "TerminalPrompt");
    }

    #[test]
    fn key_file_toml_round_trip() {
        let kf = key_file();
        let text = kf.to_toml().unwrap();
        assert!(text.contains("app_id = 123"), "{text}");
        assert!(text.contains("installation_id = 456"), "{text}");
        let back = ReviewerKeyFile::from_toml(&text).unwrap();
        assert_eq!(back.app_id, 123);
        assert_eq!(back.installation_id, 456);
        assert_eq!(back.encrypted_pem, kf.encrypted_pem);
        assert!(ReviewerKeyFile::from_toml("app_id = 1\n").is_err());
        assert!(ReviewerKeyFile::from_toml(&text.replace("app_id = 123", "app_id = 0")).is_err());
        assert!(ReviewerKeyFile::from_toml(&format!("{text}extra = 1\n")).is_err());
    }

    #[test]
    fn token_exchange_request_and_response() {
        // R40: the token can post the review and re-run the guard check, in this repo only.
        let (m, path, body) = build_token_request(456, "LILITH", TokenScope::ReviewAndRerun);
        assert_eq!(m, reqwest::Method::POST);
        assert_eq!(path, "/app/installations/456/access_tokens");
        assert_eq!(
            body,
            serde_json::json!({
                "repositories": ["LILITH"],
                "permissions": {"pull_requests": "write", "actions": "write"}
            })
        );
        // The fallback for an installation that hasn't granted Actions write yet.
        let (_, _, body) = build_token_request(456, "LILITH", TokenScope::ReviewOnly);
        assert_eq!(
            body,
            serde_json::json!({
                "repositories": ["LILITH"],
                "permissions": {"pull_requests": "write"}
            })
        );
        let (m, path) = build_revoke_request();
        assert_eq!(m, reqwest::Method::DELETE);
        assert_eq!(path, "/installation/token");
        let ok = serde_json::json!({"token": "ghs_abc", "expires_at": "2026-10-13T00:00:00Z"});
        assert_eq!(parse_token_response(201, &ok).unwrap(), "ghs_abc");
        let refused = parse_token_response(401, &serde_json::json!({"message": "Bad credentials"}))
            .unwrap_err();
        assert_eq!(
            refused.downcast_ref::<TokenRefused>().map(|r| r.status),
            Some(401)
        );
        let e = refused.to_string();
        assert!(e.contains("401") && e.contains("Bad credentials"), "{e}");
        assert!(parse_token_response(201, &serde_json::json!({})).is_err());
        assert!(parse_token_response(201, &serde_json::json!({"token": ""})).is_err());
    }

    // ---- fix round 1 -----------------------------------------------------------------------

    #[test]
    fn token_exchange_failure_posts_nothing() {
        let fake = open_pr();
        let prompt = FakePrompt::new().confirms(&[true]).passphrases(&[PASS]);
        let exchange = Exchange {
            fail_exchange: true,
            ..Exchange::default()
        };
        let e = err_text(run(&fake, &prompt, &exchange, SUMMARY, HEAD));
        assert!(e.contains("nothing was approved"), "{e}");
        assert!(fake.posted_reviews().is_empty());
        assert!(exchange.revoked.borrow().is_empty(), "no token to revoke");
    }

    #[test]
    fn failed_revoke_only_warns_and_hides_the_token() {
        let fake = open_pr();
        let prompt = FakePrompt::new().confirms(&[true]).passphrases(&[PASS]);
        let exchange = Exchange {
            fail_revoke: true,
            ..Exchange::default()
        };
        run(&fake, &prompt, &exchange, SUMMARY, HEAD).unwrap();
        assert_eq!(fake.posted_reviews().len(), 1);
        assert_eq!(exchange.revoked.borrow().len(), 1);

        let warning = revoke_warning(&anyhow!("revoking {TOKEN} failed with HTTP 500"), TOKEN);
        assert!(!warning.contains(TOKEN), "{warning}");
        assert!(warning.contains("HTTP 500"), "{warning}");
    }

    /// The same age file with its scrypt stanza demanding `log_n` (the header MAC no longer
    /// matches, which is fine: it must be refused before that matters).
    fn with_work_factor(blob: &[u8], log_n: u8) -> Vec<u8> {
        let text = String::from_utf8_lossy(blob).into_owned();
        let line = text
            .lines()
            .find(|l| l.starts_with("-> scrypt "))
            .unwrap()
            .to_string();
        let mut parts: Vec<String> = line.split(' ').map(str::to_string).collect();
        parts[3] = log_n.to_string();
        let mut out = blob.to_vec();
        let at = text.find(&line).unwrap();
        out.splice(at..at + line.len(), parts.join(" ").into_bytes());
        out
    }

    #[test]
    fn damaged_key_file_is_refused_before_the_passphrase_prompt() {
        let good = key_file();
        let truncated_header = good.encrypted_pem[..30].to_vec();
        let excessive = with_work_factor(&good.encrypted_pem, 25);
        for blob in [b"not an age file".to_vec(), truncated_header, excessive] {
            let fake = open_pr();
            // No passphrase is scripted: being asked for one panics.
            let prompt = FakePrompt::new().confirms(&[true]);
            let exchange = Exchange::default();
            let damaged = ReviewerKeyFile {
                encrypted_pem: blob,
                ..good.clone()
            };
            let e = err_text(approve(
                PR,
                &cfg(),
                &fake,
                &prompt,
                &damaged,
                None,
                &exchange,
                SUMMARY,
                NOW,
                HEAD,
                &|_| {},
            ));
            assert!(e.contains(DAMAGED_KEY), "{e}");
            assert!(e.contains("nothing was posted"), "{e}");
            assert!(exchange.calls.borrow().is_empty());
            assert!(fake.posted_reviews().is_empty());
        }
    }

    #[test]
    fn decrypt_errors_tell_wrong_passphrase_from_damage() {
        let blob = encrypt_key_with_work_factor(test_pem(), PASS, TEST_WORK_FACTOR).unwrap();
        let kind = |b: &[u8], p: &str| {
            decrypt_key(b, p)
                .unwrap_err()
                .downcast_ref::<KeyError>()
                .copied()
        };
        assert_eq!(
            kind(&blob, "a wrong passphrase"),
            Some(KeyError::WrongPassphrase)
        );
        assert_eq!(
            kind(&blob[..blob.len() - 10], PASS),
            Some(KeyError::Damaged)
        );
        assert_eq!(kind(&blob[..30], PASS), Some(KeyError::Damaged));
        assert_eq!(kind(b"garbage", PASS), Some(KeyError::Damaged));
        assert_eq!(
            kind(&with_work_factor(&blob, 25), PASS),
            Some(KeyError::Damaged)
        );
        assert_eq!(MAX_WORK_FACTOR, 20);
        for k in [KeyError::WrongPassphrase, KeyError::Damaged] {
            let shown = k.to_string();
            assert!(!shown.contains(PASS), "{shown}");
        }
        assert_eq!(KeyError::Damaged.to_string(), DAMAGED_KEY);
    }

    #[test]
    fn failed_save_leaves_no_temp_file() {
        let tmp = tempfile::tempdir().unwrap();
        // The target is a non-empty directory, so the rename over it fails.
        let key_path = tmp.path().join("app-key.age");
        std::fs::create_dir_all(key_path.join("occupied")).unwrap();
        assert!(key_file().save(&key_path).is_err());
        let left: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            left,
            vec![std::ffi::OsString::from("app-key.age")],
            "{left:?}"
        );
    }

    // ---- R40: after the approval, re-run the guard's failed pull_request runs -----------------

    #[test] // spec: QA-020, QA-033
    fn approval_reruns_every_failed_guard_run_of_this_pr_for_the_head() {
        let fake = open_pr();
        // This PR's runs for HEAD: an old success (skipped) and three non-green ones (a close
        // and reopen, a cancellation): each is its own check suite, so each is re-run.
        add_guard_run(&fake, guard_run(9, HEAD, "completed", Some("success")));
        add_guard_run(&fake, guard_run(10, HEAD, "completed", Some("failure")));
        add_guard_run(&fake, guard_run(11, HEAD, "completed", Some("cancelled")));
        add_guard_run(&fake, guard_run(12, HEAD, "completed", Some("failure")));
        // Decoys with the same SHA: another PR, a fork, one waiting for a maintainer.
        add_guard_run(
            &fake,
            WorkflowRun {
                pr_numbers: vec![99],
                ..guard_run(13, HEAD, "completed", Some("failure"))
            },
        );
        add_guard_run(
            &fake,
            WorkflowRun {
                pr_numbers: vec![],
                head_repo: "someone/LILITH".into(),
                ..guard_run(14, HEAD, "completed", Some("failure"))
            },
        );
        add_guard_run(
            &fake,
            guard_run(15, HEAD, "completed", Some("action_required")),
        );
        // Another commit, another event, another workflow.
        add_guard_run(&fake, guard_run(20, NEW, "completed", Some("failure")));
        add_guard_run(
            &fake,
            WorkflowRun {
                event: "pull_request_review".into(),
                ..guard_run(21, HEAD, "completed", Some("failure"))
            },
        );
        fake.add_workflow_run(
            "ci-pr-ready.yml",
            guard_run(30, HEAD, "completed", Some("failure")),
        );
        let exchange = Exchange::default();
        let rerun = run(&fake, &approved(), &exchange, SUMMARY, HEAD).unwrap();

        assert_eq!(
            rerun,
            Rerun::Requested {
                run_ids: vec![10, 11, 12]
            }
        );
        assert_eq!(fake.reruns(), vec![10, 11, 12]);
        assert_eq!(
            fake.run_queries(),
            vec![(
                "ci-guard.yml".to_string(),
                "pull_request".to_string(),
                HEAD.to_string()
            )]
        );
        assert_eq!(fake.posted_reviews().len(), 1);
        assert_eq!(*exchange.revoked.borrow(), vec![TOKEN.to_string()]);
    }

    #[test] // spec: QA-020
    fn guard_runs_are_only_this_prs_runs_in_this_repo() {
        let mine = guard_run(5, HEAD, "completed", Some("failure"));
        let runs = vec![
            WorkflowRun {
                event: "pull_request_review".into(),
                ..guard_run(1, HEAD, "completed", Some("failure"))
            },
            guard_run(2, NEW, "completed", Some("failure")),
            WorkflowRun {
                pr_numbers: vec![7, 99],
                ..guard_run(3, HEAD, "completed", Some("failure"))
            },
            WorkflowRun {
                pr_numbers: vec![],
                head_repo: "someone/LILITH".into(),
                ..guard_run(4, HEAD, "completed", Some("failure"))
            },
            WorkflowRun {
                path: ".github/workflows/ci-guard-copy.yml".into(),
                ..guard_run(6, HEAD, "completed", Some("failure"))
            },
            WorkflowRun {
                head_repo: "XENOFEI/lilith".into(),
                pr_numbers: vec![3, PR],
                ..guard_run(8, HEAD, "in_progress", None)
            },
            mine.clone(),
        ];
        let ids: Vec<u64> = guard_runs_for(&runs, &cfg(), PR, HEAD)
            .iter()
            .map(|r| r.id)
            .collect();
        // Oldest first; the owner and repo compare case-insensitively, as GitHub's do.
        assert_eq!(ids, vec![5, 8]);
        assert!(guard_runs_for(&runs[..5], &cfg(), PR, HEAD).is_empty());
        assert!(guard_runs_for(&[mine], &cfg(), 43, HEAD).is_empty());
    }

    #[test] // spec: QA-020
    fn the_token_is_requested_with_actions_write() {
        let fake = pr_with_guard_run();
        let exchange = Exchange::default();
        run(&fake, &approved(), &exchange, SUMMARY, HEAD).unwrap();
        assert_eq!(*exchange.scopes.borrow(), vec![TokenScope::ReviewAndRerun]);
        assert_eq!(*exchange.repos.borrow(), vec!["LILITH".to_string()]);
        assert_eq!(
            TokenScope::ReviewAndRerun.permissions(),
            serde_json::json!({"pull_requests": "write", "actions": "write"})
        );
    }

    // ---- the App's live slug (plan 2026-10-17, D6) ----------------------------------------------

    #[test] // spec: QA-020
    fn approve_prints_the_live_slug() {
        let live = "lilith-reviewer";
        let fake = pr_with_guard_run();
        fake.set_app_slug(live);
        let exchange = Exchange {
            on_exchange: Some(Box::new(|| fake.log_event("exchange"))),
            ..Exchange::default()
        };
        let prompt = approved();
        run(&fake, &prompt, &exchange, SUMMARY, HEAD).unwrap();
        let said = prompt.said();
        assert!(
            said.contains(&format!("reviewer App slug: {live}")),
            "{said:?}"
        );
        // Asked once, with the App's JWT, before the token exchange.
        let jwts = fake.app_jwts();
        assert_eq!(jwts.len(), 1, "{jwts:?}");
        assert_eq!(claims(&jwts[0]).1["iss"], "123");
        assert_eq!(fake.events()[..2], ["app", "exchange"]);
        // The approval is posted under the live slug, and the merge guard counts it.
        let reviews = fake.reviews(PR).unwrap();
        assert_eq!(reviews.last().unwrap().login, format!("{live}[bot]"));
        assert!(app_approved(&reviews, &cfg().reviewer_login(), HEAD));
    }

    #[test] // spec: QA-020
    fn approve_refuses_a_live_slug_that_is_not_the_configured_one() {
        for live in [
            "github-actions",
            "lilith-reviewer-next",
            "Lilith-Reviewer",
            "lilith-reviewer[bot]",
            "lilith-reviewer\u{1b}[2J",
        ] {
            let fake = pr_with_guard_run();
            fake.set_app_slug(live);
            let exchange = Exchange::default();
            let prompt = approved();
            let e = err_text(run(&fake, &prompt, &exchange, SUMMARY, HEAD));
            assert!(e.contains("nothing was approved"), "{e}");
            assert!(e.contains("names `lilith-reviewer`"), "{e}");
            assert!(!e.contains('\u{1b}'), "{e}");
            // The slug GitHub reported is still shown, escaped.
            let said = prompt.said();
            assert_eq!(said.len(), 1, "{said:?}");
            assert!(said[0].starts_with("reviewer App slug: "), "{said:?}");
            assert!(!said[0].contains('\u{1b}'), "{said:?}");
            // Nothing past the question: no token, no review, no re-run.
            assert!(exchange.calls.borrow().is_empty(), "token exchange ran");
            assert!(fake.posted_reviews().is_empty());
            assert!(fake.reruns().is_empty());
        }

        // GitHub not answering GET /app refuses too: the slug is never assumed.
        let fake = pr_with_guard_run();
        fake.fail_app_slug("HTTP 401: A JSON web token could not be decoded");
        let exchange = Exchange::default();
        let e = err_text(run(&fake, &approved(), &exchange, SUMMARY, HEAD));
        assert!(e.contains("nothing was approved"), "{e}");
        assert!(e.contains("401"), "{e}");
        assert!(exchange.calls.borrow().is_empty());
        assert!(fake.posted_reviews().is_empty());
    }

    #[test] // spec: QA-020
    fn post_then_rerun_then_revoke() {
        let fake = pr_with_guard_run();
        let exchange = Exchange {
            on_revoke: Some(Box::new(|| fake.log_event("revoke"))),
            ..Exchange::default()
        };
        run(&fake, &approved(), &exchange, SUMMARY, HEAD).unwrap();
        assert_eq!(
            fake.events(),
            ["app", "post_review", "workflow_runs", "rerun 12", "revoke"]
        );
    }

    #[test] // spec: QA-020
    fn no_guard_run_of_this_pr_warns_and_succeeds() {
        // Only runs of other PRs and forks exist for the SHA.
        let fake = open_pr();
        add_guard_run(
            &fake,
            WorkflowRun {
                pr_numbers: vec![99],
                ..guard_run(13, HEAD, "completed", Some("failure"))
            },
        );
        add_guard_run(
            &fake,
            WorkflowRun {
                pr_numbers: vec![],
                head_repo: "someone/LILITH".into(),
                ..guard_run(14, HEAD, "completed", Some("failure"))
            },
        );
        let exchange = Exchange::default();
        let w = warning(run(&fake, &approved(), &exchange, SUMMARY, HEAD).unwrap());
        assert!(w.starts_with(WARNING_START), "{w}");
        assert!(w.contains("no `pull_request` run"), "{w}");
        assert!(
            w.ends_with(&format!("Re-run it in the browser: {CHECKS_URL}")),
            "{w}"
        );
        assert!(!w.contains(TOKEN), "{w}");
        assert_eq!(fake.posted_reviews().len(), 1);
        assert!(fake.reruns().is_empty());
        assert_eq!(*exchange.revoked.borrow(), vec![TOKEN.to_string()]);
    }

    #[test] // spec: QA-020
    fn rerun_refused_warns_with_the_run_url_and_succeeds() {
        let fake = pr_with_guard_run();
        // An error that echoes the token: the warning must hide it.
        fake.fail_rerun(&format!(
            "HTTP 403: Resource not accessible by integration (token {TOKEN})"
        ));
        let exchange = Exchange::default();
        let w = warning(run(&fake, &approved(), &exchange, SUMMARY, HEAD).unwrap());
        assert!(w.starts_with(WARNING_START), "{w}");
        assert!(w.contains("403"), "{w}");
        assert!(
            w.ends_with(&format!("Re-run it in the browser: {RUN_12_URL}")),
            "{w}"
        );
        assert!(!w.contains(TOKEN), "{w}");
        assert_eq!(fake.posted_reviews().len(), 1);
        assert_eq!(*exchange.revoked.borrow(), vec![TOKEN.to_string()]);
    }

    #[test] // spec: QA-020
    fn listing_the_runs_fails_warns_and_succeeds() {
        let fake = pr_with_guard_run();
        fake.fail_workflow_runs("HTTP 502: Bad Gateway");
        let w = warning(run(&fake, &approved(), &Exchange::default(), SUMMARY, HEAD).unwrap());
        assert!(w.starts_with(WARNING_START), "{w}");
        assert!(w.contains("502"), "{w}");
        assert!(w.ends_with(CHECKS_URL), "{w}");
        assert!(fake.reruns().is_empty());
    }

    #[test] // spec: QA-020
    fn a_newest_run_that_already_passed_needs_no_rerun() {
        let fake = open_pr();
        add_guard_run(&fake, guard_run(12, HEAD, "completed", Some("success")));
        let rerun = run(&fake, &approved(), &Exchange::default(), SUMMARY, HEAD).unwrap();
        assert_eq!(rerun, Rerun::AlreadyPassed { run_id: 12 });
        assert!(fake.reruns().is_empty());

        // An older failed run is still its own suite, so it is still re-run.
        let fake = open_pr();
        add_guard_run(&fake, guard_run(10, HEAD, "completed", Some("failure")));
        add_guard_run(&fake, guard_run(12, HEAD, "completed", Some("success")));
        let rerun = run(&fake, &approved(), &Exchange::default(), SUMMARY, HEAD).unwrap();
        assert_eq!(rerun, Rerun::Requested { run_ids: vec![10] });
    }

    #[test] // spec: QA-020
    fn action_required_is_never_rerun() {
        let fake = open_pr();
        add_guard_run(
            &fake,
            guard_run(12, HEAD, "completed", Some("action_required")),
        );
        let w = warning(run(&fake, &approved(), &Exchange::default(), SUMMARY, HEAD).unwrap());
        assert!(w.contains("action_required"), "{w}");
        assert!(w.ends_with(RUN_12_URL), "{w}");
        assert!(fake.reruns().is_empty());
    }

    #[test] // spec: QA-020
    fn an_unfinished_run_is_awaited_then_rerun() {
        let fake = open_pr();
        add_guard_run(&fake, guard_run(12, HEAD, "in_progress", None));
        let pauses = RefCell::new(Vec::new());
        let pause = |d: Duration| {
            pauses.borrow_mut().push(d);
            if pauses.borrow().len() == 2 {
                fake.set_run_status(12, "completed", Some("failure"));
            }
        };
        let rerun = run_with_pause(
            &fake,
            &approved(),
            &Exchange::default(),
            SUMMARY,
            HEAD,
            &pause,
        )
        .unwrap();
        assert_eq!(rerun, Rerun::Requested { run_ids: vec![12] });
        assert_eq!(*pauses.borrow(), vec![RERUN_POLL; 2]);
        assert_eq!(fake.reruns(), vec![12]);
    }

    #[test] // spec: QA-020
    fn a_run_still_unfinished_after_the_wait_warns_and_succeeds() {
        let fake = open_pr();
        // An older failed run (a close and reopen) and the newer one still queued behind it.
        add_guard_run(&fake, guard_run(10, HEAD, "completed", Some("failure")));
        add_guard_run(&fake, guard_run(12, HEAD, "queued", None));
        let pauses = RefCell::new(Vec::new());
        let exchange = Exchange::default();
        let rerun = run_with_pause(&fake, &approved(), &exchange, SUMMARY, HEAD, &|d| {
            pauses.borrow_mut().push(d)
        })
        .unwrap();
        // Bounded: 60 s in 5 s steps.
        assert_eq!(RERUN_WAIT, Duration::from_secs(60));
        assert_eq!(RERUN_POLL, Duration::from_secs(5));
        assert_eq!(pauses.borrow().iter().sum::<Duration>(), RERUN_WAIT);
        let Rerun::Warning { text, requested } = rerun else {
            panic!("expected a warning, got {rerun:?}")
        };
        assert_eq!(
            text,
            format!(
                "Approval posted, but the guard check is still running; it will most likely read your approval when it finishes. If it ends red, re-run it: {RUN_12_URL}"
            )
        );
        // The finished failed run is re-run anyway; the unfinished one can't be.
        assert_eq!(requested, vec![10]);
        assert_eq!(fake.reruns(), vec![10]);
        assert_eq!(*exchange.revoked.borrow(), vec![TOKEN.to_string()]);
    }

    #[test] // spec: QA-020
    fn no_actions_permission_still_approves_and_warns_with_githubs_message() {
        for status in [403, 422] {
            let fake = pr_with_guard_run();
            let exchange = Exchange {
                refuse: vec![(TokenScope::ReviewAndRerun, status)],
                ..Exchange::default()
            };
            let w = warning(run(&fake, &approved(), &exchange, SUMMARY, HEAD).unwrap());
            assert_eq!(
                *exchange.scopes.borrow(),
                vec![TokenScope::ReviewAndRerun, TokenScope::ReviewOnly]
            );
            assert_eq!(fake.posted_reviews().len(), 1);
            assert!(w.starts_with(WARNING_START), "{w}");
            // GitHub's own words, then the likely cause.
            assert!(w.contains(&format!("HTTP {status}")), "{w}");
            assert!(
                w.contains("The permissions requested are not granted to this installation."),
                "{w}"
            );
            assert!(w.contains("Actions: Read and write"), "{w}");
            assert!(w.ends_with(CHECKS_URL), "{w}");
            assert!(!w.contains(TOKEN), "{w}");
            // Nothing is tried with a token that can't re-run.
            assert!(fake.run_queries().is_empty());
            assert!(fake.reruns().is_empty());
            assert_eq!(*exchange.revoked.borrow(), vec![TOKEN.to_string()]);
        }
        // Any other refusal is not retried, and nothing is approved.
        let fake = pr_with_guard_run();
        let exchange = Exchange {
            fail_exchange: true,
            ..Exchange::default()
        };
        let e = err_text(run(&fake, &approved(), &exchange, SUMMARY, HEAD));
        assert!(e.contains("nothing was approved"), "{e}");
        assert_eq!(exchange.scopes.borrow().len(), 1);
        assert!(fake.posted_reviews().is_empty());
    }

    #[test] // spec: QA-020
    fn a_refused_review_only_retry_approves_nothing() {
        let fake = pr_with_guard_run();
        let exchange = Exchange {
            refuse: vec![
                (TokenScope::ReviewAndRerun, 422),
                (TokenScope::ReviewOnly, 403),
            ],
            ..Exchange::default()
        };
        let e = err_text(run(&fake, &approved(), &exchange, SUMMARY, HEAD));
        assert!(e.contains("nothing was approved"), "{e}");
        assert!(e.contains("403"), "{e}");
        assert_eq!(
            *exchange.scopes.borrow(),
            vec![TokenScope::ReviewAndRerun, TokenScope::ReviewOnly]
        );
        assert!(fake.posted_reviews().is_empty());
        assert!(exchange.revoked.borrow().is_empty(), "no token to revoke");
    }

    #[test] // spec: QA-020
    fn a_failed_post_reruns_nothing() {
        let fake = pr_with_guard_run();
        fake.set_head(PR, NEW);
        let exchange = Exchange::default();
        let e = err_text(run(&fake, &approved(), &exchange, SUMMARY, HEAD));
        assert_eq!(e, HEAD_MOVED);
        assert!(fake.run_queries().is_empty());
        assert!(fake.reruns().is_empty());
        assert_eq!(*exchange.revoked.borrow(), vec![TOKEN.to_string()]);
    }

    fn approve_via(
        gh: &dyn GitHubApi,
        exchange: &Exchange,
        pause: &dyn Fn(Duration),
    ) -> Result<Rerun> {
        approve(
            PR,
            &cfg(),
            gh,
            &approved(),
            &key_file(),
            None,
            exchange,
            SUMMARY,
            NOW,
            HEAD,
            pause,
        )
    }

    #[test] // spec: QA-020
    fn a_head_moved_after_the_approval_reruns_nothing() {
        // A push lands right after the review: the approval no longer counts, so nothing of the
        // old head is re-run.
        let fake = pr_with_guard_run();
        let pushed = || fake.set_head(PR, NEW);
        let hooked = Hooked {
            on_post: Some(&pushed),
            ..Hooked::new(&fake)
        };
        let w = warning(approve_via(&hooked, &Exchange::default(), &|_| {}).unwrap());
        assert!(w.contains("head moved"), "{w}");
        assert!(w.contains("lil dev approve 42"), "{w}");
        assert!(fake.reruns().is_empty());
        assert_eq!(fake.posted_reviews().len(), 1);
    }

    #[test] // spec: QA-020
    fn a_head_moved_while_waiting_reruns_nothing() {
        let fake = open_pr();
        add_guard_run(&fake, guard_run(12, HEAD, "in_progress", None));
        let pause = |_: Duration| {
            fake.set_run_status(12, "completed", Some("failure"));
            fake.set_head(PR, NEW);
        };
        let w = warning(
            run_with_pause(
                &fake,
                &approved(),
                &Exchange::default(),
                SUMMARY,
                HEAD,
                &pause,
            )
            .unwrap(),
        );
        assert!(w.contains("head moved"), "{w}");
        assert!(fake.reruns().is_empty());
    }

    #[test] // spec: QA-020
    fn a_failed_pr_reread_warns_and_succeeds() {
        let fake = pr_with_guard_run();
        let hooked = Hooked {
            pr_fails_after_post: true,
            ..Hooked::new(&fake)
        };
        let exchange = Exchange::default();
        let w = warning(approve_via(&hooked, &exchange, &|_| {}).unwrap());
        assert!(w.starts_with(WARNING_START), "{w}");
        assert!(w.contains("re-reading PR #42 failed"), "{w}");
        assert!(w.ends_with(CHECKS_URL), "{w}");
        assert!(fake.reruns().is_empty());
        assert_eq!(fake.posted_reviews().len(), 1);
        assert_eq!(*exchange.revoked.borrow(), vec![TOKEN.to_string()]);
    }

    #[test] // spec: QA-020
    fn the_token_is_revoked_even_when_posting_or_rerunning_panics() {
        let boom = || panic!("simulated crash");
        let post_panics = pr_with_guard_run();
        let rerun_panics = pr_with_guard_run();
        for (fake, hooked) in [
            (
                &post_panics,
                Hooked {
                    on_post: Some(&boom),
                    ..Hooked::new(&post_panics)
                },
            ),
            (
                &rerun_panics,
                Hooked {
                    on_rerun: Some(&boom),
                    ..Hooked::new(&rerun_panics)
                },
            ),
        ] {
            let exchange = Exchange::default();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                approve_via(&hooked, &exchange, &|_| {})
            }));
            assert!(result.is_err(), "expected the simulated crash");
            assert_eq!(*exchange.revoked.borrow(), vec![TOKEN.to_string()]);
            assert!(fake.reruns().is_empty());
        }
    }

    #[test] // spec: QA-020
    fn revoke_on_drop_revokes_once_and_only_warns_on_failure() {
        for fail_revoke in [false, true] {
            let exchange = Exchange {
                fail_revoke,
                ..Exchange::default()
            };
            let guard = RevokeOnDrop::new(&exchange, TOKEN.to_string());
            assert_eq!(guard.secret(), TOKEN);
            assert!(exchange.revoked.borrow().is_empty());
            drop(guard);
            assert_eq!(*exchange.revoked.borrow(), vec![TOKEN.to_string()]);
        }
    }

    /// Forwards to the fake, with hooks: `on_post` runs right after the review is posted,
    /// `on_rerun` right before a re-run is requested, and `pr` fails once a review is posted
    /// when `pr_fails_after_post` is set.
    struct Hooked<'a> {
        fake: &'a FakeGitHub,
        on_post: Option<&'a dyn Fn()>,
        on_rerun: Option<&'a dyn Fn()>,
        pr_fails_after_post: bool,
        posted: std::cell::Cell<bool>,
    }

    impl<'a> Hooked<'a> {
        fn new(fake: &'a FakeGitHub) -> Self {
            Self {
                fake,
                on_post: None,
                on_rerun: None,
                pr_fails_after_post: false,
                posted: std::cell::Cell::new(false),
            }
        }
    }

    impl GitHubApi for Hooked<'_> {
        fn pr(&self, n: u64) -> Result<PrInfo> {
            if self.pr_fails_after_post && self.posted.get() {
                bail!("HTTP 502: Bad Gateway");
            }
            self.fake.pr(n)
        }
        fn check(&self, sha: &str, name: &str) -> Result<crate::dev::github::CheckState> {
            self.fake.check(sha, name)
        }
        fn reviews(&self, n: u64) -> Result<Vec<crate::dev::github::Review>> {
            self.fake.reviews(n)
        }
        fn merge(&self, n: u64, sha: &str) -> Result<crate::dev::github::MergeOutcome> {
            self.fake.merge(n, sha)
        }
        fn post_review_as_app(&self, n: u64, sha: &str, body: &str, token: &str) -> Result<()> {
            let posted = self.fake.post_review_as_app(n, sha, body, token);
            self.posted.set(true);
            if let Some(f) = self.on_post {
                f();
            }
            posted
        }
        fn commit_pulls(&self, sha: &str) -> Result<Vec<crate::dev::github::CommitPull>> {
            self.fake.commit_pulls(sha)
        }
        fn workflow_runs_as_app(
            &self,
            workflow: &str,
            event: &str,
            head_sha: &str,
            token: &str,
        ) -> Result<Vec<WorkflowRun>> {
            self.fake
                .workflow_runs_as_app(workflow, event, head_sha, token)
        }
        fn rerun_as_app(&self, run_id: u64, token: &str) -> Result<()> {
            if let Some(f) = self.on_rerun {
                f();
            }
            self.fake.rerun_as_app(run_id, token)
        }
        fn app_slug(&self, jwt: &str) -> Result<String> {
            self.fake.app_slug(jwt)
        }
    }

    #[test] // spec: QA-020
    fn rerun_warning_is_one_safe_line() {
        let w = rerun_warning(
            &anyhow!("first line\nsecond {TOKEN} \u{1b}[2J"),
            CHECKS_URL,
            TOKEN,
        );
        assert!(!w.contains('\n'), "{w}");
        assert!(!w.contains('\u{1b}'), "{w}");
        assert!(!w.contains(TOKEN), "{w}");
        assert!(w.starts_with(WARNING_START), "{w}");
        assert!(
            w.ends_with(&format!("). Re-run it in the browser: {CHECKS_URL}")),
            "{w}"
        );
    }

    // ---- CLI glue: the summary is judged on the PR's head against origin/main ------------------

    fn git_t(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .output()
            .expect("git must be installed");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn write_t(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn commit_t(dir: &Path, msg: &str) -> String {
        git_t(dir, &["add", "-A"]);
        git_t(dir, &["commit", "-q", "-m", msg]);
        git_t(dir, &["rev-parse", "HEAD"])
    }

    /// A work repo with a local bare origin. `main` commits the settings and protects
    /// `crates/lil-spec-tests/**`. PR 42 edits a spec test; PR 43 edits only a README.
    /// Returns (tempdir, work repo, PR 42 head, PR 43 head).
    fn glue_fixture() -> (tempfile::TempDir, PathBuf, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        git_t(
            tmp.path(),
            &[
                "-c",
                "init.defaultBranch=main",
                "init",
                "--bare",
                "-q",
                "origin.git",
            ],
        );
        std::fs::create_dir_all(&work).unwrap();
        git_t(&work, &["-c", "init.defaultBranch=main", "init", "-q"]);
        for (k, v) in [
            ("user.name", "Approve Test"),
            ("user.email", "approve@test.invalid"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            git_t(&work, &["config", k, v]);
        }
        write_t(&work, CONFIG_PATH, COMMITTED);
        write_t(
            &work,
            "dev/protected-paths.txt",
            "crates/lil-spec-tests/**\ndev/merge-guard.toml\n",
        );
        write_t(
            &work,
            "crates/lil-spec-tests/tests/smoke.rs",
            "#[test]\nfn spec_harness_runs() {}\n",
        );
        write_t(&work, "README.md", "hello\n");
        commit_t(&work, "base");
        let origin = tmp.path().join("origin.git");
        git_t(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git_t(&work, &["push", "-q", "origin", "main"]);
        git_t(&work, &["fetch", "-q", "origin"]);

        // The PR heads are made in a second clone, so the work repo only has them once the
        // glue fetches them.
        let other = tmp.path().join("other");
        git_t(
            tmp.path(),
            &[
                "clone",
                "-q",
                "-b",
                "main",
                origin.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        for (k, v) in [
            ("user.name", "Approve Test"),
            ("user.email", "approve@test.invalid"),
            ("commit.gpgsign", "false"),
        ] {
            git_t(&other, &["config", k, v]);
        }
        git_t(&other, &["checkout", "-q", "-b", "protected"]);
        write_t(
            &other,
            "crates/lil-spec-tests/tests/smoke.rs",
            "#[test]\nfn spec_harness_runs() { assert!(true) }\n",
        );
        let protected = commit_t(&other, "touch a spec test");
        git_t(&other, &["push", "-q", "origin", "HEAD:refs/pull/42/head"]);

        git_t(&other, &["checkout", "-q", "main"]);
        git_t(&other, &["checkout", "-q", "-b", "docs"]);
        write_t(&other, "README.md", "hello again\n");
        let docs = commit_t(&other, "docs only");
        git_t(&other, &["push", "-q", "origin", "HEAD:refs/pull/43/head"]);
        (tmp, work, protected, docs)
    }

    /// Lends a `&FakeGitHub` as a boxed `GitHubApi`.
    struct FakeRef<'a>(&'a FakeGitHub);

    impl GitHubApi for FakeRef<'_> {
        fn pr(&self, n: u64) -> Result<PrInfo> {
            self.0.pr(n)
        }
        fn check(&self, sha: &str, name: &str) -> Result<crate::dev::github::CheckState> {
            self.0.check(sha, name)
        }
        fn reviews(&self, n: u64) -> Result<Vec<crate::dev::github::Review>> {
            self.0.reviews(n)
        }
        fn merge(&self, n: u64, sha: &str) -> Result<crate::dev::github::MergeOutcome> {
            self.0.merge(n, sha)
        }
        fn post_review_as_app(&self, n: u64, sha: &str, body: &str, token: &str) -> Result<()> {
            self.0.post_review_as_app(n, sha, body, token)
        }
        fn commit_pulls(&self, sha: &str) -> Result<Vec<crate::dev::github::CommitPull>> {
            self.0.commit_pulls(sha)
        }
        fn workflow_runs_as_app(
            &self,
            workflow: &str,
            event: &str,
            head_sha: &str,
            token: &str,
        ) -> Result<Vec<WorkflowRun>> {
            self.0
                .workflow_runs_as_app(workflow, event, head_sha, token)
        }
        fn rerun_as_app(&self, run_id: u64, token: &str) -> Result<()> {
            self.0.rerun_as_app(run_id, token)
        }
        fn app_slug(&self, jwt: &str) -> Result<String> {
            self.0.app_slug(jwt)
        }
    }

    #[test]
    fn summary_is_judged_on_the_pr_head_against_origin_main() {
        let (_tmp, work, protected, docs) = glue_fixture();
        let fake = FakeGitHub::new();
        fake.add_pr(42, &protected, "main");
        fake.add_pr(43, &docs, "main");
        let github = |_: &MergeGuardConfig| -> Result<Box<dyn GitHubApi + '_>> {
            Ok(Box::new(FakeRef(&fake)))
        };

        let p = prepare_approval(&work, 42, &cfg(), &github).unwrap();
        assert_eq!(p.head, protected);
        assert_eq!(p.cfg, cfg());
        assert!(
            p.summary.contains("crates/lil-spec-tests/tests/smoke.rs"),
            "{}",
            p.summary
        );
        assert_eq!(p.gh.pr(42).unwrap().head_sha, protected);

        let e = format!(
            "{:#}",
            prepare_approval(&work, 43, &cfg(), &github).err().unwrap()
        );
        assert!(e.contains(NOTHING_PROTECTED), "{e}");

        // A decoy local branch `origin/main` at the PR head would make the PR look empty if the
        // base were resolved by its short name.
        git_t(
            &work,
            &[
                "fetch",
                "-q",
                "origin",
                "refs/pull/42/head:refs/heads/origin/main",
            ],
        );
        let p = prepare_approval(&work, 42, &cfg(), &github).unwrap();
        assert!(
            p.summary.contains("crates/lil-spec-tests/tests/smoke.rs"),
            "decoy base won: {}",
            p.summary
        );

        fake.set_draft(42, true);
        let e = format!(
            "{:#}",
            prepare_approval(&work, 42, &cfg(), &github).err().unwrap()
        );
        assert!(e.contains("draft"), "{e}");
        assert!(fake.posted_reviews().is_empty());
    }
}
