//! PuTTY saved session discovery.
//!
//! PuTTY does not keep sessions in a config file the way Tabby or Electerm do.
//! On Windows they live under `HKCU\Software\SimonTatham\PuTTY\Sessions`; the
//! Unix port writes one file per session under `~/.putty/sessions`. Both are
//! read here, and so is a `regedit`-exported `.reg` file, which is how someone
//! moves their sessions off an old machine.
//!
//! Nothing here is written back — PuTTY's own configuration is never modified.

use std::collections::HashSet;
use std::path::PathBuf;

use super::{auth_hint, concrete_identity_path, push_candidate, trimmed, ParsedCandidate};
use crate::platform::home_dir;

/// PuTTY's own limit on a registry key name, and a sane ceiling for a value.
const MAX_VALUE_BYTES: u32 = 4096;
const MAX_SESSION_FILE_BYTES: u64 = 64 * 1024;
/// PuTTY's template for new sessions, not a real host.
const DEFAULT_SETTINGS: &str = "Default Settings";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PuttySession {
    pub name: String,
    pub host_name: Option<String>,
    pub port: Option<u16>,
    pub user_name: Option<String>,
    pub protocol: Option<String>,
    pub public_key_file: Option<String>,
    pub try_agent: Option<bool>,
}

enum RegValue {
    Text(String),
    Dword(u32),
}

impl PuttySession {
    fn new(name: String) -> Self {
        Self {
            name,
            ..Default::default()
        }
    }

    fn set(&mut self, key: &str, value: RegValue) {
        match (key, value) {
            ("HostName", RegValue::Text(text)) => self.host_name = Some(text),
            ("UserName", RegValue::Text(text)) => self.user_name = Some(text),
            ("Protocol", RegValue::Text(text)) => self.protocol = Some(text),
            ("PublicKeyFile", RegValue::Text(text)) => self.public_key_file = Some(text),
            ("PortNumber", RegValue::Dword(number)) => self.port = u16::try_from(number).ok(),
            ("TryAgent", RegValue::Dword(number)) => self.try_agent = Some(number != 0),
            _ => {}
        }
    }

    /// A missing `Protocol` means SSH: it is PuTTY's default and older exports
    /// frequently omit it.
    fn is_ssh(&self) -> bool {
        match self.protocol.as_deref() {
            None => true,
            Some(protocol) => {
                let protocol = protocol.trim().to_ascii_lowercase();
                protocol.is_empty() || protocol == "ssh" || protocol == "ssh-connection"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Session name escaping
// ---------------------------------------------------------------------------

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Reverse PuTTY's `mungestr`, which percent-escapes spaces, backslashes,
/// wildcards, `%` itself, anything outside printable ASCII, and a leading dot.
/// A `%` that is not followed by two hex digits is literal.
pub(crate) fn unmunge(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 3 <= bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
            {
                out.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    // Modern PuTTY writes UTF-8 session names; older builds wrote the system
    // codepage, which we cannot recover, so replace rather than fail.
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Windows registry
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod registry {
    use super::{PuttySession, RegValue, DEFAULT_SETTINGS, MAX_VALUE_BYTES};
    use std::ptr;
    use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_SZ,
    };

    const SESSIONS_PATH: &str = r"Software\SimonTatham\PuTTY\Sessions";
    /// The documented maximum length of a registry key name.
    const MAX_KEY_NAME_CHARS: usize = 256;

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

    fn open(parent: HKEY, path: &[u16]) -> Option<RegKey> {
        let mut handle: HKEY = ptr::null_mut();
        // SAFETY: `path` is NUL-terminated and outlives the call; `handle` is a
        // local the callee only writes on success. Read-only access is requested.
        let status =
            unsafe { RegOpenKeyExW(parent, path.as_ptr(), 0, KEY_READ, &mut handle as *mut HKEY) };
        (status == ERROR_SUCCESS).then_some(RegKey(handle))
    }

    fn query_value(key: HKEY, name: &str) -> Option<RegValue> {
        let name = wide(name);
        let mut value_type = 0u32;
        let mut size = 0u32;
        // First call sizes the buffer; `lpcbData` counts bytes, not characters.
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
        if status != ERROR_SUCCESS || size == 0 || size > MAX_VALUE_BYTES {
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        // SAFETY: `buffer` is `size` bytes long, which is what `size` tells the
        // callee it may write.
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
        if status == ERROR_MORE_DATA || status != ERROR_SUCCESS {
            return None;
        }
        let bytes = buffer.get(..size as usize)?;
        match value_type {
            REG_SZ | REG_EXPAND_SZ => {
                // A Vec<u8> is only byte-aligned, so decode pairs rather than
                // casting the pointer to *const u16.
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                let text = String::from_utf16_lossy(&units);
                Some(RegValue::Text(text.trim_end_matches('\0').to_string()))
            }
            REG_DWORD if bytes.len() == 4 => Some(RegValue::Dword(u32::from_ne_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3],
            ]))),
            _ => None,
        }
    }

    const VALUE_NAMES: [&str; 6] = [
        "HostName",
        "PortNumber",
        "UserName",
        "Protocol",
        "PublicKeyFile",
        "TryAgent",
    ];

    pub(super) fn sessions(limit: usize) -> Vec<PuttySession> {
        sessions_under(SESSIONS_PATH, limit)
    }

    /// Split out from `sessions` so the enumeration can be exercised against a
    /// key the test owns instead of PuTTY's.
    fn sessions_under(path: &str, limit: usize) -> Vec<PuttySession> {
        let Some(root) = open(HKEY_CURRENT_USER, &wide(path)) else {
            // PuTTY not installed, or no sessions saved. Not an error.
            return Vec::new();
        };
        let mut sessions = Vec::new();
        let mut name = [0u16; MAX_KEY_NAME_CHARS];
        for index in 0..limit as u32 {
            // `length` is reset every iteration: on output the callee overwrites
            // it with the actual length, so reusing the previous value would
            // truncate every name after the first.
            let mut length = name.len() as u32;
            // SAFETY: `name` is MAX_KEY_NAME_CHARS long and `length` says so;
            // the optional out-parameters are all null.
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
            if status != ERROR_SUCCESS {
                // ERROR_NO_MORE_ITEMS, or anything unexpected: stop cleanly
                // rather than fail the whole import.
                break;
            }
            let length = (length as usize).min(name.len());
            let munged = String::from_utf16_lossy(&name[..length]);
            if munged.is_empty() {
                continue;
            }
            let session_name = super::unmunge(&munged);
            if session_name == DEFAULT_SETTINGS {
                continue;
            }
            // `name` is still NUL-terminated from the enumeration, so it can be
            // handed straight back as a subkey path.
            let Some(subkey) = open(root.0, &name[..=length]) else {
                continue;
            };
            let mut session = PuttySession::new(session_name);
            for value_name in VALUE_NAMES {
                if let Some(value) = query_value(subkey.0, value_name) {
                    session.set(value_name, value);
                }
            }
            sessions.push(session);
        }
        sessions
    }

    /// Exercises the enumeration FFI for real: every other test in this file
    /// feeds the parser text, which leaves the unsafe registry walk — the
    /// name-length reset, the UTF-16 decoding, the handle lifetimes — with no
    /// coverage at all. The test owns its own key under Luma's namespace and
    /// removes it again; PuTTY's keys are never written to.
    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::System::Registry::{
            RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, KEY_WRITE, REG_OPTION_NON_VOLATILE,
        };

        struct TempKey(String);

        impl TempKey {
            fn new() -> Self {
                Self(format!(
                    r"Software\Luma\putty-import-test-{}",
                    uuid::Uuid::new_v4()
                ))
            }

            fn create(&self, subkey: &str) -> RegKey {
                let path = wide(&format!(r"{}\{subkey}", self.0));
                let mut handle: HKEY = ptr::null_mut();
                // SAFETY: `path` is NUL-terminated; the security-attributes and
                // disposition out-parameters are optional and passed as null.
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
                assert_eq!(status, ERROR_SUCCESS, "could not create {path:?}");
                RegKey(handle)
            }
        }

        impl Drop for TempKey {
            fn drop(&mut self) {
                let path = wide(&self.0);
                // SAFETY: `path` is NUL-terminated and names a key this test
                // created under its own namespace.
                unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) };
            }
        }

        fn set_string(key: &RegKey, name: &str, value: &str) {
            let name = wide(name);
            let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes: Vec<u8> = data.iter().flat_map(|unit| unit.to_le_bytes()).collect();
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
            // SAFETY: a REG_DWORD is exactly 4 bytes, which `bytes` is.
            let status =
                unsafe { RegSetValueExW(key.0, name.as_ptr(), 0, REG_DWORD, bytes.as_ptr(), 4) };
            assert_eq!(status, ERROR_SUCCESS);
        }

        #[test]
        fn enumerates_sessions_from_the_registry() {
            let root = TempKey::new();
            let sessions_path = format!(r"{}\Sessions", root.0);

            // A long name and a short one: if the enumeration forgets to reset
            // the name-length parameter, the second comes back truncated.
            let long = root.create(r"Sessions\a%20very%20long%20session%20name%20for%20testing");
            set_string(&long, "HostName", "long.example.com");
            set_string(&long, "Protocol", "ssh");
            set_dword(&long, "PortNumber", 2222);

            let short = root.create(r"Sessions\db");
            set_string(&short, "HostName", "db.internal");
            set_string(&short, "UserName", "deploy");
            set_string(&short, "PublicKeyFile", r"C:\keys\db.ppk");
            set_dword(&short, "TryAgent", 1);

            let skipped = root.create(r"Sessions\Default%20Settings");
            set_string(&skipped, "HostName", "template.example.com");

            let mut sessions = sessions_under(&sessions_path, 500);
            sessions.sort_by(|a, b| a.name.cmp(&b.name));

            assert_eq!(sessions.len(), 2, "Default Settings is not a session");
            assert_eq!(sessions[0].name, "a very long session name for testing");
            assert_eq!(sessions[0].host_name.as_deref(), Some("long.example.com"));
            assert_eq!(sessions[0].port, Some(2222));
            assert_eq!(sessions[1].name, "db");
            assert_eq!(sessions[1].host_name.as_deref(), Some("db.internal"));
            assert_eq!(sessions[1].user_name.as_deref(), Some("deploy"));
            assert_eq!(
                sessions[1].public_key_file.as_deref(),
                Some(r"C:\keys\db.ppk")
            );
            assert_eq!(sessions[1].try_agent, Some(true));
        }

        #[test]
        fn a_missing_key_is_not_an_error() {
            // PuTTY not installed is the common case and must stay silent.
            assert!(sessions_under(r"Software\Luma\definitely-not-here", 500).is_empty());
        }
    }
}

#[cfg(not(windows))]
mod registry {
    use super::PuttySession;

    pub(super) fn sessions(_limit: usize) -> Vec<PuttySession> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Unix session files
// ---------------------------------------------------------------------------

fn putty_directory() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os("PUTTYDIR") {
        if !directory.is_empty() {
            return Some(PathBuf::from(directory));
        }
    }
    Some(home_dir()?.join(".putty"))
}

/// Each file is one session: the name is the munged filename, the body is
/// `Key=Value` lines.
fn parse_session_file(name: String, contents: &str) -> PuttySession {
    let mut session = PuttySession::new(name);
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        // The file format is untyped, so numeric values arrive as decimal text.
        let parsed = match key {
            "PortNumber" | "TryAgent" => match value.parse::<u32>() {
                Ok(number) => RegValue::Dword(number),
                Err(_) => continue,
            },
            _ => RegValue::Text(value.to_string()),
        };
        session.set(key, parsed);
    }
    session
}

fn file_sessions(limit: usize) -> Vec<PuttySession> {
    let Some(directory) = putty_directory() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(directory.join("sessions")) else {
        return Vec::new();
    };
    // Sorted so the 500-entry cap takes a deterministic slice rather than
    // whatever order the filesystem happened to hand back.
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    paths.sort();

    let mut sessions = Vec::new();
    for path in paths.into_iter().take(limit) {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name.starts_with('.') {
            continue;
        }
        let name = unmunge(file_name);
        if name == DEFAULT_SETTINGS {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > MAX_SESSION_FILE_BYTES {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        sessions.push(parse_session_file(name, &contents));
    }
    sessions
}

// ---------------------------------------------------------------------------
// regedit export files
// ---------------------------------------------------------------------------

/// Consume a `"..."`-quoted token, unescaping `\\` and `\"`, and return the
/// remainder. Without the unescaping every `C:\\Users\\me\\key.ppk` in an
/// export would import with doubled separators.
fn split_quoted(input: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut characters = input.char_indices();
    while let Some((index, character)) = characters.next() {
        match character {
            '\\' => {
                let (_, escaped) = characters.next()?;
                out.push(escaped);
            }
            '"' => return Some((out, &input[index + 1..])),
            _ => out.push(character),
        }
    }
    None
}

fn parse_reg_value(line: &str) -> Option<(String, RegValue)> {
    let (name, rest) = split_quoted(line.strip_prefix('"')?)?;
    let rest = rest.strip_prefix('=')?;
    if let Some(quoted) = rest.strip_prefix('"') {
        let (value, _) = split_quoted(quoted)?;
        return Some((name, RegValue::Text(value)));
    }
    if let Some(hex) = rest.strip_prefix("dword:") {
        return u32::from_str_radix(hex.trim(), 16)
            .ok()
            .map(|number| (name, RegValue::Dword(number)));
    }
    // hex(2):, hex(7): and friends are not values we map; skip them silently.
    None
}

/// Pull the session name out of a section header, ignoring PuTTY's other keys
/// (`SshHostKeys`, `Jumplist`, ...) and `[-HKEY...]` deletion markers.
fn session_name_from_section(path: &str) -> Option<&str> {
    if path.starts_with('-') {
        return None;
    }
    const MARKER: &str = r"\software\simontatham\putty\sessions\";
    let lowercase = path.to_ascii_lowercase();
    let index = lowercase.find(MARKER)?;
    let name = &path[index + MARKER.len()..];
    if name.is_empty() || name.contains('\\') {
        return None;
    }
    Some(name)
}

pub(crate) fn reg_export_sessions(text: &str) -> Vec<PuttySession> {
    // Join `\`-continued lines before parsing.
    let mut lines: Vec<String> = Vec::new();
    let mut buffer = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(head) = line.strip_suffix('\\') {
            buffer.push_str(head.trim_end());
            continue;
        }
        buffer.push_str(line);
        lines.push(std::mem::take(&mut buffer));
    }
    if !buffer.is_empty() {
        lines.push(buffer);
    }

    let mut sessions = Vec::new();
    let mut current: Option<PuttySession> = None;
    for line in lines {
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(session) = current.take() {
                sessions.push(session);
            }
            let path = &line[1..line.len() - 1];
            current = session_name_from_section(path).and_then(|name| {
                let name = unmunge(name);
                (name != DEFAULT_SETTINGS).then(|| PuttySession::new(name))
            });
            continue;
        }
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some(session) = current.as_mut() {
            if let Some((name, value)) = parse_reg_value(&line) {
                session.set(&name, value);
            }
        }
    }
    if let Some(session) = current.take() {
        sessions.push(session);
    }
    sessions
}

// ---------------------------------------------------------------------------
// Candidate mapping
// ---------------------------------------------------------------------------

pub(crate) fn live_sessions(limit: usize) -> Vec<PuttySession> {
    let mut sessions = registry::sessions(limit);
    let mut seen: HashSet<String> = sessions
        .iter()
        .map(|session| session.name.to_ascii_lowercase())
        .collect();
    for session in file_sessions(limit) {
        if seen.insert(session.name.to_ascii_lowercase()) {
            sessions.push(session);
        }
    }
    sessions
}

/// PuTTY accepts `user@host` in the HostName field, in which case the embedded
/// user wins over a separate empty UserName.
fn split_user_host(host_name: String, user_name: Option<String>) -> (Option<String>, String) {
    match host_name.rsplit_once('@') {
        Some((user, host)) if !host.trim().is_empty() => {
            let user = trimmed(Some(user.to_string()));
            (user_name.or(user), host.trim().to_string())
        }
        _ => (user_name, host_name),
    }
}

pub(crate) fn to_candidates(sessions: Vec<PuttySession>) -> Vec<ParsedCandidate> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for session in sessions {
        if !session.is_ssh() {
            continue;
        }
        let Some(host_name) = trimmed(session.host_name) else {
            continue;
        };
        let name = trimmed(Some(session.name)).unwrap_or_else(|| host_name.clone());
        let (username, hostname) = split_user_host(host_name, trimmed(session.user_name));
        let identity_file = concrete_identity_path(session.public_key_file);
        let agent = session.try_agent.unwrap_or(false);
        let hint = auth_hint(agent.then_some("agent"), identity_file.is_some());
        push_candidate(
            &mut candidates,
            &mut seen,
            ParsedCandidate {
                name,
                hostname,
                // PuTTY writes 0 for "unset" in some hand-edited files.
                port: session.port.filter(|port| *port > 0).unwrap_or(22),
                username,
                // PuTTY has no notion of session groups.
                group: None,
                auth_hint: hint,
                identity_file,
            },
        );
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmunges_putty_session_names() {
        assert_eq!(unmunge("prod%20web"), "prod web");
        assert_eq!(unmunge("100%25"), "100%");
        assert_eq!(unmunge("%2Ehidden"), ".hidden");
        // Only a leading dot is escaped, so later ones arrive literally.
        assert_eq!(unmunge("web.example.com"), "web.example.com");
        // A stray percent that is not an escape stays put.
        assert_eq!(unmunge("50%off"), "50%off");
        assert_eq!(unmunge("trailing%"), "trailing%");
        assert_eq!(unmunge("%5Cshare"), r"\share");
    }

    #[test]
    fn parses_a_regedit_export() {
        let export = concat!(
            "Windows Registry Editor Version 5.00\r\n",
            "\r\n",
            "[HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\\Default%20Settings]\r\n",
            "\"HostName\"=\"\"\r\n",
            "\r\n",
            "[HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\\prod%20web]\r\n",
            "\"HostName\"=\"web.example.com\"\r\n",
            "\"PortNumber\"=dword:00000916\r\n",
            "\"UserName\"=\"deploy\"\r\n",
            "\"Protocol\"=\"ssh\"\r\n",
            "\"PublicKeyFile\"=\"C:\\\\Users\\\\alice\\\\keys\\\\prod.ppk\"\r\n",
            "\r\n",
            "[HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\\legacy]\r\n",
            "\"HostName\"=\"bob@old.example.com\"\r\n",
            "\"Protocol\"=\"telnet\"\r\n",
            "\r\n",
            "[HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\\inherited]\r\n",
            "\"HostName\"=\"carol@shell.example.com\"\r\n",
            "\"TryAgent\"=dword:00000001\r\n",
            "\r\n",
            "[HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\SshHostKeys]\r\n",
            "\"rsa2@22:web.example.com\"=\"0x23,0xabc\"\r\n",
        );

        let sessions = reg_export_sessions(export);
        assert_eq!(
            sessions.len(),
            3,
            "Default Settings and SshHostKeys are not sessions"
        );

        let prod = &sessions[0];
        assert_eq!(prod.name, "prod web");
        assert_eq!(prod.host_name.as_deref(), Some("web.example.com"));
        assert_eq!(prod.port, Some(2326));
        assert_eq!(prod.user_name.as_deref(), Some("deploy"));
        assert_eq!(
            prod.public_key_file.as_deref(),
            Some(r"C:\Users\alice\keys\prod.ppk"),
            "backslash escapes must be unescaped exactly once"
        );

        let candidates = to_candidates(sessions);
        assert_eq!(candidates.len(), 2, "the telnet session is skipped");
        assert_eq!(candidates[0].name, "prod web");
        assert_eq!(candidates[0].port, 2326);
        assert_eq!(candidates[0].auth_hint, "public-key");
        assert_eq!(
            candidates[0].identity_file.as_deref(),
            Some(r"C:\Users\alice\keys\prod.ppk")
        );
        // user@host in HostName is split when there is no separate UserName.
        assert_eq!(candidates[1].name, "inherited");
        assert_eq!(candidates[1].hostname, "shell.example.com");
        assert_eq!(candidates[1].username.as_deref(), Some("carol"));
        assert_eq!(candidates[1].auth_hint, "agent");
    }

    #[test]
    fn parses_a_regedit4_ansi_export_and_ignores_deletion_markers() {
        let export = concat!(
            "REGEDIT4\n",
            "\n",
            "[-HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\\removed]\n",
            "\n",
            "[HKEY_USERS\\S-1-5-21-1\\Software\\SimonTatham\\PuTTY\\Sessions\\db]\n",
            "; a comment\n",
            "\"HostName\"=\"db.internal\"\n",
            "\"PortNumber\"=dword:00000016\n",
        );
        let sessions = reg_export_sessions(export);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "db");
        assert_eq!(sessions[0].port, Some(22));
        // A protocol-less session is treated as SSH.
        assert!(sessions[0].is_ssh());
    }

    #[test]
    fn sessions_without_a_hostname_are_skipped() {
        let export = concat!(
            "[HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\\empty]\n",
            "\"PortNumber\"=dword:00000016\n",
        );
        assert!(to_candidates(reg_export_sessions(export)).is_empty());
    }

    #[test]
    fn parses_unix_session_files() {
        let directory =
            std::env::temp_dir().join(format!("luma-putty-test-{}", uuid::Uuid::new_v4()));
        let sessions_directory = directory.join("sessions");
        std::fs::create_dir_all(&sessions_directory).unwrap();
        std::fs::write(
            sessions_directory.join("prod%20web"),
            "Protocol=ssh\nHostName=web.example.com\nPortNumber=2222\nUserName=deploy\n",
        )
        .unwrap();
        std::fs::write(
            sessions_directory.join("Default%20Settings"),
            "Protocol=ssh\nHostName=\n",
        )
        .unwrap();

        // PUTTYDIR is how the Unix port relocates its own config directory.
        std::env::set_var("PUTTYDIR", &directory);
        let sessions = file_sessions(500);
        std::env::remove_var("PUTTYDIR");
        std::fs::remove_dir_all(&directory).ok();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "prod web");
        assert_eq!(sessions[0].host_name.as_deref(), Some("web.example.com"));
        assert_eq!(sessions[0].port, Some(2222));
        assert_eq!(sessions[0].user_name.as_deref(), Some("deploy"));
    }

    #[test]
    fn non_putty_text_yields_no_sessions() {
        assert!(reg_export_sessions("{\"hosts\": []}").is_empty());
        assert!(reg_export_sessions("").is_empty());
    }
}
