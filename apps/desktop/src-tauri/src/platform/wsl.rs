//! Installed WSL distributions, read from `HKCU\...\Lxss` — the same source
//! Windows Terminal uses. Reading the registry rather than running
//! `wsl.exe --list` keeps shell detection free of a process spawn, which
//! matters because detection runs on every local terminal launch.

use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    REG_DWORD, REG_SZ,
};

const LXSS_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";
/// `State` of a distribution that has finished installing. Others are mid
/// install, conversion or removal and cannot be started.
const STATE_INSTALLED: u32 = 1;
const MAX_KEY_NAME_CHARS: usize = 256;
const MAX_VALUE_BYTES: u32 = 4096;
/// Bounds the enumeration; nobody has this many distributions.
const MAX_DISTRIBUTIONS: u32 = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distribution {
    pub name: String,
    pub is_default: bool,
}

/// Installed distributions, the default first. Docker Desktop's internal
/// distributions are skipped: they are not shells anyone means to open.
pub fn distributions() -> Vec<Distribution> {
    distributions_under(LXSS_PATH)
}

fn distributions_under(path: &str) -> Vec<Distribution> {
    let Some(root) = open(HKEY_CURRENT_USER, &wide(path)) else {
        return Vec::new();
    };
    let default_guid = read_string(root.0, "DefaultDistribution");
    let mut distributions = Vec::new();
    let mut name = [0u16; MAX_KEY_NAME_CHARS];
    for index in 0..MAX_DISTRIBUTIONS {
        // Reset every iteration: the callee overwrites it with the actual length.
        let mut length = name.len() as u32;
        // SAFETY: `name` is MAX_KEY_NAME_CHARS long and `length` says so; the
        // optional out-parameters are null.
        let status = unsafe {
            RegEnumKeyExW(
                root.0,
                index,
                name.as_mut_ptr(),
                &mut length as *mut u32,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != ERROR_SUCCESS {
            tracing::warn!(index, status, "skipping unreadable WSL distribution key");
            continue;
        }
        let length = (length as usize).min(name.len());
        let guid = String::from_utf16_lossy(&name[..length]);
        // `name` is still NUL-terminated from the enumeration.
        let Some(subkey) = open(root.0, &name[..=length]) else {
            continue;
        };
        if read_dword(subkey.0, "State") != Some(STATE_INSTALLED) {
            continue;
        }
        let Some(distribution_name) = read_string(subkey.0, "DistributionName") else {
            continue;
        };
        let distribution_name = distribution_name.trim().to_string();
        if distribution_name.is_empty()
            || distribution_name
                .to_ascii_lowercase()
                .starts_with("docker-desktop")
        {
            continue;
        }
        let is_default = default_guid
            .as_deref()
            .is_some_and(|default| default.eq_ignore_ascii_case(&guid));
        distributions.push(Distribution {
            name: distribution_name,
            is_default,
        });
    }
    // Stable: registry order otherwise, which is install order.
    distributions.sort_by_key(|distribution| !distribution.is_default);
    distributions
}

/// Closes its key on drop so an early return cannot leak a handle.
struct RegKey(HKEY);

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: self.0 came from a successful RegOpenKeyExW and is closed
        // exactly once, here.
        unsafe { RegCloseKey(self.0) };
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `None` covers both "WSL never installed" (the key is absent) and a key we
/// could not read; either way there is nothing to offer.
fn open(parent: HKEY, path: &[u16]) -> Option<RegKey> {
    let mut handle: HKEY = ptr::null_mut();
    // SAFETY: `path` is NUL-terminated and outlives the call; `handle` is only
    // written on success. Read-only access is requested.
    let status =
        unsafe { RegOpenKeyExW(parent, path.as_ptr(), 0, KEY_READ, &mut handle as *mut HKEY) };
    (status == ERROR_SUCCESS).then_some(RegKey(handle))
}

fn read_raw(key: HKEY, name: &str, expected_type: u32) -> Option<Vec<u8>> {
    let name = wide(name);
    let mut value_type = 0u32;
    let mut size = 0u32;
    // First call sizes the buffer; `lpcbData` counts bytes.
    // SAFETY: all pointers reference locals that outlive the call.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            ptr::null_mut(),
            &mut value_type as *mut u32,
            ptr::null_mut(),
            &mut size as *mut u32,
        )
    };
    if status != ERROR_SUCCESS || value_type != expected_type || size == 0 || size > MAX_VALUE_BYTES
    {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    // SAFETY: `buffer` is `size` bytes long, which is what `size` declares.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            ptr::null_mut(),
            &mut value_type as *mut u32,
            buffer.as_mut_ptr(),
            &mut size as *mut u32,
        )
    };
    if status != ERROR_SUCCESS || value_type != expected_type {
        return None;
    }
    buffer.truncate(size as usize);
    Some(buffer)
}

fn read_string(key: HKEY, name: &str) -> Option<String> {
    let bytes = read_raw(key, name, REG_SZ)?;
    // A Vec<u8> is only byte-aligned, so decode pairs rather than casting.
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    Some(
        String::from_utf16_lossy(&units)
            .trim_end_matches('\0')
            .to_string(),
    )
}

fn read_dword(key: HKEY, name: &str) -> Option<u32> {
    let bytes: [u8; 4] = read_raw(key, name, REG_DWORD)?.try_into().ok()?;
    Some(u32::from_ne_bytes(bytes))
}

/// Runs the enumeration against a key the test owns under Luma's namespace;
/// the real Lxss key is never written to.
#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::{
        RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, KEY_WRITE, REG_OPTION_NON_VOLATILE,
    };

    struct TempKey(String);

    impl TempKey {
        fn new() -> Self {
            Self(format!(r"Software\Luma\wsl-test-{}", uuid::Uuid::new_v4()))
        }

        fn create(&self, subkey: &str) -> RegKey {
            let path = if subkey.is_empty() {
                wide(&self.0)
            } else {
                wide(&format!(r"{}\{subkey}", self.0))
            };
            let mut handle: HKEY = ptr::null_mut();
            // SAFETY: `path` is NUL-terminated; the optional parameters are null.
            let status = unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    path.as_ptr(),
                    0,
                    ptr::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_WRITE | KEY_READ,
                    ptr::null(),
                    &mut handle as *mut HKEY,
                    ptr::null_mut(),
                )
            };
            assert_eq!(status, ERROR_SUCCESS);
            RegKey(handle)
        }
    }

    impl Drop for TempKey {
        fn drop(&mut self) {
            let path = wide(&self.0);
            // SAFETY: `path` names a key this test created.
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) };
        }
    }

    fn set_string(key: &RegKey, name: &str, value: &str) {
        let name = wide(name);
        let bytes: Vec<u8> = wide(value)
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        // SAFETY: `bytes` is `bytes.len()` long, which is what is declared.
        let status = unsafe {
            RegSetValueExW(
                key.0,
                name.as_ptr(),
                0,
                REG_SZ,
                bytes.as_ptr(),
                bytes.len() as u32,
            )
        };
        assert_eq!(status, ERROR_SUCCESS);
    }

    fn set_dword(key: &RegKey, name: &str, value: u32) {
        let name = wide(name);
        let bytes = value.to_ne_bytes();
        // SAFETY: a REG_DWORD is exactly 4 bytes.
        let status =
            unsafe { RegSetValueExW(key.0, name.as_ptr(), 0, REG_DWORD, bytes.as_ptr(), 4) };
        assert_eq!(status, ERROR_SUCCESS);
    }

    fn distribution(root: &TempKey, guid: &str, name: &str, state: u32) {
        let key = root.create(guid);
        set_string(&key, "DistributionName", name);
        set_dword(&key, "State", state);
    }

    #[test]
    fn lists_installed_distributions_default_first() {
        let root = TempKey::new();
        distribution(&root, "{1}", "kali-linux", STATE_INSTALLED);
        distribution(&root, "{2}", "docker-desktop", STATE_INSTALLED);
        distribution(&root, "{3}", "Ubuntu-24.04", STATE_INSTALLED);
        distribution(&root, "{4}", "Debian", 3);
        set_string(&root.create(""), "DefaultDistribution", "{3}");

        assert_eq!(
            distributions_under(&root.0),
            vec![
                Distribution {
                    name: "Ubuntu-24.04".into(),
                    is_default: true,
                },
                Distribution {
                    name: "kali-linux".into(),
                    is_default: false,
                },
            ]
        );
    }

    #[test]
    fn a_missing_key_means_no_distributions() {
        assert!(distributions_under(r"Software\Luma\definitely-not-here").is_empty());
    }
}
