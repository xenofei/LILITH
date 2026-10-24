//! `lil dev reviewer-hello enroll | status | remove`: Windows Hello for the reviewer key
//! (QA-088, P4a-R23, P4a-R38; plan 4a Task 28), and the authentication every approval and every
//! merge asks for (QA-036).
//!
//! **The files**, beside the passphrase file `app-key.age` (which always stays, the fallback):
//!
//! - `app-key.hello.toml`: `route = "kek" | "dpapi"`, `credential = "LILITH-reviewer-kek"`,
//!   `challenge = "<64 lowercase hex>"` ([`HelloFile`]). Written last, so it marks a finished
//!   enrolment.
//! - KEK route: `app-key.hello.age`, the PEM under age's scrypt with the passphrase the
//!   lowercase hex of the KEK: SHA-256 over `LILITH reviewer KEK v1` and the Windows Hello key
//!   credential's signature over the challenge (P4a-R23). The existing age decryption stays the
//!   one path.
//! - DPAPI route: `app-key.dpapi`, the PEM protected with DPAPI for this Windows user, used only
//!   after Windows Hello's consent prompt says yes. That gate is a check in the UI, not
//!   encryption, the weaker option.
//!
//! **The seam.** Everything route-specific goes through [`HelloPlatform`]: [`WindowsHello`] for
//! real (`lil_platform::hello`), a fake in `tests/reviewer_hello.rs`. Which route `enroll` uses
//! is [`PROVEN_ROUTE`], settled by Task 28's route step (Step 0, P4a-R38); until then it is
//! `None`, so `enroll` refuses and every approval and merge uses the passphrase.
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

/// The Windows Hello key credential's name (KEK route).
pub const CREDENTIAL: &str = "LILITH-reviewer-kek";

/// The route Task 28's Step 0 proved (P4a-R38), or `None` until it has run. Phase B sets it.
pub const PROVEN_ROUTE: Option<Route> = None;

/// Which Windows Hello route protects the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Kek,
    Dpapi,
}

impl Route {
    fn name(self) -> &'static str {
        match self {
            Route::Kek => "kek",
            Route::Dpapi => "dpapi",
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

    /// Strict: unknown keys, another route, another credential name or a challenge that isn't
    /// 64 lowercase hex digits are errors.
    pub fn from_toml(text: &str) -> Result<Self> {
        let raw: HelloToml =
            toml::from_str(text).context("it is not a valid Windows Hello file")?;
        let route = match raw.route.as_str() {
            "kek" => Route::Kek,
            "dpapi" => Route::Dpapi,
            other => bail!("its route {other:?} is neither \"kek\" nor \"dpapi\""),
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

fn reviewer_dir(profile: &Path) -> PathBuf {
    key_path_in(profile)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// `<profile>\<home>\reviewer\app-key.hello.toml`.
pub fn hello_file_path_in(profile: &Path) -> PathBuf {
    reviewer_dir(profile).join("app-key.hello.toml")
}

/// `<profile>\<home>\reviewer\app-key.hello.age` (KEK route).
pub fn hello_age_path_in(profile: &Path) -> PathBuf {
    reviewer_dir(profile).join("app-key.hello.age")
}

/// `<profile>\<home>\reviewer\app-key.dpapi` (DPAPI route).
pub fn dpapi_path_in(profile: &Path) -> PathBuf {
    reviewer_dir(profile).join("app-key.dpapi")
}

fn sealed_path_in(profile: &Path, route: Route) -> PathBuf {
    match route {
        Route::Kek => hello_age_path_in(profile),
        Route::Dpapi => dpapi_path_in(profile),
    }
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

// ---------------------------------------------------------------------------------------------
// The route seam.
// ---------------------------------------------------------------------------------------------

/// The route-specific calls (P4a-R38), one seam: [`WindowsHello`] for real, a fake in tests.
/// `message` is what the window that owns the prompt says.
pub trait HelloPlatform {
    /// Whether Windows Hello is set up for this user. No prompt.
    fn supported(&self) -> bool;
    /// Whether a Windows Hello key would be held in hardware (a TPM 2.0 is present). No prompt.
    fn hardware_held(&self) -> bool;
    /// KEK route: creates (or replaces) the key credential. Prompts.
    fn kek_enroll(&self, credential: &str, message: &str) -> io::Result<()>;
    /// KEK route: the KEK from the credential's signature over `challenge`. Prompts.
    fn kek_derive(
        &self,
        credential: &str,
        challenge: &[u8; 32],
        message: &str,
    ) -> io::Result<Zeroizing<[u8; 32]>>;
    /// KEK route: deletes the key credential; one that isn't there is fine. No prompt.
    fn kek_remove(&self, credential: &str) -> io::Result<()>;
    /// DPAPI route's gate: `true` when Windows Hello verified you. Prompts.
    fn consent(&self, message: &str) -> io::Result<bool>;
    /// DPAPI route: protects for this Windows user. No prompt.
    fn dpapi_protect(&self, data: &[u8]) -> io::Result<Vec<u8>>;
    /// DPAPI route: unprotects. No prompt.
    fn dpapi_unprotect(&self, blob: &[u8]) -> io::Result<Zeroizing<Vec<u8>>>;
}

/// The real Windows Hello (`lil_platform::hello`). Every prompt is owned by a window this
/// process opens and brings to the front ([`lil_platform::hello::OwnerWindow`]), never the
/// console (P4a-R38).
pub struct WindowsHello;

fn owned<T>(message: &str, call: impl FnOnce(isize) -> io::Result<T>) -> io::Result<T> {
    let window = lil_platform::hello::OwnerWindow::open("LILITH: Windows Hello", message)?;
    window.bring_to_front();
    call(window.hwnd())
}

impl HelloPlatform for WindowsHello {
    fn supported(&self) -> bool {
        lil_platform::hello::supported()
    }
    fn hardware_held(&self) -> bool {
        lil_platform::hello::tpm_present()
    }
    fn kek_enroll(&self, credential: &str, message: &str) -> io::Result<()> {
        owned(message, |hwnd| {
            lil_platform::hello::kek_enroll_for_window(hwnd, credential)
        })
    }
    fn kek_derive(
        &self,
        credential: &str,
        challenge: &[u8; 32],
        message: &str,
    ) -> io::Result<Zeroizing<[u8; 32]>> {
        owned(message, |hwnd| {
            lil_platform::hello::kek_derive_for_window(hwnd, credential, challenge)
        })
    }
    fn kek_remove(&self, credential: &str) -> io::Result<()> {
        lil_platform::hello::kek_remove(credential)
    }
    fn consent(&self, message: &str) -> io::Result<bool> {
        owned(message, |hwnd| {
            lil_platform::hello::consent_for_window(hwnd, message)
        })
    }
    fn dpapi_protect(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        lil_platform::hello::dpapi_protect(data)
    }
    fn dpapi_unprotect(&self, blob: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
        lil_platform::hello::dpapi_unprotect(blob)
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

/// The Windows Hello path for an enrolment: the KEK route signs the challenge and opens
/// `app-key.hello.age`; the DPAPI route asks for consent and then unprotects `app-key.dpapi`.
/// Whatever goes wrong is [`KeyError::Cancelled`] or [`KeyError::Unavailable`], after `say`
/// has told you why.
pub struct HelloUnlocker<'a> {
    file: HelloFile,
    sealed: Vec<u8>,
    platform: &'a dyn HelloPlatform,
    message: String,
    say: &'a dyn Fn(&str),
}

fn hello_error(say: &dyn Fn(&str), e: &io::Error) -> KeyError {
    if e.kind() == io::ErrorKind::Interrupted {
        KeyError::Cancelled
    } else {
        say(&format!("Windows Hello failed: {e}."));
        KeyError::Unavailable
    }
}

impl Unlocker for HelloUnlocker<'_> {
    fn unlock(&self) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        match self.file.route {
            Route::Kek => {
                let kek = self
                    .platform
                    .kek_derive(&self.file.credential, &self.file.challenge, &self.message)
                    .map_err(|e| hello_error(self.say, &e))?;
                let passphrase = hex(&kek[..]);
                drop(kek);
                decrypt_key(&self.sealed, &passphrase)
                    .map(Zeroizing::new)
                    .map_err(|_| {
                        (self.say)(&format!(
                            "Windows Hello answered, but its key doesn't open {}.",
                            HELLO_AGE_NAME
                        ));
                        KeyError::Unavailable
                    })
            }
            Route::Dpapi => {
                if !self
                    .platform
                    .consent(&self.message)
                    .map_err(|e| hello_error(self.say, &e))?
                {
                    return Err(KeyError::Cancelled);
                }
                self.platform.dpapi_unprotect(&self.sealed).map_err(|e| {
                    (self.say)(&format!("DPAPI couldn't open {DPAPI_NAME}: {e}."));
                    KeyError::Unavailable
                })
            }
        }
    }
}

const HELLO_AGE_NAME: &str = "app-key.hello.age";
const DPAPI_NAME: &str = "app-key.dpapi";

/// The enrolment in `profile`, read and checked: `Ok(None)` when there is none.
fn read_enrolment(profile: &Path) -> Result<Option<(HelloFile, Vec<u8>)>> {
    let path = hello_file_path_in(profile);
    if matches!(std::fs::symlink_metadata(&path), Err(e) if e.kind() == io::ErrorKind::NotFound) {
        return Ok(None);
    }
    let file = HelloFile::load(&path)?;
    let sealed_path = sealed_path_in(profile, file.route);
    let sealed = std::fs::read(&sealed_path)
        .with_context(|| format!("reading {}", sealed_path.display()))?;
    if file.route == Route::Kek && check_encrypted(&sealed).is_err() {
        bail!("{} is not an age passphrase file", sealed_path.display());
    }
    Ok(Some((file, sealed)))
}

/// Windows Hello for an approval or a merge, when you enrolled it: `None` when you didn't, or
/// when its files are damaged (then `say` tells you, and the passphrase is used).
pub fn enrolled_hello<'a>(
    profile: &Path,
    platform: &'a dyn HelloPlatform,
    message: &str,
    say: &'a dyn Fn(&str),
) -> Option<HelloUnlocker<'a>> {
    match read_enrolment(profile) {
        Ok(None) => None,
        Ok(Some((file, sealed))) => Some(HelloUnlocker {
            file,
            sealed,
            platform,
            message: message.to_string(),
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

/// The real [`Authenticator`]: `show` gets the summary and the audit before anything prompts.
pub struct KeyAuthenticator<'a> {
    show: &'a dyn Fn(&str),
    hello: Option<&'a dyn Unlocker>,
    passphrase: &'a dyn Unlocker,
    say: &'a dyn Fn(&str),
}

impl<'a> KeyAuthenticator<'a> {
    pub fn new(
        show: &'a dyn Fn(&str),
        hello: Option<&'a dyn Unlocker>,
        passphrase: &'a dyn Unlocker,
        say: &'a dyn Fn(&str),
    ) -> Self {
        Self {
            show,
            hello,
            passphrase,
            say,
        }
    }
}

/// What you read before you authenticate.
pub fn presentation(summary: &str, audit: &str) -> String {
    format!(
        "{}\n\nIndependent audit:\n{}\n",
        summary.trim_end(),
        audit.trim_end()
    )
}

impl Authenticator for KeyAuthenticator<'_> {
    fn authenticate(&self, summary: &str, audit: &str) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        (self.show)(&presentation(summary, audit));
        unlock_reviewer_key(self.hello, self.passphrase, self.say)
    }
}

// ---------------------------------------------------------------------------------------------
// enroll, status, remove.
// ---------------------------------------------------------------------------------------------

/// Enrols Windows Hello on `route`: decrypts `app-key.age` with your passphrase first (a wrong
/// passphrase stops here, before Windows Hello is asked), then seals the same PEM for the route
/// and writes the Hello file last. `app-key.age` is only read. On any failure the files written
/// so far, and the credential, are removed again.
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
    let sealed_path = sealed_path_in(profile, route);
    let message = "LILITH: set up Windows Hello for the reviewer key";
    let written = (|| -> Result<()> {
        let sealed = match route {
            Route::Kek => {
                say("Windows Hello asks twice: once to create the key, once to sign with it.");
                platform
                    .kek_enroll(CREDENTIAL, message)
                    .map_err(|e| anyhow!("Windows Hello didn't create the key: {e}"))?;
                let kek = platform
                    .kek_derive(CREDENTIAL, &challenge, message)
                    .map_err(|e| anyhow!("Windows Hello didn't sign: {e}"))?;
                let passphrase = hex(&kek[..]);
                drop(kek);
                let sealed = encrypt_key_with_work_factor(&pem, &passphrase, log_n)?;
                // The sealed copy must open with the same KEK before it replaces anything.
                let back = Zeroizing::new(decrypt_key(&sealed, &passphrase)?);
                if back[..] != pem[..] {
                    bail!("the sealed key didn't read back the same");
                }
                sealed
            }
            Route::Dpapi => {
                let sealed = platform
                    .dpapi_protect(&pem)
                    .map_err(|e| anyhow!("DPAPI couldn't protect the key: {e}"))?;
                let back = platform
                    .dpapi_unprotect(&sealed)
                    .map_err(|e| anyhow!("DPAPI couldn't read the key back: {e}"))?;
                if back[..] != pem[..] {
                    bail!("the protected key didn't read back the same");
                }
                sealed
            }
        };
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
        if route == Route::Kek {
            let _ = platform.kek_remove(CREDENTIAL);
        }
        return Err(e.context("nothing was enrolled"));
    }
    Ok(())
}

/// What `lil dev reviewer-hello status` prints: the passphrase file, the route, and whether the
/// key is hardware-held (never "hardware-bound" without a TPM, QA-088). Never prompts.
pub fn status(profile: &Path, platform: &dyn HelloPlatform) -> Vec<String> {
    let key = key_path_in(profile);
    let mut out = vec![format!(
        "Passphrase key (the fallback): {} ({})",
        key.display(),
        if key.exists() { "present" } else { "missing" }
    )];
    match read_enrolment(profile) {
        Ok(None) => out.push(
            "Windows Hello: not enrolled; approvals and merges ask for the passphrase.".into(),
        ),
        Ok(Some((file, _))) => match file.route {
            Route::Kek => {
                out.push(format!(
                    "Windows Hello: enrolled, route KEK: the key credential {} signs a fixed challenge, and a key derived from that signature opens {}.",
                    file.credential,
                    hello_age_path_in(profile).display()
                ));
                out.push(if platform.hardware_held() {
                    "Key storage: hardware-held: yes (a TPM 2.0 is present, and Windows Hello keeps its key in it).".into()
                } else {
                    "Key storage: hardware-held: no (no TPM 2.0 was found, so the Windows Hello key is held in software).".into()
                });
            }
            Route::Dpapi => {
                out.push(format!(
                    "Windows Hello: enrolled, route DPAPI: {} is protected for your Windows account, and Windows Hello's consent prompt gates it (a check in the UI, not encryption).",
                    dpapi_path_in(profile).display()
                ));
                out.push(
                    "Key storage: hardware-held: no (DPAPI is held in software, under your Windows sign-in)."
                        .into(),
                );
            }
        },
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
    out
}

/// Removes the Windows Hello files and the key credential; `app-key.age` stays. A damaged
/// enrolment is removed too (the credential then also, in case it was the KEK route). Returns
/// what was removed.
pub fn remove(profile: &Path, platform: &dyn HelloPlatform) -> Result<Vec<String>> {
    let route = HelloFile::load(&hello_file_path_in(profile))
        .ok()
        .map(|f| f.route);
    let enrolled_somehow = route.is_some()
        || hello_file_path_in(profile).exists()
        || hello_age_path_in(profile).exists();
    let mut removed = Vec::new();
    if route == Some(Route::Kek) || (route.is_none() && enrolled_somehow) {
        platform.kek_remove(CREDENTIAL).map_err(|e| {
            anyhow!("removing the Windows Hello key credential {CREDENTIAL} failed: {e}")
        })?;
        removed.push(format!("the Windows Hello key credential {CREDENTIAL}"));
    }
    for path in [
        hello_file_path_in(profile),
        hello_age_path_in(profile),
        dpapi_path_in(profile),
    ] {
        if remove_if_there(&path)? {
            removed.push(path.display().to_string());
        }
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
            "Windows Hello's route isn't settled yet (plan 4a Task 28's route step); approvals and merges keep asking for the passphrase. Nothing was written"
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
