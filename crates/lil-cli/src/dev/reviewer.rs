//! `lil dev reviewer-setup` and `lil dev approve` (QA-020 as amended).
//!
//! Changes to protected files merge only with an APPROVED review from the reviewer GitHub App
//! (`worldwright-reviewer`) on the PR's exact head commit. This module is the only thing that
//! posts that review, and it does so only for the owner, at a real terminal, after the owner has
//! read the guard's summary, said yes, and typed the passphrase that unlocks the App's key.
//! Agents have no terminal, so they can never approve.
//!
//! **The key.** `lil dev reviewer-setup` encrypts the App's downloaded `.pem` with an age scrypt
//! passphrase and writes it, with the App id and installation id, to
//! `%USERPROFILE%\<home>\reviewer\app-key.age` ([`key_path_in`]), and never to the legacy
//! `%USERPROFILE%\<legacy home>\reviewer\app-key.age` ([`legacy_key_path_in`]), where keys were
//! written before the rename. `approve` reads the key from the new path when it exists, else
//! from the legacy one ([`find_key_in`], R4), and prints which. Both paths are outside the repo
//! and outside the suite's `%APPDATA%\<App>*` / `%LOCALAPPDATA%\<App>*` data, and the repo's
//! `.claude` deny rules keep agents out of both home folders. The plaintext `.pem` is never
//! deleted or modified here; the owner deletes it.
//!
//! **Approving** ([`approve`]): refuse unless at a terminal; refuse when nothing protected
//! changed; show the summary and ask "(y/N)"; read the passphrase; decrypt the key (a wrong
//! passphrase stops here, before any network call); mint a 9-minute RS256 App JWT
//! ([`app_jwt`]); exchange it for an installation token; re-read the PR and refuse unless it is
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

use super::github::{API_BASE, GitHubApi, PrInfo, WorkflowRun};
use super::guard;
use super::merge::{
    self, MergeGuardConfig, blocker_lines, check_origin, committed_config, explain, fetch,
    is_commit_sha, one_line, short,
};
use age::secrecy::SecretString;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use lil_platform::names;
use reqwest::Method;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroize::Zeroizing;

/// The shortest passphrase `reviewer-setup` accepts, in characters.
pub const MIN_PASSPHRASE_CHARS: usize = 12;
/// scrypt work factor (`N = 2^18`) for the stored key: fixed, not calibrated, so a slow or debug
/// build never writes a weaker file.
pub const WORK_FACTOR: u8 = 18;
/// The largest work factor a key file may demand when it is decrypted.
pub const MAX_WORK_FACTOR: u8 = 20;

pub const NOT_INTERACTIVE: &str = "lil dev approve only runs at a real terminal (stdin and stderr must both be one); nothing was approved";
pub const WRONG_PASSPHRASE: &str = "wrong passphrase; nothing was posted";
pub const HEAD_MOVED: &str = "the PR head moved since the summary; nothing was approved";
pub const NOTHING_PROTECTED: &str = "nothing protected to approve";
pub const DAMAGED_KEY: &str = "the key file is damaged or not an age passphrase file";

/// Why a key file could not be decrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    WrongPassphrase,
    Damaged,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KeyError::WrongPassphrase => "wrong passphrase",
            KeyError::Damaged => DAMAGED_KEY,
        })
    }
}

impl std::error::Error for KeyError {}

const SETUP_NOT_INTERACTIVE: &str = "lil dev reviewer-setup only runs at a real terminal (stdin and stderr must both be one); nothing was written";

// ---------------------------------------------------------------------------------------------
// The key file.
// ---------------------------------------------------------------------------------------------

/// The reviewer App's credentials as stored on disk. `encrypted_pem` is an age (scrypt) file;
/// on disk it is TOML with that field base64-encoded.
#[derive(Clone, PartialEq, Eq)]
pub struct ReviewerKeyFile {
    pub app_id: u64,
    pub installation_id: u64,
    pub encrypted_pem: Vec<u8>,
}

impl fmt::Debug for ReviewerKeyFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReviewerKeyFile")
            .field("app_id", &self.app_id)
            .field("installation_id", &self.installation_id)
            .field(
                "encrypted_pem",
                &format_args!("<redacted, {} bytes>", self.encrypted_pem.len()),
            )
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyFileToml {
    app_id: u64,
    installation_id: u64,
    encrypted_pem: String,
}

impl ReviewerKeyFile {
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string(&KeyFileToml {
            app_id: self.app_id,
            installation_id: self.installation_id,
            encrypted_pem: STANDARD.encode(&self.encrypted_pem),
        })
        .context("writing the reviewer key file")
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        let raw: KeyFileToml = toml::from_str(text).context("parsing the reviewer key file")?;
        if raw.app_id == 0 || raw.installation_id == 0 {
            bail!("the reviewer key file has an App id or installation id of 0");
        }
        let encrypted_pem = STANDARD
            .decode(raw.encrypted_pem.trim())
            .context("the reviewer key file's `encrypted_pem` is not base64")?;
        Ok(Self {
            app_id: raw.app_id,
            installation_id: raw.installation_id,
            encrypted_pem,
        })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the reviewer key file {}", path.display()))?;
        Self::from_toml(&text).with_context(|| format!("in {}", path.display()))
    }

    /// Writes the file (creating its folder) through a temporary file and a rename, so a failed
    /// write never leaves half a key file behind.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path
            .parent()
            .ok_or_else(|| anyhow!("{} has no parent folder", path.display()))?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let text = self.to_toml()?;
        let tmp = path.with_extension("age.tmp");
        let written = (|| -> Result<()> {
            let mut file = std::fs::File::create(&tmp)
                .with_context(|| format!("creating {}", tmp.display()))?;
            file.write_all(text.as_bytes())
                .with_context(|| format!("writing {}", tmp.display()))?;
            file.sync_all()
                .with_context(|| format!("flushing {} to disk", tmp.display()))?;
            drop(file);
            std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written
    }
}

/// `<profile>\<home>\reviewer\app-key.age` ([`names::HOME_DIR`]): where `reviewer-setup`
/// writes the key.
pub fn key_path_in(profile: &Path) -> PathBuf {
    profile
        .join(names::HOME_DIR)
        .join("reviewer")
        .join("app-key.age")
}

/// `<profile>\<legacy home>\reviewer\app-key.age` ([`names::LEGACY_HOME_DIR`]): where the key
/// was written before the rename. Read when the new path has no key (R4).
pub fn legacy_key_path_in(profile: &Path) -> PathBuf {
    profile
        .join(names::LEGACY_HOME_DIR)
        .join("reviewer")
        .join("app-key.age")
}

/// The reviewer key to use: [`key_path_in`] when anything is there, else
/// [`legacy_key_path_in`] when anything is there, else `None`. "Anything" means whatever isn't
/// a plain not-found: a file, a folder, a link, or a path that can't be examined. So an occupied
/// or unreadable new path is chosen, and loading it refuses; the legacy key is used only when
/// nothing at all is at the new path. Only looks; nothing is created.
pub fn find_key_in(profile: &Path) -> Option<PathBuf> {
    [key_path_in(profile), legacy_key_path_in(profile)]
        .into_iter()
        .find(|p| {
            !matches!(std::fs::symlink_metadata(p), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        })
}

/// [`find_key_in`], or an error naming both paths it looked at.
fn locate_key_in(profile: &Path) -> Result<PathBuf> {
    find_key_in(profile).ok_or_else(|| {
        anyhow!(
            "there is no reviewer key at {} or at the legacy {}; run `lil dev reviewer-setup` first",
            key_path_in(profile).display(),
            legacy_key_path_in(profile).display()
        )
    })
}

/// The key file [`locate_key_in`] finds, loaded, with its path. A key that can't be read or
/// parsed is an error naming its path, never a reason to try the other one.
fn load_key_in(profile: &Path) -> Result<(PathBuf, ReviewerKeyFile)> {
    let path = locate_key_in(profile)?;
    let key = ReviewerKeyFile::load(&path)?;
    Ok((path, key))
}

/// Where `reviewer-setup` writes the key: [`setup_key_path_in`] for `%USERPROFILE%`.
pub fn setup_key_path() -> Result<PathBuf> {
    Ok(setup_key_path_in(&user_profile()?))
}

/// Where `reviewer-setup` writes the key for `profile`: only ever [`key_path_in`], the new home
/// folder. A key in the legacy home folder is left alone; `approve` still reads it while the new
/// path has none.
pub fn setup_key_path_in(profile: &Path) -> PathBuf {
    key_path_in(profile)
}

/// `%USERPROFILE%`, which must be set and absolute.
fn user_profile() -> Result<PathBuf> {
    let profile = std::env::var_os("USERPROFILE")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            anyhow!("USERPROFILE is not set, so there is no place for the reviewer key")
        })?;
    if !profile.is_absolute() {
        bail!(
            "USERPROFILE ({}) is not an absolute path",
            profile.display()
        );
    }
    Ok(profile)
}

// ---------------------------------------------------------------------------------------------
// Encryption and the App JWT.
// ---------------------------------------------------------------------------------------------

/// Encrypts `pem` to `passphrase` with age's scrypt recipient at [`WORK_FACTOR`]. Only tests use
/// it; setup calls [`encrypt_key_with_work_factor`] directly.
#[cfg(test)]
pub fn encrypt_key(pem: &[u8], passphrase: &str) -> Result<Vec<u8>> {
    encrypt_key_with_work_factor(pem, passphrase, WORK_FACTOR)
}

/// Encrypts `pem` to `passphrase` with age's scrypt recipient at the scrypt work factor
/// `log_n` ([`WORK_FACTOR`] for real keys; tests use a cheap one).
pub fn encrypt_key_with_work_factor(pem: &[u8], passphrase: &str, log_n: u8) -> Result<Vec<u8>> {
    let mut recipient = age::scrypt::Recipient::new(SecretString::from(passphrase.to_owned()));
    recipient.set_work_factor(log_n);
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .context("setting up the encryption")?;
    let mut out = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut out)
        .context("encrypting the key")?;
    writer.write_all(pem).context("encrypting the key")?;
    writer.finish().context("encrypting the key")?;
    Ok(out)
}

/// The scrypt work factor in an age header's `-> scrypt <salt> <log_n>` stanza.
fn header_work_factor(blob: &[u8]) -> Option<u8> {
    let end = blob.windows(4).position(|w| w == b"\n---")?;
    std::str::from_utf8(&blob[..end])
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("-> scrypt "))?
        .split(' ')
        .nth(1)?
        .parse()
        .ok()
}

/// Errors with [`KeyError::Damaged`] unless `blob` is a passphrase-encrypted age file whose work
/// factor is at most [`MAX_WORK_FACTOR`]. Needs no passphrase, so it runs before the prompt.
fn check_encrypted(blob: &[u8]) -> std::result::Result<age::Decryptor<&[u8]>, KeyError> {
    let decryptor = age::Decryptor::new_buffered(blob).map_err(|_| KeyError::Damaged)?;
    if !decryptor.is_scrypt() {
        return Err(KeyError::Damaged);
    }
    match header_work_factor(blob) {
        Some(log_n) if log_n <= MAX_WORK_FACTOR => Ok(decryptor),
        _ => Err(KeyError::Damaged),
    }
}

/// Decrypts a blob made by [`encrypt_key_with_work_factor`]. The error is a [`KeyError`]:
/// `WrongPassphrase` when the passphrase doesn't unlock it, `Damaged` for a malformed or
/// truncated file or one that demands more than [`MAX_WORK_FACTOR`]. Neither carries any secret.
pub fn decrypt_key(blob: &[u8], passphrase: &str) -> Result<Vec<u8>> {
    let decryptor = check_encrypted(blob)?;
    let mut identity = age::scrypt::Identity::new(SecretString::from(passphrase.to_owned()));
    identity.set_max_work_factor(MAX_WORK_FACTOR);
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| match e {
            age::DecryptError::DecryptionFailed
            | age::DecryptError::KeyDecryptionFailed
            | age::DecryptError::NoMatchingKeys => KeyError::WrongPassphrase,
            _ => KeyError::Damaged,
        })?;
    // Sized up front so the plaintext isn't left behind in reallocated buffers.
    let mut pem = Zeroizing::new(Vec::with_capacity(blob.len()));
    reader
        .read_to_end(&mut pem)
        .map_err(|_| KeyError::Damaged)?;
    Ok(std::mem::take(&mut *pem))
}

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
/// refusal posts nothing.
#[allow(
    clippy::too_many_arguments,
    reason = "the brief's signature plus expected_head (ruling R20) and pause (R40)"
)]
pub fn approve(
    pr: u64,
    cfg: &MergeGuardConfig,
    gh: &dyn GitHubApi,
    prompt: &dyn Prompt,
    key_file: &ReviewerKeyFile,
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

    let damaged =
        || anyhow!("{DAMAGED_KEY}; run `lil dev reviewer-setup` again; nothing was posted");
    // A damaged file is refused before the owner types the passphrase.
    check_encrypted(&key_file.encrypted_pem).map_err(|_| damaged())?;
    let passphrase = Zeroizing::new(prompt.passphrase("Reviewer key passphrase: "));
    // Decrypt before anything touches the network: a wrong passphrase stops here.
    let pem = Zeroizing::new(
        decrypt_key(&key_file.encrypted_pem, &passphrase).map_err(|e| {
            match e.downcast_ref::<KeyError>() {
                Some(KeyError::WrongPassphrase) => anyhow!(WRONG_PASSPHRASE),
                _ => damaged(),
            }
        })?,
    );
    drop(passphrase);
    let jwt = Zeroizing::new(app_jwt(key_file.app_id, &pem, now_unix)?);
    drop(pem);
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
    let key_file = match user_profile().and_then(|profile| load_key_in(&profile)) {
        Ok((path, key)) => {
            println!("Reviewer key: {}", path.display());
            key
        }
        Err(e) => return refuse(&format!("{e:#}")),
    };
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
        &GitHubAppTokens,
        &prepared.summary,
        unix_now(),
        &prepared.head,
        &std::thread::sleep,
    ) {
        Ok(rerun) => {
            println!(
                "Approved PR #{pr} at {} as {}.",
                short(&prepared.head),
                prepared.cfg.reviewer_login()
            );
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
            0
        }
        Err(e) => refuse(&format!("{e:#}")),
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
    }

    impl FakePrompt {
        fn new() -> Self {
            Self {
                interactive: true,
                lines: RefCell::default(),
                passphrases: RefCell::default(),
                confirms: RefCell::default(),
                asked: RefCell::default(),
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
            head_repo: "xenofei/Worldwright-Suite".into(),
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

    const CHECKS_URL: &str = "https://github.com/xenofei/Worldwright-Suite/pull/42/checks";
    const RUN_12_URL: &str = "https://github.com/xenofei/Worldwright-Suite/actions/runs/12";
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
        fake.add_review(
            PR,
            "worldwright-reviewer[bot]",
            "APPROVED",
            "an-older-commit",
        );
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
        assert_eq!(
            *exchange.repos.borrow(),
            vec!["Worldwright-Suite".to_string()]
        );
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
        assert_eq!(
            legacy_key_path_in(profile),
            profile
                .join(names::LEGACY_HOME_DIR)
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
        for path in [key_path_in(profile), legacy_key_path_in(profile)] {
            assert!(!path.starts_with(&repo), "{path:?} is inside the repo");
            // Windows puts both app-data roots inside the profile; check that layout, injected.
            for root in [
                profile.join("AppData").join("Roaming"),
                profile.join("AppData").join("Local"),
            ] {
                assert!(!under_suite_data(&path, &root), "{path:?} under {root:?}");
            }
            // And against this machine's roots (the variables' values only; nothing under them
            // is resolved or touched).
            for var in ["APPDATA", "LOCALAPPDATA"] {
                if let Some(root) = std::env::var_os(var) {
                    assert!(
                        !under_suite_data(&path, Path::new(&root)),
                        "{path:?} under %{var}%"
                    );
                }
            }
            // The home folder itself, under either name, is not a suite data root.
            assert!(!under_suite_data(&path, profile));
        }
    }

    #[test]
    fn key_lookup_prefers_the_new_path() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path();
        for p in [key_path_in(profile), legacy_key_path_in(profile)] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "key").unwrap();
        }
        assert_eq!(find_key_in(profile), Some(key_path_in(profile)));
        assert_eq!(locate_key_in(profile).unwrap(), key_path_in(profile));
        // Only the new one: still the new one.
        std::fs::remove_file(legacy_key_path_in(profile)).unwrap();
        assert_eq!(find_key_in(profile), Some(key_path_in(profile)));
    }

    #[test]
    fn key_lookup_finds_the_legacy_path() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path();
        let legacy = legacy_key_path_in(profile);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "key").unwrap();
        assert_eq!(find_key_in(profile), Some(legacy.clone()));
        assert_eq!(locate_key_in(profile).unwrap(), legacy);
        // Looking never creates the new folder.
        assert!(!profile.join(names::HOME_DIR).exists());
    }

    #[test]
    fn missing_key_message_names_both_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path();
        assert_eq!(find_key_in(profile), None);
        let text = format!("{:#}", locate_key_in(profile).unwrap_err());
        assert!(
            text.contains(&key_path_in(profile).display().to_string()),
            "{text}"
        );
        assert!(
            text.contains(&legacy_key_path_in(profile).display().to_string()),
            "{text}"
        );
        assert!(text.contains("lil dev reviewer-setup"), "{text}");
        assert!(load_key_in(profile).is_err());
    }

    /// Anything at the new key path is the key or a refusal; the legacy key is used only when
    /// nothing is there at all.
    #[test]
    fn occupied_new_key_path_refuses_instead_of_falling_back() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path();
        let legacy = legacy_key_path_in(profile);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        key_file().save(&legacy).unwrap();
        assert_eq!(load_key_in(profile).unwrap().0, legacy);

        // A folder at the new path: chosen, and loading it refuses.
        let new = key_path_in(profile);
        std::fs::create_dir_all(&new).unwrap();
        assert_eq!(find_key_in(profile), Some(new.clone()));
        let text = format!("{:#}", load_key_in(profile).unwrap_err());
        assert!(text.contains(&new.display().to_string()), "{text}");
        std::fs::remove_dir(&new).unwrap();

        // A damaged file at the new path: the same.
        std::fs::write(&new, "not a key file").unwrap();
        assert_eq!(find_key_in(profile), Some(new.clone()));
        let text = format!("{:#}", load_key_in(profile).unwrap_err());
        assert!(text.contains(&new.display().to_string()), "{text}");

        // A good file at the new path wins over the legacy one.
        key_file().save(&new).unwrap();
        assert_eq!(load_key_in(profile).unwrap().0, new);
        // The legacy key was never touched.
        assert!(ReviewerKeyFile::load(&legacy).is_ok());
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
        let p = dir.join("worldwright-reviewer.2026-10-13.private-key.pem");
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
    fn files_under(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    todo.push(path);
                } else {
                    let bytes = std::fs::read(&path).unwrap();
                    out.push((path, bytes));
                }
            }
        }
        out.sort();
        out
    }

    /// Task B5: `reviewer-setup` writes the key only under the new home folder. A key in the
    /// legacy home folder is left byte for byte, nothing is created there, and `approve` then
    /// finds the new key.
    #[test]
    fn reviewer_setup_writes_only_the_new_key_path() {
        let tmp = tempfile::tempdir().unwrap();
        let pem_path = write_pem(tmp.path(), &test_key().pkcs1_pem);
        let pem_answer = pem_path.display().to_string();
        for legacy_key_exists in [false, true] {
            let profile = tmp.path().join(format!("profile-{legacy_key_exists}"));
            std::fs::create_dir_all(&profile).unwrap();
            let legacy_home = profile.join(names::LEGACY_HOME_DIR);
            let legacy = legacy_key_path_in(&profile);
            if legacy_key_exists {
                std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
                std::fs::write(&legacy, b"legacy key bytes").unwrap();
            }
            let before = files_under(&legacy_home);

            let path = setup_key_path_in(&profile);
            assert_eq!(path, key_path_in(&profile));
            let prompt = FakePrompt::new()
                .lines(&["123", "456", &pem_answer])
                .passphrases(&[PASS, PASS]);
            setup_with_work_factor(&prompt, &path, TEST_WORK_FACTOR).unwrap();

            let saved = ReviewerKeyFile::load(&key_path_in(&profile)).unwrap();
            assert_eq!(saved.app_id, 123);
            assert_eq!(files_under(&legacy_home), before, "legacy folder changed");
            if legacy_key_exists {
                assert_eq!(std::fs::read(&legacy).unwrap(), b"legacy key bytes");
            } else {
                assert!(!legacy_home.exists(), "{legacy_home:?} was created");
            }
            assert_eq!(find_key_in(&profile), Some(key_path_in(&profile)));
        }
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
        let (m, path, body) =
            build_token_request(456, "Worldwright-Suite", TokenScope::ReviewAndRerun);
        assert_eq!(m, reqwest::Method::POST);
        assert_eq!(path, "/app/installations/456/access_tokens");
        assert_eq!(
            body,
            serde_json::json!({
                "repositories": ["Worldwright-Suite"],
                "permissions": {"pull_requests": "write", "actions": "write"}
            })
        );
        // The fallback for an installation that hasn't granted Actions write yet.
        let (_, _, body) = build_token_request(456, "Worldwright-Suite", TokenScope::ReviewOnly);
        assert_eq!(
            body,
            serde_json::json!({
                "repositories": ["Worldwright-Suite"],
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
                head_repo: "someone/Worldwright-Suite".into(),
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
                head_repo: "someone/Worldwright-Suite".into(),
                ..guard_run(4, HEAD, "completed", Some("failure"))
            },
            WorkflowRun {
                path: ".github/workflows/ci-guard-copy.yml".into(),
                ..guard_run(6, HEAD, "completed", Some("failure"))
            },
            WorkflowRun {
                head_repo: "XENOFEI/worldwright-suite".into(),
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
        assert_eq!(
            *exchange.repos.borrow(),
            vec!["Worldwright-Suite".to_string()]
        );
        assert_eq!(
            TokenScope::ReviewAndRerun.permissions(),
            serde_json::json!({"pull_requests": "write", "actions": "write"})
        );
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
            ["post_review", "workflow_runs", "rerun 12", "revoke"]
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
                head_repo: "someone/Worldwright-Suite".into(),
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
