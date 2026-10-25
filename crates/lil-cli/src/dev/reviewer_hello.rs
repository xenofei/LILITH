//! `lil dev reviewer-hello enroll | status | remove`: Windows Hello for the reviewer key
//! (QA-088, P4a-R23, P4a-R38; plan 4a Task 28), and the authentication every approval and every
//! merge asks for (QA-036).
//!
//! **The one route: the KEK** (Task 28 fix round 1: the DPAPI route is not built, because a
//! DPAPI-protected key is readable without a prompt by every program running as you, agents
//! included). The files, beside the passphrase file `app-key.age`, which always stays as the
//! fallback:
//!
//! - `app-key.hello.toml`: `route = "kek"`, `credential = "LILITH-reviewer-kek"`,
//!   `challenge = "<64 lowercase hex>"` ([`HelloFile`]). Written last, so it marks a finished
//!   enrolment. Any other route in it makes the file damaged.
//! - `app-key.hello.age`: the PEM under age's scrypt, the passphrase the lowercase hex of the
//!   KEK: SHA-256 over `LILITH reviewer KEK v1` and the Windows Hello key credential's signature
//!   over the challenge (P4a-R23). The existing age decryption stays the one path.
//!
//! **The seam.** The Windows Hello calls go through [`HelloPlatform`]: [`WindowsHello`] for real
//! (`lil_platform::hello`), a fake in `tests/reviewer_hello.rs`. [`PROVEN_ROUTE`] is the KEK,
//! proven by Task 28's route step (Step 0, P4a-R38) on 2026-10-24.
//!
//! **Not with face sign-in** (owner decision 2026-10-24). Windows Hello unlocks the key only
//! while face recognition isn't enrolled for Windows Hello on this account
//! ([`face_blocks_hello`]), so the prompt always needs a deliberate PIN or fingerprint. While it
//! is on, or can't be told, the passphrase is used and you are told why; `enroll` refuses, and
//! `status` shows it. Found on, LILITH also removes its own enrolment ([`face_gate`]; Phase B
//! review I1): with face sign-in on, any program running as you could open it with your face.
//! The check runs when the enrolment is read and again right before each prompt that signs with
//! the key; enrolment's create prompt is followed by such a re-checked sign.
//!
//! **In front first (QA-088, Phase B ruling).** The window that owns the Windows Hello prompt
//! shows what you are confirming (for a merge, the summary and the audit, P4a-R38). When
//! Windows keeps it behind, it flashes in the taskbar and waits, within
//! `lil_platform::hello::HELLO_WAIT`, for you to bring it forward; it doesn't fall back to the
//! passphrase for that. Not brought forward in time is a refusal for a merge or an approval
//! ([`KeyError::Denied`]). A prompt that isn't answered in time, or that fails, makes Windows
//! Hello unavailable, and the passphrase is asked for, or, with no terminal ([`NoTerminal`]),
//! nothing is unlocked.
//!
//! **Merge or Deny first** (Task 28 fix round 2). Windows Hello Face can complete a prompt with
//! no deliberate act, so for an approval or a merge that window has two buttons, "Merge" (or
//! "Approve") and "Deny", and Windows Hello is raised only after you clicked the first. "Deny",
//! closing the window or no choice within `HELLO_WAIT` refuses ([`KeyError::Denied`]): nothing is
//! approved or merged, and the passphrase is not asked instead. At a terminal without Windows
//! Hello the same summary and audit are printed, then the passphrase is asked. Only a real
//! hardware click on "Merge" or "Approve" counts (`lil_platform::hello::choice_from_command`):
//! another program running as you can't press it.
//!
//! **The order** ([`unlock_reviewer_key`]): Windows Hello first when enrolled; when it is
//! cancelled or fails, or its files are damaged, the passphrase. Nothing that uses the key runs
//! before one of them has unlocked it.
//!
//! **Secrets.** The decrypted PEM, the KEK and its hex stay in `Zeroizing` buffers and are never
//! printed or logged; nothing here writes them anywhere but the files above.
//!
//! This module needs no GitHub and no git: `tests/reviewer_hello.rs` compiles it on its own.

use super::reviewer_key::{
    self, KeyError, WORK_FACTOR, check_encrypted, decrypt_key, encrypt_key_with_work_factor,
    key_path_in, load_key_in,
};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// The Windows Hello key credential's name.
pub const CREDENTIAL: &str = "LILITH-reviewer-kek";

/// The route Task 28's Step 0 proved (P4a-R38): the KEK. On 2026-10-24 the probe created a
/// window-owned credential and signed twice with identical signatures, and the prompt came to
/// the front by itself in every step, over a fullscreen window too.
pub const PROVEN_ROUTE: Option<Route> = Some(Route::Kek);

/// The Windows Hello route that protects the key. The KEK is the only one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Kek,
}

impl Route {
    fn name(self) -> &'static str {
        match self {
            Route::Kek => "kek",
        }
    }
}

/// `app-key.hello.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloFile {
    pub route: Route,
    pub credential: String,
    pub challenge: [u8; 32],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloToml {
    route: String,
    credential: String,
    challenge: String,
}

/// Collapses whitespace, newlines in error chains included, onto one line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lowercase hex, in a buffer that is zeroed when dropped (it can be the KEK's).
fn hex(bytes: &[u8]) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(bytes.len() * 2));
    for b in bytes {
        out.push(char::from(b"0123456789abcdef"[usize::from(b >> 4)]));
        out.push(char::from(b"0123456789abcdef"[usize::from(b & 15)]));
    }
    out
}

fn unhex_32(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 || !bytes.iter().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let digit = |c: u8| if c <= b'9' { c - b'0' } else { c - b'a' + 10 };
    let mut out = [0u8; 32];
    for (i, pair) in bytes.chunks(2).enumerate() {
        out[i] = digit(pair[0]) << 4 | digit(pair[1]);
    }
    Some(out)
}

impl HelloFile {
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string(&HelloToml {
            route: self.route.name().into(),
            credential: self.credential.clone(),
            challenge: hex(&self.challenge).to_string(),
        })
        .context("writing the Windows Hello file")
    }

    /// Strict: unknown keys, a route other than `kek`, another credential name or a challenge
    /// that isn't 64 lowercase hex digits are errors.
    pub fn from_toml(text: &str) -> Result<Self> {
        let raw: HelloToml =
            toml::from_str(text).context("it is not a valid Windows Hello file")?;
        let route = match raw.route.as_str() {
            "kek" => Route::Kek,
            other => bail!("its route {other:?} isn't \"kek\", the only one LILITH builds"),
        };
        if raw.credential != CREDENTIAL {
            bail!(
                "it names the credential {:?}, not {CREDENTIAL}",
                raw.credential
            );
        }
        let challenge = unhex_32(&raw.challenge)
            .ok_or_else(|| anyhow!("its challenge is not 64 lowercase hex digits"))?;
        Ok(Self {
            route,
            credential: raw.credential,
            challenge,
        })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_toml(&text)
    }
}

/// `app-key.hello.toml` beside the key file `key_path`.
fn hello_file_beside(key_path: &Path) -> PathBuf {
    key_path.with_file_name("app-key.hello.toml")
}

/// `app-key.hello.age` beside the key file `key_path`.
fn hello_age_beside(key_path: &Path) -> PathBuf {
    key_path.with_file_name("app-key.hello.age")
}

/// `<profile>\<home>\reviewer\app-key.hello.toml`.
pub fn hello_file_path_in(profile: &Path) -> PathBuf {
    hello_file_beside(&key_path_in(profile))
}

/// `<profile>\<home>\reviewer\app-key.hello.age`.
pub fn hello_age_path_in(profile: &Path) -> PathBuf {
    hello_age_beside(&key_path_in(profile))
}

/// Writes `bytes` through a temporary file and a rename, so a failed write leaves nothing half
/// written.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let written = (|| -> Result<()> {
        let mut file =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("flushing {}", tmp.display()))?;
        drop(file);
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

fn remove_if_there(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => {
            // A folder where a file belongs (damage): take it away too.
            if path.is_dir() {
                std::fs::remove_dir_all(path)
                    .with_context(|| format!("removing {}", path.display()))?;
                return Ok(true);
            }
            Err(e).with_context(|| format!("removing {}", path.display()))
        }
    }
}

/// Removes the Windows Hello files beside `key_path` (not the credential): `reviewer-setup`
/// calls this when it writes a new key, since an enrolment holds the old key (Task 28 review,
/// M3). Returns what was removed.
pub fn discard_enrolment_beside(key_path: &Path) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    for path in [hello_file_beside(key_path), hello_age_beside(key_path)] {
        if remove_if_there(&path)? {
            removed.push(path);
        }
    }
    Ok(removed)
}

// ---------------------------------------------------------------------------------------------
// The Windows Hello seam.
// ---------------------------------------------------------------------------------------------

pub use lil_platform::hello::FaceSignIn;

/// Why Windows Hello isn't used for the reviewer key, or `None` when face sign-in is off
/// (owner decision 2026-10-24: Windows Hello unlocks merges only without face sign-in, so the
/// prompt always needs a deliberate PIN or fingerprint). Pure.
pub fn face_blocks_hello(face: &FaceSignIn) -> Option<String> {
    match face {
        FaceSignIn::Off => None,
        FaceSignIn::On => Some(
            "face sign-in is on for Windows Hello; LILITH uses your passphrase until you turn it off in Settings > Accounts > Sign-in options".into(),
        ),
        FaceSignIn::Unknown(why) => Some(format!(
            "LILITH can't tell whether face sign-in is on for Windows Hello ({}), so it uses your passphrase",
            one_line(why)
        )),
    }
}

/// The face rule applied to an enrolment in `profile` (owner decision 2026-10-24; Phase B
/// review I1, plan 4a ruling): `Ok(())` while `face` is off. On: a face could complete the
/// credential's prompt for any program running as you, so LILITH removes its own Windows Hello
/// enrolment ([`remove`]: the two files, then the key credential, which never prompts; the
/// passphrase file stays), and the error says so and how to enrol again. Unknown: the enrolment
/// stays, and the error is the warning. Either error means: use the passphrase.
pub fn face_gate(
    profile: &Path,
    platform: &dyn HelloPlatform,
    face: &FaceSignIn,
) -> Result<(), String> {
    let Some(why) = face_blocks_hello(face) else {
        return Ok(());
    };
    if *face != FaceSignIn::On {
        return Err(format!(
            "{why}. The Windows Hello enrolment stays; turn face sign-in off, or make sure it is off, in Settings > Accounts > Sign-in options"
        ));
    }
    let what = match remove(profile, platform) {
        Ok(removed) if removed.is_empty() => String::new(),
        Ok(_) => " LILITH removed its Windows Hello enrolment (app-key.hello.toml, app-key.hello.age and the key credential), because with face sign-in on any program running as you could open it with your face; your passphrase key stays. To use Windows Hello again, turn face sign-in off, then run `lil dev reviewer-hello enroll`".into(),
        Err(e) => format!(
            " Removing LILITH's Windows Hello enrolment failed ({}); run `lil dev reviewer-hello remove` now, because with face sign-in on any program running as you could open it with your face",
            one_line(&format!("{e:#}"))
        ),
    };
    Err(format!("{why}.{what}"))
}

/// The Windows Hello calls (P4a-R38), one seam: [`WindowsHello`] for real, a fake in tests.
/// `shown` is what the window that owns the prompt shows.
pub trait HelloPlatform {
    /// Whether Windows Hello is set up for this user. No prompt.
    fn supported(&self) -> bool;
    /// Whether a TPM 2.0 is present (Windows Hello normally keeps its keys there). No prompt.
    fn hardware_held(&self) -> bool;
    /// Whether Windows Hello face sign-in is enrolled for this user (owner decision 2026-10-24:
    /// Windows Hello unlocks only without it). Read-only; no prompt.
    fn face_sign_in(&self) -> FaceSignIn;
    /// Creates (or replaces) the key credential. Prompts. An error when its window wasn't
    /// brought forward in time (it flashes in the taskbar meanwhile) or the prompt isn't
    /// answered in time.
    fn kek_enroll(&self, credential: &str, shown: &str) -> io::Result<()>;
    /// The KEK from the credential's signature over `challenge`. Prompts; errors as above.
    /// With `accept` ("Merge", "Approve"), the window has that button and "Deny", and Windows
    /// Hello is raised only after the accept click; "Deny", closing the window or no choice in
    /// time (the window never brought forward included) is a `PermissionDenied` error, which
    /// refuses outright (Task 28 fix round 2, Phase B ruling). `ready` runs right before
    /// Windows Hello is raised, after the click; its error stops the call (Phase B review m2:
    /// the face check runs again there).
    fn kek_derive(
        &self,
        credential: &str,
        challenge: &[u8; 32],
        shown: &str,
        accept: Option<&str>,
        ready: &dyn Fn() -> io::Result<()>,
    ) -> io::Result<Zeroizing<[u8; 32]>>;
    /// Deletes the key credential; one that isn't there is fine. Whether it was there. No
    /// prompt.
    fn kek_remove(&self, credential: &str) -> io::Result<bool>;
}

/// The real Windows Hello (`lil_platform::hello`): every prompt is owned by a window this
/// process opens and tries to bring to the front, never the console. When Windows keeps it
/// behind, it flashes in the taskbar and waits for you to bring it forward (QA-088, Phase B
/// ruling); a prompt is cancelled after `HELLO_WAIT`.
pub struct WindowsHello;

impl HelloPlatform for WindowsHello {
    fn supported(&self) -> bool {
        lil_platform::hello::supported()
    }
    fn hardware_held(&self) -> bool {
        lil_platform::hello::tpm_present()
    }
    fn face_sign_in(&self) -> FaceSignIn {
        lil_platform::hello::face_sign_in()
    }
    fn kek_enroll(&self, credential: &str, shown: &str) -> io::Result<()> {
        lil_platform::hello::kek_enroll(credential, shown)
    }
    fn kek_derive(
        &self,
        credential: &str,
        challenge: &[u8; 32],
        shown: &str,
        accept: Option<&str>,
        ready: &dyn Fn() -> io::Result<()>,
    ) -> io::Result<Zeroizing<[u8; 32]>> {
        lil_platform::hello::kek_derive(credential, challenge, shown, accept, ready)
    }
    fn kek_remove(&self, credential: &str) -> io::Result<bool> {
        let existed = lil_platform::hello::credential_exists(credential)?;
        lil_platform::hello::kek_remove(credential)?;
        Ok(existed)
    }
}

// ---------------------------------------------------------------------------------------------
// Unlocking.
// ---------------------------------------------------------------------------------------------

/// Unlocks the reviewer key's PEM. A fake in tests.
pub trait Unlocker {
    fn unlock(&self) -> Result<Zeroizing<Vec<u8>>, KeyError>;
}

/// The passphrase path: `app-key.age` with the passphrase `ask` reads. A damaged file is
/// refused before you are asked.
pub struct PassphraseUnlocker<'a> {
    encrypted_pem: &'a [u8],
    ask: &'a dyn Fn(&str) -> Zeroizing<String>,
    label: &'a str,
}

impl<'a> PassphraseUnlocker<'a> {
    pub fn new(encrypted_pem: &'a [u8], ask: &'a dyn Fn(&str) -> Zeroizing<String>) -> Self {
        Self {
            encrypted_pem,
            ask,
            label: "Reviewer key passphrase: ",
        }
    }

    /// The same, with another prompt label.
    pub fn labelled(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }
}

fn key_error(e: &anyhow::Error) -> KeyError {
    e.downcast_ref::<KeyError>()
        .copied()
        .unwrap_or(KeyError::Damaged)
}

impl Unlocker for PassphraseUnlocker<'_> {
    fn unlock(&self) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        check_encrypted(self.encrypted_pem).map_err(|_| KeyError::Damaged)?;
        let passphrase = (self.ask)(self.label);
        decrypt_key(self.encrypted_pem, &passphrase)
            .map(Zeroizing::new)
            .map_err(|e| key_error(&e))
    }
}

/// The passphrase path where there is no terminal: nobody can type it, so it unlocks nothing
/// (and the console, which `rpassword` would open directly, is never read).
pub struct NoTerminal<'a> {
    pub say: &'a dyn Fn(&str),
}

impl Unlocker for NoTerminal<'_> {
    fn unlock(&self) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        (self.say)("There is no terminal here to type the passphrase into.");
        Err(KeyError::Unavailable)
    }
}

/// Windows Hello for an enrolment: signs the challenge and opens `app-key.hello.age`. Whatever
/// goes wrong is [`KeyError::Cancelled`] or [`KeyError::Unavailable`], after `say` has told you
/// why.
pub struct HelloUnlocker<'a> {
    /// Where the enrolment is: the face rule may remove it ([`face_gate`]).
    profile: PathBuf,
    file: HelloFile,
    sealed: Vec<u8>,
    platform: &'a dyn HelloPlatform,
    /// What the owner window shows when nothing more specific is given.
    message: String,
    /// The accept button ("Merge", "Approve"), or `None` for none.
    accept: Option<String>,
    say: &'a dyn Fn(&str),
}

fn hello_error(say: &dyn Fn(&str), e: &io::Error) -> KeyError {
    if e.kind() == io::ErrorKind::Interrupted {
        KeyError::Cancelled
    } else if e.kind() == io::ErrorKind::PermissionDenied {
        say(&format!("Refused: {e}; nothing was approved or merged."));
        KeyError::Denied
    } else {
        say(&format!("Windows Hello isn't used: {e}."));
        KeyError::Unavailable
    }
}

impl HelloUnlocker<'_> {
    /// Unlocks with the owner window showing `shown`.
    pub fn unlock_showing(&self, shown: &str) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        // The face rule again, right before Windows Hello is raised (Phase B review m2).
        let face_off = || {
            face_gate(&self.profile, self.platform, &self.platform.face_sign_in())
                .map_err(io::Error::other)
        };
        let kek = self
            .platform
            .kek_derive(
                &self.file.credential,
                &self.file.challenge,
                shown,
                self.accept.as_deref(),
                &face_off,
            )
            .map_err(|e| hello_error(self.say, &e))?;
        let passphrase = hex(&kek[..]);
        drop(kek);
        decrypt_key(&self.sealed, &passphrase)
            .map(Zeroizing::new)
            .map_err(|_| {
                (self.say)("Windows Hello answered, but its key doesn't open app-key.hello.age.");
                KeyError::Unavailable
            })
    }
}

impl Unlocker for HelloUnlocker<'_> {
    fn unlock(&self) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        self.unlock_showing(&self.message)
    }
}

/// A [`HelloUnlocker`] that shows a given text.
struct Showing<'a, 'h> {
    hello: &'a HelloUnlocker<'h>,
    shown: &'a str,
}

impl Unlocker for Showing<'_, '_> {
    fn unlock(&self) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        self.hello.unlock_showing(self.shown)
    }
}

/// The enrolment in `profile`, read and checked: `Ok(None)` when there is none.
fn read_enrolment(profile: &Path) -> Result<Option<(HelloFile, Vec<u8>)>> {
    let path = hello_file_path_in(profile);
    if matches!(std::fs::symlink_metadata(&path), Err(e) if e.kind() == io::ErrorKind::NotFound) {
        return Ok(None);
    }
    let file = HelloFile::load(&path)?;
    let sealed_path = hello_age_path_in(profile);
    let sealed = std::fs::read(&sealed_path)
        .with_context(|| format!("reading {}", sealed_path.display()))?;
    if check_encrypted(&sealed).is_err() {
        bail!("{} is not an age passphrase file", sealed_path.display());
    }
    Ok(Some((file, sealed)))
}

/// Windows Hello for an approval or a merge, when you enrolled it: `None` when you didn't, or
/// when its files are damaged (then `say` tells you, and the passphrase is used). `message` is
/// what its window shows unless the caller gives more ([`HelloUnlocker::unlock_showing`]);
/// `accept` ("Merge", "Approve") is the button you click before Windows Hello is raised.
pub fn enrolled_hello<'a>(
    profile: &Path,
    platform: &'a dyn HelloPlatform,
    message: &str,
    accept: Option<&str>,
    say: &'a dyn Fn(&str),
) -> Option<HelloUnlocker<'a>> {
    match read_enrolment(profile) {
        Ok(None) => None,
        Ok(Some(_)) if let Err(why) = face_gate(profile, platform, &platform.face_sign_in()) => {
            say(&format!("Windows Hello isn't used: {why}."));
            None
        }
        Ok(Some((file, sealed))) => Some(HelloUnlocker {
            profile: profile.to_path_buf(),
            file,
            sealed,
            platform,
            message: message.to_string(),
            accept: accept.map(str::to_string),
            say,
        }),
        Err(e) => {
            say(&format!(
                "Windows Hello isn't used: its files are damaged ({}). Run `lil dev reviewer-hello remove`, then enroll again.",
                one_line(&format!("{e:#}"))
            ));
            None
        }
    }
}

/// Hello first when enrolled; when it is cancelled or fails, the passphrase. Only one of them
/// unlocking the key lets the caller go on.
pub fn unlock_reviewer_key(
    hello: Option<&dyn Unlocker>,
    passphrase: &dyn Unlocker,
    say: &dyn Fn(&str),
) -> Result<Zeroizing<Vec<u8>>, KeyError> {
    if let Some(hello) = hello {
        match hello.unlock() {
            Ok(pem) => return Ok(pem),
            // "Deny" in the LILITH window is your answer, not a reason to ask again.
            Err(KeyError::Denied) => return Err(KeyError::Denied),
            Err(KeyError::Cancelled) => {
                say("Windows Hello was cancelled; asking for the passphrase instead.");
            }
            Err(_) => {
                say("Windows Hello didn't unlock the key; asking for the passphrase instead.")
            }
        }
    }
    passphrase.unlock()
}

// ---------------------------------------------------------------------------------------------
// The authentication every approval and merge asks for (QA-036, P4a-R38).
// ---------------------------------------------------------------------------------------------

/// Shows `summary` and `audit`, then authenticates you: Windows Hello first, then the
/// passphrase. The PEM it returns is the proof, and what an approval signs with. A fake in tests.
pub trait Authenticator {
    fn authenticate(&self, summary: &str, audit: &str) -> Result<Zeroizing<Vec<u8>>, KeyError>;
}

/// The real [`Authenticator`]. The summary and the audit, made safe to show by `render`, go to
/// `show` (the terminal) and to the window that owns the Windows Hello prompt, before anything
/// prompts.
pub struct KeyAuthenticator<'a> {
    show: &'a dyn Fn(&str),
    render: &'a dyn Fn(&str) -> String,
    hello: Option<&'a HelloUnlocker<'a>>,
    passphrase: &'a dyn Unlocker,
    say: &'a dyn Fn(&str),
}

impl<'a> KeyAuthenticator<'a> {
    pub fn new(
        show: &'a dyn Fn(&str),
        render: &'a dyn Fn(&str) -> String,
        hello: Option<&'a HelloUnlocker<'a>>,
        passphrase: &'a dyn Unlocker,
        say: &'a dyn Fn(&str),
    ) -> Self {
        Self {
            show,
            render,
            hello,
            passphrase,
            say,
        }
    }
}

/// The mark in front of every line someone else wrote (an agent's summary, the audit), so it
/// can't pass for lil's own lines (Task 28 review, M9).
pub const QUOTE: &str = "  | ";

/// `text`'s lines, each behind [`QUOTE`].
pub fn quoted(text: &str) -> String {
    let mut out = String::new();
    for line in text.trim_end().lines() {
        out.push_str(QUOTE);
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// What you read before you authenticate: the summary, then the audit, quoted.
pub fn presentation(summary: &str, audit: &str) -> String {
    format!(
        "{}\n\nIndependent audit:\n{}",
        summary.trim_end(),
        quoted(audit)
    )
}

impl Authenticator for KeyAuthenticator<'_> {
    fn authenticate(&self, summary: &str, audit: &str) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        let shown = (self.render)(&presentation(summary, audit));
        (self.show)(&shown);
        let hello = self.hello.map(|hello| Showing {
            hello,
            shown: &shown,
        });
        unlock_reviewer_key(
            hello.as_ref().map(|h| h as &dyn Unlocker),
            self.passphrase,
            self.say,
        )
    }
}

// ---------------------------------------------------------------------------------------------
// enroll, status, remove.
// ---------------------------------------------------------------------------------------------

/// Enrols Windows Hello: decrypts `app-key.age` with your passphrase first (a wrong passphrase
/// stops here, before Windows Hello is asked), then seals the same PEM under the KEK and writes
/// the Hello file last. `app-key.age` is only read. On any failure the files written so far, and
/// the credential, are removed again.
pub fn enroll(
    profile: &Path,
    route: Route,
    platform: &dyn HelloPlatform,
    ask: &dyn Fn(&str) -> Zeroizing<String>,
    challenge: [u8; 32],
    log_n: u8,
    say: &dyn Fn(&str),
) -> Result<()> {
    let (path, key) = load_key_in(profile)?;
    if !matches!(
        std::fs::symlink_metadata(hello_file_path_in(profile)),
        Err(e) if e.kind() == io::ErrorKind::NotFound
    ) {
        bail!(
            "Windows Hello is already enrolled ({}); run `lil dev reviewer-hello remove` first",
            hello_file_path_in(profile).display()
        );
    }
    // Owner decision 2026-10-24: no enrolment while face sign-in is on (or can't be told).
    if let Some(why) = face_blocks_hello(&platform.face_sign_in()) {
        bail!("{why}. Nothing was written");
    }
    let pem = PassphraseUnlocker::new(&key.encrypted_pem, ask)
        .unlock()
        .map_err(|e| match e {
            KeyError::WrongPassphrase => anyhow!("wrong passphrase; nothing was written"),
            other => anyhow!("{other} ({}); nothing was written", path.display()),
        })?;
    if !platform.supported() {
        bail!(
            "Windows Hello isn't set up for your Windows account; set it up in Settings, Accounts, Sign-in options, then try again. Nothing was written"
        );
    }
    let sealed_path = hello_age_path_in(profile);
    let shown = "LILITH: set up Windows Hello for the reviewer key.\n\nWindows Hello asks twice: once to create the key, once to sign with it.";
    let written = (|| -> Result<()> {
        say(
            "Windows Hello asks twice: once to create the key, once to sign with it. If a LILITH window flashes in the taskbar instead of coming forward, click it.",
        );
        platform
            .kek_enroll(CREDENTIAL, shown)
            .map_err(|e| anyhow!("Windows Hello didn't create the key: {e}"))?;
        // The face rule again, right before the sign prompt (Phase B review m2).
        let face_off = || match face_blocks_hello(&platform.face_sign_in()) {
            None => Ok(()),
            Some(why) => Err(io::Error::other(why)),
        };
        let kek = platform
            .kek_derive(CREDENTIAL, &challenge, shown, None, &face_off)
            .map_err(|e| anyhow!("Windows Hello didn't sign: {e}"))?;
        let passphrase = hex(&kek[..]);
        drop(kek);
        let sealed = encrypt_key_with_work_factor(&pem, &passphrase, log_n)?;
        // The sealed copy must open with the same KEK before it replaces anything.
        let back = Zeroizing::new(decrypt_key(&sealed, &passphrase)?);
        if back[..] != pem[..] {
            bail!("the sealed key didn't read back the same");
        }
        write_atomic(&sealed_path, &sealed)?;
        let file = HelloFile {
            route,
            credential: CREDENTIAL.into(),
            challenge,
        };
        write_atomic(&hello_file_path_in(profile), file.to_toml()?.as_bytes())
    })();
    if let Err(e) = written {
        let _ = remove_if_there(&sealed_path);
        let _ = remove_if_there(&hello_file_path_in(profile));
        let _ = platform.kek_remove(CREDENTIAL);
        return Err(e.context("nothing was enrolled"));
    }
    Ok(())
}

/// What `lil dev reviewer-hello status` prints: the passphrase file, the route, and whether the
/// key is likely hardware-held (never "hardware-bound", QA-088), and face sign-in. Never
/// prompts. Face sign-in on removes the enrolment first ([`face_gate`]).
pub fn status(profile: &Path, platform: &dyn HelloPlatform) -> Vec<String> {
    let key = key_path_in(profile);
    let mut out = vec![format!(
        "Passphrase key (the fallback): {} ({})",
        key.display(),
        if key.exists() { "present" } else { "missing" }
    )];
    let face = platform.face_sign_in();
    let face_line = match read_enrolment(profile) {
        Ok(Some(_)) => face_gate(profile, platform, &face).err(),
        _ => face_blocks_hello(&face),
    };
    match read_enrolment(profile) {
        Ok(None) => out.push(
            "Windows Hello: not enrolled; approvals and merges ask for the passphrase.".into(),
        ),
        Ok(Some((file, _))) => {
            out.push(format!(
                "Windows Hello: enrolled, route KEK: the key credential {} signs a fixed challenge, and a key derived from that signature opens {}.",
                file.credential,
                hello_age_path_in(profile).display()
            ));
            // Windows Hello normally keeps its keys in the TPM when there is one, but can fall
            // back to software; this isn't checked per key (Task 28 review, M1).
            out.push(if platform.hardware_held() {
                "Key storage: hardware-held: probably (a TPM 2.0 is present, and Windows Hello normally keeps its key in it; not checked for this key).".into()
            } else {
                "Key storage: hardware-held: no (no TPM 2.0 was found, so the Windows Hello key is held in software).".into()
            });
        }
        Err(e) => out.push(format!(
            "Windows Hello: its files are damaged ({}); approvals and merges ask for the passphrase. Run `lil dev reviewer-hello remove`, then enroll again.",
            one_line(&format!("{e:#}"))
        )),
    }
    out.push(format!(
        "Windows Hello on this PC: {}.",
        if platform.supported() {
            "set up"
        } else {
            "not set up"
        }
    ));
    out.push(match face_line {
        None => "Face sign-in: off, so Windows Hello can unlock the reviewer key (a PIN or fingerprint at its prompt).".into(),
        Some(why) => format!(
            "Face sign-in: {} ({why}).",
            if face == FaceSignIn::On { "on" } else { "unknown" }
        ),
    });
    out
}

/// Removes the Windows Hello files, then the key credential; `app-key.age` stays. A damaged
/// enrolment is removed too. The files go first, so a credential that can't be removed (Windows
/// Hello switched off) never keeps them; that failure is then the error. The credential is
/// removed even when the files are already gone (Phase B re-review N1: a removal that failed
/// half-way leaves only the credential); its failure is reported only when files were there.
/// Returns what was removed.
pub fn remove(profile: &Path, platform: &dyn HelloPlatform) -> Result<Vec<String>> {
    let enrolled_somehow =
        hello_file_path_in(profile).exists() || hello_age_path_in(profile).exists();
    let mut removed = Vec::new();
    for path in [hello_file_path_in(profile), hello_age_path_in(profile)] {
        if remove_if_there(&path)? {
            removed.push(path.display().to_string());
        }
    }
    let existed = match platform.kek_remove(CREDENTIAL) {
        Ok(existed) => existed,
        Err(e) if enrolled_somehow => {
            return Err(anyhow!(
                "the Windows Hello files are removed, but removing the key credential {CREDENTIAL} failed: {e}"
            ));
        }
        Err(_) => false,
    };
    if enrolled_somehow || existed {
        removed.push(format!("the Windows Hello key credential {CREDENTIAL}"));
    }
    Ok(removed)
}

// ---------------------------------------------------------------------------------------------
// CLI wiring.
// ---------------------------------------------------------------------------------------------

fn at_a_terminal() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn read_passphrase(label: &str) -> Zeroizing<String> {
    Zeroizing::new(rpassword::prompt_password(label).unwrap_or_default())
}

fn say_to_stderr(text: &str) {
    eprintln!("{text}");
}

/// `lil dev reviewer-hello enroll`: exit 0 when enrolled.
pub fn run_enroll() -> i32 {
    let fail = |e: &anyhow::Error| {
        eprintln!(
            "lil dev reviewer-hello enroll: {}",
            one_line(&format!("{e:#}"))
        );
        1
    };
    if !at_a_terminal() {
        return fail(&anyhow!(
            "it only runs at a real terminal (stdin and stderr must both be one); nothing was written"
        ));
    }
    let Some(route) = PROVEN_ROUTE else {
        return fail(&anyhow!(
            "Windows Hello's route isn't proven yet (plan 4a Task 28's route step); approvals and merges keep asking for the passphrase. Nothing was written"
        ));
    };
    let result = reviewer_key::user_profile().and_then(|profile| {
        let challenge = lil_platform::hello::new_challenge().context("making the challenge")?;
        enroll(
            &profile,
            route,
            &WindowsHello,
            &read_passphrase,
            challenge,
            WORK_FACTOR,
            &say_to_stderr,
        )?;
        Ok(profile)
    });
    match result {
        Ok(profile) => {
            for line in status(&profile, &WindowsHello) {
                println!("{line}");
            }
            0
        }
        Err(e) => fail(&e),
    }
}

/// `lil dev reviewer-hello status`: always exit 0 once it could look.
pub fn run_status() -> i32 {
    match reviewer_key::user_profile() {
        Ok(profile) => {
            for line in status(&profile, &WindowsHello) {
                println!("{line}");
            }
            0
        }
        Err(e) => {
            eprintln!("lil dev reviewer-hello status: {e:#}");
            1
        }
    }
}

/// `lil dev reviewer-hello remove`: exit 0 when nothing of the enrolment is left.
pub fn run_remove() -> i32 {
    let result = reviewer_key::user_profile().and_then(|profile| remove(&profile, &WindowsHello));
    match result {
        Ok(removed) if removed.is_empty() => {
            println!("Windows Hello wasn't enrolled; nothing to remove. The passphrase key stays.");
            0
        }
        Ok(removed) => {
            for what in removed {
                println!("Removed {what}.");
            }
            println!("The passphrase key stays; approvals and merges ask for it.");
            0
        }
        Err(e) => {
            eprintln!(
                "lil dev reviewer-hello remove: {}",
                one_line(&format!("{e:#}"))
            );
            1
        }
    }
}
