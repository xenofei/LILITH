//! The reviewer App's key file and its passphrase encryption (QA-020), shared by
//! `lil dev reviewer-setup`, `lil dev approve`, Windows Hello (`reviewer_hello`, QA-088) and the
//! merge flow. Moved here unchanged from `reviewer.rs` (plan 4a Task 28) so that this part, which
//! needs no GitHub or git, can be compiled on its own by `tests/reviewer_hello.rs`; `reviewer.rs`
//! re-exports it, so every path that named these items still does.
//!
//! The key is the App's `.pem`, encrypted with an age scrypt passphrase, in
//! `%USERPROFILE%\<home>\reviewer\app-key.age` ([`key_path_in`]) with the App id and the
//! installation id. The decrypted PEM is only ever held in a `Zeroizing` buffer by the callers.

use age::secrecy::SecretString;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use lil_platform::names;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// scrypt work factor (`N = 2^18`) for the stored key: fixed, not calibrated, so a slow or debug
/// build never writes a weaker file.
pub const WORK_FACTOR: u8 = 18;
/// The largest work factor a key file may demand when it is decrypted.
pub const MAX_WORK_FACTOR: u8 = 20;

pub const DAMAGED_KEY: &str = "the key file is damaged or not an age passphrase file";

/// Why a key file could not be decrypted, or the key not unlocked (Task 28 added the last three).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    WrongPassphrase,
    Damaged,
    /// You cancelled the Windows Hello prompt (or chose its other-option button).
    Cancelled,
    /// Windows Hello couldn't unlock the key: a failed call, or a Hello file it doesn't open.
    Unavailable,
    /// You chose "Deny" in the LILITH window, closed it, or chose nothing in time: a refusal,
    /// never a reason to ask for the passphrase instead (Task 28 fix round 2).
    Denied,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KeyError::WrongPassphrase => "wrong passphrase",
            KeyError::Damaged => DAMAGED_KEY,
            KeyError::Cancelled => "the Windows Hello prompt was cancelled",
            KeyError::Unavailable => "Windows Hello couldn't unlock the reviewer key",
            KeyError::Denied => {
                "you didn't confirm it in the LILITH window (Deny, closed, or no choice in time)"
            }
        })
    }
}

impl std::error::Error for KeyError {}

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

/// The reviewer key to use: [`key_path_in`] when anything is there, else `None`. "Anything"
/// means whatever isn't a plain not-found: a file, a folder, a link, or a path that can't be
/// examined. So an occupied or unreadable path is chosen, and loading it refuses. Only looks;
/// nothing is created.
pub fn find_key_in(profile: &Path) -> Option<PathBuf> {
    Some(key_path_in(profile)).filter(|p| {
        !matches!(std::fs::symlink_metadata(p), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
    })
}

/// [`find_key_in`], or an error naming the path it looked at.
pub(crate) fn locate_key_in(profile: &Path) -> Result<PathBuf> {
    find_key_in(profile).ok_or_else(|| {
        anyhow!(
            "there is no reviewer key at {}; run `lil dev reviewer-setup` first",
            key_path_in(profile).display()
        )
    })
}

/// The key file [`locate_key_in`] finds, loaded, with its path. A key that can't be read or
/// parsed is an error naming its path.
pub(crate) fn load_key_in(profile: &Path) -> Result<(PathBuf, ReviewerKeyFile)> {
    let path = locate_key_in(profile)?;
    let key = ReviewerKeyFile::load(&path)?;
    Ok((path, key))
}

/// Where `reviewer-setup` writes the key: [`setup_key_path_in`] for `%USERPROFILE%`.
pub fn setup_key_path() -> Result<PathBuf> {
    Ok(setup_key_path_in(&user_profile()?))
}

/// Where `reviewer-setup` writes the key for `profile`: [`key_path_in`].
pub fn setup_key_path_in(profile: &Path) -> PathBuf {
    key_path_in(profile)
}

/// `%USERPROFILE%`, which must be set and absolute.
pub(crate) fn user_profile() -> Result<PathBuf> {
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
// Encryption.
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
pub(crate) fn check_encrypted(blob: &[u8]) -> std::result::Result<age::Decryptor<&[u8]>, KeyError> {
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
