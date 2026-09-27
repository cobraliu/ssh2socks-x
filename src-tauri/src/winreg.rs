//! Minimal HKEY_CURRENT_USER access for the login item and system proxy.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD,
    RRF_RT_REG_SZ,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn err(code: u32) -> String {
    std::io::Error::from_raw_os_error(code as i32).to_string()
}

pub fn get_string(key: &str, name: &str) -> Option<String> {
    let (key, name) = (wide(key), wide(name));
    let mut size = 0u32;
    // SAFETY: a size query with null data, then a read into a buffer of
    // that size; both strings are NUL-terminated.
    unsafe {
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        );
        if status != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2) + 1];
        let mut size = (buf.len() * 2) as u32;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut c_void,
            &mut size,
        );
        if status != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

pub fn get_dword(key: &str, name: &str) -> Option<u32> {
    let (key, name) = (wide(key), wide(name));
    let mut value = 0u32;
    let mut size = 4u32;
    // SAFETY: reads at most 4 bytes into `value`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut value as *mut u32 as *mut c_void,
            &mut size,
        )
    };
    (status == ERROR_SUCCESS).then_some(value)
}

fn set_raw(key: &str, name: &str, kind: u32, data: &[u8]) -> Result<(), String> {
    let (key, name) = (wide(key), wide(name));
    let mut handle: HKEY = std::ptr::null_mut();
    // SAFETY: opens (or creates) the key, writes `data`, closes the handle.
    unsafe {
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut handle,
            std::ptr::null_mut(),
        );
        if status != ERROR_SUCCESS {
            return Err(err(status));
        }
        let status = RegSetValueExW(
            handle,
            name.as_ptr(),
            0,
            kind,
            data.as_ptr(),
            data.len() as u32,
        );
        RegCloseKey(handle);
        if status != ERROR_SUCCESS {
            return Err(err(status));
        }
    }
    Ok(())
}

pub fn set_string(key: &str, name: &str, value: &str) -> Result<(), String> {
    let data: Vec<u8> = wide(value).iter().flat_map(|c| c.to_le_bytes()).collect();
    set_raw(key, name, REG_SZ, &data)
}

pub fn set_dword(key: &str, name: &str, value: u32) -> Result<(), String> {
    set_raw(key, name, REG_DWORD, &value.to_le_bytes())
}

/// Deleting a value that does not exist is fine.
pub fn delete(key: &str, name: &str) -> Result<(), String> {
    let (key, name) = (wide(key), wide(name));
    // SAFETY: both strings are NUL-terminated.
    let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
    match status {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        e => Err(err(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_values() {
        let key = format!("Software\\ssh2socks-test-{}", std::process::id());
        assert_eq!(get_string(&key, "s"), None);
        set_string(&key, "s", "socks=127.0.0.1:1080").unwrap();
        set_dword(&key, "d", 1).unwrap();
        assert_eq!(
            get_string(&key, "s").as_deref(),
            Some("socks=127.0.0.1:1080")
        );
        assert_eq!(get_dword(&key, "d"), Some(1));
        assert_eq!(get_dword(&key, "s"), None);
        delete(&key, "s").unwrap();
        delete(&key, "s").unwrap();
        delete(&key, "d").unwrap();
        assert_eq!(get_string(&key, "s"), None);
        let w = wide(&key);
        // SAFETY: NUL-terminated key name under HKCU.
        unsafe {
            windows_sys::Win32::System::Registry::RegDeleteTreeW(HKEY_CURRENT_USER, w.as_ptr())
        };
    }
}
