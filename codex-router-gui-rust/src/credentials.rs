//! Minimal Windows Credential Manager access for headless Router Host.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};
use zeroize::Zeroizing;

/// UserData-scoped prefix so CodexRouter keys do not collide with CraftStation
/// or another Router copy that uses a different state root. Empty = legacy
/// `CodexRouter/{name}` targets (tests and pre-scope processes).
static CREDENTIAL_SCOPE: OnceLock<String> = OnceLock::new();

/// Pin Windows credential names to this installation's UserData root.
/// Safe to call more than once; the first non-empty scope wins.
pub fn set_scope_from_root(router_root: &Path) {
    let scope = credential_scope(router_root);
    if scope.is_empty() {
        return;
    }
    let _ = CREDENTIAL_SCOPE.set(scope);
}

fn credential_scope(router_root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let root = credential_state_root(router_root);
    let canonical = std::fs::canonicalize(&root).unwrap_or(root);
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    digest
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn credential_state_root(router_root: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("CODEX_ROUTER_USER_DATA_ROOT") {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return path;
        }
    }
    if std::env::var_os("CODEX_ROUTER_PORTABLE_STATE").is_some_and(|value| value == "1") {
        return router_root.to_path_buf();
    }
    if router_root.join("release-manifest.json").is_file() {
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local_app_data)
                .join("Codex-Router")
                .join("UserData");
        }
    }
    router_root.to_path_buf()
}

pub fn wincred_target(name: &str) -> String {
    match CREDENTIAL_SCOPE.get() {
        Some(scope) if !scope.is_empty() => format!("CodexRouter/{scope}/{name}"),
        _ => format!("CodexRouter/{name}"),
    }
}

pub fn legacy_wincred_target(name: &str) -> String {
    format!("CodexRouter/{name}")
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn environment_override(name: &str) -> Option<&'static str> {
    match name {
        "AdminPassword" => Some("CODEX_ROUTER_ADMIN_PASSWORD"),
        "LocalApiKey" => Some("CODEX_ROUTER_LOCAL_API_KEY"),
        "CliManagementSecret" => Some("CODEX_ROUTER_CLI_MANAGEMENT_SECRET"),
        _ => None,
    }
}

pub fn read_text(name: &str) -> Result<Option<Zeroizing<String>>> {
    if let Some(value) = environment_override(name)
        .and_then(|variable| std::env::var(variable).ok())
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(Some(Zeroizing::new(value)));
    }
    if let Some(value) = read_target(&wincred_target(name))? {
        return Ok(Some(value));
    }
    let scoped = CREDENTIAL_SCOPE
        .get()
        .is_some_and(|scope| !scope.is_empty());
    if scoped {
        if let Some(value) = read_target(&legacy_wincred_target(name))? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn read_target(target_name: &str) -> Result<Option<Zeroizing<String>>> {
    let target = wide(target_name);
    let mut credential: *mut CREDENTIALW = null_mut();
    let found = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if found == 0 {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
            Ok(None)
        } else {
            Err(error).context("Windows Credential Manager read failed")
        };
    }
    if credential.is_null() {
        bail!("Windows Credential Manager returned an empty record");
    }
    let result = unsafe {
        let record = &*credential;
        if !record.CredentialBlobSize.is_multiple_of(2) || record.CredentialBlob.is_null() {
            bail!("Windows credential contains invalid UTF-16 data");
        }
        let units = std::slice::from_raw_parts(
            record.CredentialBlob.cast::<u16>(),
            record.CredentialBlobSize as usize / 2,
        );
        let end = units
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units.len());
        String::from_utf16(&units[..end]).map(Zeroizing::new)
    };
    unsafe { CredFree(credential.cast()) };
    Ok(Some(
        result.context("Windows credential contains invalid UTF-16")?,
    ))
}

pub fn write_text(name: &str, secret: &str) -> Result<()> {
    if environment_override(name).is_some_and(|variable| std::env::var_os(variable).is_some()) {
        bail!("environment-overridden credential is read-only");
    }
    if name.trim().is_empty() || name.contains('\0') {
        bail!("Windows credential name is invalid");
    }
    let mut target = wide(&wincred_target(name));
    let mut username = wide(&std::env::var("USERNAME").unwrap_or_default());
    let mut secret: Vec<u16> = secret.encode_utf16().collect();
    let blob_size = u32::try_from(secret.len().saturating_mul(std::mem::size_of::<u16>()))
        .context("Windows credential is too large")?;
    if blob_size > 2560 {
        bail!("Windows credential exceeds 2560 bytes");
    }
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        CredentialBlobSize: blob_size,
        CredentialBlob: secret.as_ptr().cast_mut().cast::<u8>(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: username.as_mut_ptr(),
        ..Default::default()
    };
    let written = unsafe { CredWriteW(&credential, 0) } != 0;
    secret.fill(0);
    if !written {
        return Err(std::io::Error::last_os_error())
            .context("Windows Credential Manager write failed");
    }
    Ok(())
}

pub fn delete_text(name: &str) -> Result<()> {
    delete_target(&wincred_target(name))?;
    if CREDENTIAL_SCOPE
        .get()
        .is_some_and(|scope| !scope.is_empty())
    {
        delete_target(&legacy_wincred_target(name))?;
    }
    Ok(())
}

fn delete_target(target_name: &str) -> Result<()> {
    let target = wide(target_name);
    let deleted = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0;
    if !deleted {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
            return Ok(());
        }
        return Err(error).context("Windows Credential Manager delete failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_host_runtime_credentials_have_environment_overrides() {
        assert_eq!(
            environment_override("AdminPassword"),
            Some("CODEX_ROUTER_ADMIN_PASSWORD")
        );
        assert_eq!(
            environment_override("LocalApiKey"),
            Some("CODEX_ROUTER_LOCAL_API_KEY")
        );
        assert_eq!(
            environment_override("CliManagementSecret"),
            Some("CODEX_ROUTER_CLI_MANAGEMENT_SECRET")
        );
        assert_eq!(environment_override("AccountKey-1"), None);
    }

    #[test]
    fn unscope_targets_keep_the_legacy_codex_router_prefix() {
        assert_eq!(wincred_target("LocalApiKey"), "CodexRouter/LocalApiKey");
        assert_eq!(
            legacy_wincred_target("LocalApiKey"),
            "CodexRouter/LocalApiKey"
        );
    }
}
