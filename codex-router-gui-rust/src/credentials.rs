//! Minimal Windows Credential Manager access for headless Router Host.

use anyhow::{bail, Context, Result};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};
use zeroize::Zeroizing;

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn read_text(name: &str) -> Result<Option<Zeroizing<String>>> {
    let target = wide(&format!("CodexRouter/{name}"));
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
    if name.trim().is_empty() || name.contains('\0') {
        bail!("Windows credential name is invalid");
    }
    let mut target = wide(&format!("CodexRouter/{name}"));
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
    let target = wide(&format!("CodexRouter/{name}"));
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
