// DPAPI helpers are reserved for a future credential-at-rest vault. They are
// currently unused, so silence the dead-code lint while keeping the module.
#![allow(dead_code)]

use std::ptr;
use windows_sys::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

pub fn encrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    let data_blob = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };

    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };

    let result = unsafe {
        CryptProtectData(
            &data_blob,
            ptr::null(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
    };

    if result == 0 {
        return Err("DPAPI encryption failed".into());
    }

    let encrypted =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec() };

    unsafe {
        windows_sys::Win32::Foundation::LocalFree(out_blob.pbData as _);
    }

    Ok(encrypted)
}

pub fn decrypt(encrypted: &[u8]) -> Result<Vec<u8>, String> {
    let data_blob = CRYPT_INTEGER_BLOB {
        cbData: encrypted.len() as u32,
        pbData: encrypted.as_ptr() as *mut u8,
    };

    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };

    let result = unsafe {
        CryptUnprotectData(
            &data_blob,
            ptr::null_mut(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
    };

    if result == 0 {
        return Err("DPAPI decryption failed".into());
    }

    let decrypted =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec() };

    unsafe {
        windows_sys::Win32::Foundation::LocalFree(out_blob.pbData as _);
    }

    Ok(decrypted)
}
