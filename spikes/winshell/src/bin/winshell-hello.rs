//! `winshell-hello`: the Windows Hello checks (S1.8), a console program run from Windows
//! Terminal. Each command prints one JSON line.
//!
//! - `supported`: (a) `KeyCredentialManager::IsSupportedAsync()`
//! - `create`: (b) `RequestCreateAsync("LILITH-spike-kek", ReplaceExisting)` (Hello prompt)
//! - `sign [--count 5]`: (c) `OpenAsync`, then `RequestSignAsync` over the fixed challenge
//! - `tpm`: (d) `Tbsi_GetDeviceInfo`
//! - `verify`: (e) `IUserConsentVerifierInterop::RequestVerificationForWindowAsync(GetConsoleWindow(), …)`
//! - `delete`: (f) `DeleteAsync`; `exists`: whether `OpenAsync` still finds the credential
//! - `dpapi`: a `CryptProtectData` / `CryptUnprotectData` round trip (the DPAPI route)

use serde_json::{Value, json};
use windows::Security::Credentials::UI::{UserConsentVerificationResult, UserConsentVerifier};
use windows::Security::Credentials::{
    KeyCredentialCreationOption, KeyCredentialManager, KeyCredentialStatus,
};
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::Win32::System::Console::GetConsoleWindow;
use windows::Win32::System::TpmBaseServices::{TPM_DEVICE_INFO, Tbsi_GetDeviceInfo};
use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
use windows::core::{Array, HSTRING};
use windows_future::IAsyncOperation;
use winshell::args::Args;
use winshell::text::hex;
use winshell::win::init_winrt;
use winshell::{CHALLENGE, CREDENTIAL_NAME};

fn main() {
    init_winrt();
    let args = Args::parse(std::env::args().skip(1));
    let out = match args.command.as_deref() {
        Some("supported") => supported(),
        Some("create") => create(),
        Some("sign") => sign(args.num("count", 5)),
        Some("tpm") => tpm(),
        Some("verify") => verify(),
        Some("delete") => delete(),
        Some("exists") => exists(),
        Some("dpapi") => dpapi(),
        other => json!({ "error": format!("unknown command {other:?}") }),
    };
    println!("{out}");
}

fn e(err: windows::core::Error) -> Value {
    json!(format!("{err}"))
}

fn status_name(s: KeyCredentialStatus) -> &'static str {
    match s {
        KeyCredentialStatus::Success => "Success",
        KeyCredentialStatus::UnknownError => "UnknownError",
        KeyCredentialStatus::NotFound => "NotFound",
        KeyCredentialStatus::UserCanceled => "UserCanceled",
        KeyCredentialStatus::UserPrefersPassword => "UserPrefersPassword",
        KeyCredentialStatus::CredentialAlreadyExists => "CredentialAlreadyExists",
        KeyCredentialStatus::SecurityDeviceLocked => "SecurityDeviceLocked",
        _ => "Unknown",
    }
}

fn supported() -> Value {
    match KeyCredentialManager::IsSupportedAsync().and_then(|op| op.join()) {
        Ok(b) => json!({ "check": "supported", "supported": b }),
        Err(err) => json!({ "check": "supported", "supported": false, "error": e(err) }),
    }
}

fn create() -> Value {
    let r = KeyCredentialManager::RequestCreateAsync(
        &HSTRING::from(CREDENTIAL_NAME),
        KeyCredentialCreationOption::ReplaceExisting,
    )
    .and_then(|op| op.join())
    .and_then(|r| r.Status());
    match r {
        Ok(s) => json!({ "check": "create", "status": status_name(s) }),
        Err(err) => json!({ "check": "create", "status": "error", "error": e(err) }),
    }
}

fn sign(count: u64) -> Value {
    let opened =
        KeyCredentialManager::OpenAsync(&HSTRING::from(CREDENTIAL_NAME)).and_then(|op| op.join());
    let result = match opened {
        Ok(r) => r,
        Err(err) => return json!({ "check": "sign", "error": e(err) }),
    };
    let status = result.Status().map(status_name).unwrap_or("error");
    if status != "Success" {
        return json!({ "check": "sign", "open_status": status, "signatures": [] });
    }
    let credential = match result.Credential() {
        Ok(c) => c,
        Err(err) => return json!({ "check": "sign", "error": e(err) }),
    };
    let mut signatures = Vec::new();
    for _ in 0..count {
        let one = (|| -> windows::core::Result<Value> {
            let challenge = CryptographicBuffer::CreateFromByteArray(&CHALLENGE)?;
            let op = credential.RequestSignAsync(&challenge)?.join()?;
            let st = status_name(op.Status()?);
            if st != "Success" {
                return Ok(json!({ "status": st }));
            }
            let mut bytes = Array::<u8>::new();
            CryptographicBuffer::CopyToByteArray(&op.Result()?, &mut bytes)?;
            Ok(json!({ "status": st, "signature_hex": hex(&bytes) }))
        })();
        signatures.push(one.unwrap_or_else(|err| json!({ "status": "error", "error": e(err) })));
    }
    json!({
        "check": "sign", "open_status": status, "challenge_hex": hex(&CHALLENGE),
        "signatures": signatures,
    })
}

fn tpm() -> Value {
    let mut info = TPM_DEVICE_INFO {
        structVersion: 1, // TPM_VERSION_1 of the structure
        ..Default::default()
    };
    // SAFETY: the structure is sized and owned by us.
    let rc = unsafe {
        Tbsi_GetDeviceInfo(
            size_of::<TPM_DEVICE_INFO>() as u32,
            &mut info as *mut _ as *mut _,
        )
    };
    let version = match (rc, info.tpmVersion) {
        (0, 1) => "1.2",
        (0, 2) => "2.0",
        (0, _) => "other",
        _ => "none",
    };
    json!({
        "check": "tpm", "tbs_result": format!("0x{rc:08x}"),
        "tpm_version": version, "tpm_2_0": rc == 0 && info.tpmVersion == 2,
    })
}

fn verification_name(r: UserConsentVerificationResult) -> &'static str {
    match r {
        UserConsentVerificationResult::Verified => "Verified",
        UserConsentVerificationResult::DeviceNotPresent => "DeviceNotPresent",
        UserConsentVerificationResult::NotConfiguredForUser => "NotConfiguredForUser",
        UserConsentVerificationResult::DisabledByPolicy => "DisabledByPolicy",
        UserConsentVerificationResult::DeviceBusy => "DeviceBusy",
        UserConsentVerificationResult::RetriesExhausted => "RetriesExhausted",
        UserConsentVerificationResult::Canceled => "Canceled",
        _ => "Unknown",
    }
}

fn verify() -> Value {
    // SAFETY: GetConsoleWindow is a plain query; the interop call gets a valid HWND (or null)
    // and a message HSTRING that outlives it.
    let r = (|| -> windows::core::Result<UserConsentVerificationResult> {
        let interop = windows::core::factory::<UserConsentVerifier, IUserConsentVerifierInterop>()?;
        let hwnd = unsafe { GetConsoleWindow() };
        let op: IAsyncOperation<UserConsentVerificationResult> = unsafe {
            interop.RequestVerificationForWindowAsync(
                hwnd,
                &HSTRING::from("LILITH spike 11: confirm it is you (S1.8 e)"),
            )?
        };
        op.join()
    })();
    // SAFETY: plain query.
    let hwnd = unsafe { GetConsoleWindow() }.0 as isize;
    match r {
        Ok(v) => json!({ "check": "verify", "console_hwnd": hwnd, "result": verification_name(v) }),
        Err(err) => {
            json!({ "check": "verify", "console_hwnd": hwnd, "result": "error", "error": e(err) })
        }
    }
}

fn delete() -> Value {
    match KeyCredentialManager::DeleteAsync(&HSTRING::from(CREDENTIAL_NAME))
        .and_then(|op| op.join())
    {
        Ok(()) => json!({ "check": "delete", "deleted": true }),
        Err(err) => json!({ "check": "delete", "deleted": false, "error": e(err) }),
    }
}

fn exists() -> Value {
    let r = KeyCredentialManager::OpenAsync(&HSTRING::from(CREDENTIAL_NAME))
        .and_then(|op| op.join())
        .and_then(|r| r.Status());
    match r {
        Ok(s) => {
            json!({ "check": "exists", "status": status_name(s), "exists": s == KeyCredentialStatus::Success })
        }
        Err(err) => json!({ "check": "exists", "status": "error", "error": e(err) }),
    }
}

fn dpapi() -> Value {
    let plain = b"LILITH spike 11 DPAPI round trip".to_vec();
    // SAFETY: input blobs point into live buffers; output blobs are LocalAlloc'd by the API
    // and freed here after copying.
    let r = unsafe {
        (|| -> windows::core::Result<Vec<u8>> {
            let input = CRYPT_INTEGER_BLOB {
                cbData: plain.len() as u32,
                pbData: plain.as_ptr() as *mut u8,
            };
            let mut sealed = CRYPT_INTEGER_BLOB::default();
            CryptProtectData(
                &input,
                &HSTRING::from("LILITH spike"),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut sealed,
            )?;
            let mut opened = CRYPT_INTEGER_BLOB::default();
            let res = CryptUnprotectData(
                &sealed,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut opened,
            );
            let _ = LocalFree(Some(HLOCAL(sealed.pbData as *mut _)));
            res?;
            let out = std::slice::from_raw_parts(opened.pbData, opened.cbData as usize).to_vec();
            let _ = LocalFree(Some(HLOCAL(opened.pbData as *mut _)));
            Ok(out)
        })()
    };
    match r {
        Ok(out) => json!({ "check": "dpapi", "round_trip": out == plain }),
        Err(err) => json!({ "check": "dpapi", "round_trip": false, "error": e(err) }),
    }
}
