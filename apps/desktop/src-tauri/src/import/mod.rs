pub(crate) mod ppk;
mod putty;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::{Row, SqlitePool};

use zeroize::{Zeroize, Zeroizing};

use crate::errors::{LumaError, Result};
use crate::keystore::{self, KeystoreState};
use crate::platform::home_dir;
use crate::storage::host_groups;
use crate::storage::hosts::{self, Host, HostInput};
use crate::storage::key_references::{self, KeyReferenceInput};

const MAX_IMPORT_ENTRIES: usize = 500;
const MAX_IMPORT_FILE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportedHostCandidate {
    pub name: String,
    pub hostname: String,
    pub port: u16,
    pub username: Option<String>,
    pub group: Option<String>,
    pub auth_hint: String,
    pub already_exists: bool,
    /// The private key path the source referenced, verbatim. Doubles as the key
    /// a passphrase is supplied under, so two hosts sharing a key are only ever
    /// asked about once.
    pub key_file: Option<String>,
    /// What we found at `key_file`: one of `openssh`, `ppk`, `ppk-encrypted`,
    /// `missing`, or `unreadable`. The UI needs all five apart: link the path,
    /// convert silently, prompt for a passphrase, or warn.
    pub key_status: Option<String>,
    pub key_algorithm: Option<String>,
    pub key_comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportHostsRequest {
    #[serde(default = "crate::storage::vaults::default_id")]
    pub vault_id: String,
    pub selected_names: Vec<String>,
    /// Passphrases for encrypted `.ppk` files, keyed by the candidate's
    /// `keyFile`. Absent entries mean "import the host without its key".
    #[serde(default)]
    pub key_passphrases: HashMap<String, String>,
}

impl Drop for ImportHostsRequest {
    fn drop(&mut self) {
        for passphrase in self.key_passphrases.values_mut() {
            passphrase.zeroize();
        }
    }
}

/// A host that was imported without the key its session referenced.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlinkedKey {
    pub host: String,
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedHostsResult {
    pub imported_hosts: Vec<Host>,
    pub created_groups: Vec<String>,
    pub skipped_existing: Vec<String>,
    /// Names of the key references created from converted `.ppk` files.
    pub imported_keys: Vec<String>,
    pub unlinked_keys: Vec<UnlinkedKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportSource {
    Tabby,
    Electerm,
    /// A `regedit`-exported `.reg` file.
    Putty,
    /// PuTTY's saved sessions on this machine.
    PuttyLive,
}

impl ImportSource {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "tabby" => Ok(Self::Tabby),
            "electerm" => Ok(Self::Electerm),
            "putty" => Ok(Self::Putty),
            "putty-live" => Ok(Self::PuttyLive),
            _ => Err(LumaError::InvalidInput(
                "source must be one of 'tabby', 'electerm', 'putty', or 'putty-live'".into(),
            )),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Tabby => "Tabby",
            Self::Electerm => "Electerm",
            Self::Putty | Self::PuttyLive => "PuTTY",
        }
    }

    /// Only the live PuTTY source reads machine state instead of a file.
    fn needs_path(self) -> bool {
        self != Self::PuttyLive
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedCandidate {
    name: String,
    hostname: String,
    port: u16,
    username: Option<String>,
    group: Option<String>,
    auth_hint: String,
    identity_file: Option<String>,
}

impl ParsedCandidate {
    fn public(&self, already_exists: bool, key: Option<&KeyDescription>) -> ImportedHostCandidate {
        ImportedHostCandidate {
            name: self.name.clone(),
            hostname: self.hostname.clone(),
            port: self.port,
            username: self.username.clone(),
            group: self.group.clone(),
            auth_hint: self.auth_hint.clone(),
            already_exists,
            key_file: self.identity_file.clone(),
            key_status: key.map(|key| key.status.to_string()),
            key_algorithm: key.and_then(|key| key.algorithm.clone()),
            key_comment: key.and_then(|key| key.comment.clone()),
        }
    }
}

/// What sits at a candidate's referenced key path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyDescription {
    status: &'static str,
    algorithm: Option<String>,
    comment: Option<String>,
}

impl KeyDescription {
    fn plain(status: &'static str) -> Self {
        Self {
            status,
            algorithm: None,
            comment: None,
        }
    }
}

/// Inspect a referenced key without decrypting it. A key we cannot read is
/// described, never fatal: one broken `.ppk` must not blank out a preview of
/// forty perfectly good hosts.
fn describe_key_file(raw_path: &str) -> KeyDescription {
    let path = PathBuf::from(expanded_identity_file(raw_path));
    let Ok(metadata) = fs::metadata(&path) else {
        return KeyDescription::plain("missing");
    };
    if !metadata.is_file() {
        return KeyDescription::plain("missing");
    }
    if metadata.len() > ppk::MAX_PPK_FILE_BYTES as u64 {
        return KeyDescription::plain("unreadable");
    }
    let Ok(bytes) = fs::read(&path) else {
        return KeyDescription::plain("unreadable");
    };
    if !ppk::is_ppk(&bytes) {
        // An OpenSSH key needs no conversion; it is linked by path exactly as
        // Tabby and Electerm imports have always done.
        return KeyDescription::plain("openssh");
    }
    match ppk::inspect(&bytes) {
        Ok(info) => KeyDescription {
            status: if info.encrypted {
                "ppk-encrypted"
            } else {
                "ppk"
            },
            algorithm: Some(info.algorithm),
            comment: (!info.comment.is_empty()).then_some(info.comment),
        },
        Err(_) => KeyDescription::plain("unreadable"),
    }
}

/// Describe each distinct key path once, so hosts sharing a key cost one read.
fn describe_key_files(candidates: &[ParsedCandidate]) -> HashMap<String, KeyDescription> {
    let mut described = HashMap::new();
    for candidate in candidates {
        if let Some(identity_file) = &candidate.identity_file {
            if !described.contains_key(identity_file) {
                described.insert(identity_file.clone(), describe_key_file(identity_file));
            }
        }
    }
    described
}

#[derive(Debug, Default)]
struct LooseString(Option<String>);

impl<'de> Deserialize<'de> for LooseString {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LooseStringVisitor;

        impl<'de> Visitor<'de> for LooseStringVisitor {
            type Value = LooseString;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string-like value")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
                Ok(LooseString(Some(value.to_string())))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
                Ok(LooseString(Some(value)))
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
                Ok(LooseString(Some(value.to_string())))
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
                Ok(LooseString(Some(value.to_string())))
            }

            fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E> {
                let value =
                    (value.is_finite() && value.fract() == 0.0).then(|| format!("{value:.0}"));
                Ok(LooseString(value))
            }

            fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
                Ok(LooseString(None))
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(LooseString(None))
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(LooseString(None))
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LooseString(None))
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut result = None;
                while let Some(key) = map.next_key::<LooseString>()? {
                    let key = key.0.unwrap_or_default();
                    if matches!(
                        key.as_str(),
                        "type"
                            | "method"
                            | "authType"
                            | "id"
                            | "path"
                            | "file"
                            | "filename"
                            | "localPath"
                            | "privateKeyPath"
                            | "identityFile"
                    ) {
                        let value = map.next_value::<LooseString>()?.0;
                        if result.is_none() {
                            result = value;
                        }
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(LooseString(result))
            }
        }

        deserializer.deserialize_any(LooseStringVisitor)
    }
}

fn deserialize_loose_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    LooseString::deserialize(deserializer).map(|value| value.0)
}

fn deserialize_loose_strings<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct LooseStringsVisitor;

    impl<'de> Visitor<'de> for LooseStringsVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a string or list of string-like values")
        }

        fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
            Ok(vec![value.to_string()])
        }

        fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
            Ok(vec![value])
        }

        fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = sequence.next_element::<LooseString>()? {
                if let Some(value) = value.0 {
                    values.push(value);
                }
            }
            Ok(values)
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(key) = map.next_key::<LooseString>()? {
                let key = key.0.unwrap_or_default();
                if matches!(
                    key.as_str(),
                    "path" | "file" | "filename" | "localPath" | "privateKeyPath" | "identityFile"
                ) {
                    if let Some(value) = map.next_value::<LooseString>()?.0 {
                        values.push(value);
                    }
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(values)
        }

        fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
    }

    deserializer.deserialize_any(LooseStringsVisitor)
}

fn deserialize_port<'de, D>(deserializer: D) -> std::result::Result<Option<u16>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = LooseString::deserialize(deserializer)?.0;
    Ok(value
        .as_deref()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|port| *port > 0))
}

#[derive(Debug, Default, Deserialize)]
struct TabbyConfig {
    #[serde(default)]
    profiles: Vec<TabbyProfile>,
    #[serde(default)]
    groups: Vec<TabbyGroup>,
}

#[derive(Debug, Default, Deserialize)]
struct TabbyProfile {
    #[serde(
        rename = "type",
        default,
        deserialize_with = "deserialize_loose_string"
    )]
    profile_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    group: Option<String>,
    #[serde(
        rename = "groupId",
        default,
        deserialize_with = "deserialize_loose_string"
    )]
    group_id: Option<String>,
    #[serde(default)]
    options: TabbyOptions,
}

#[derive(Debug, Default, Deserialize)]
struct TabbyOptions {
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    host: Option<String>,
    #[serde(default, deserialize_with = "deserialize_port")]
    port: Option<u16>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    user: Option<String>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    auth: Option<String>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    group: Option<String>,
    #[serde(
        rename = "groupId",
        default,
        deserialize_with = "deserialize_loose_string"
    )]
    group_id: Option<String>,
    #[serde(
        rename = "identityFile",
        default,
        deserialize_with = "deserialize_loose_string"
    )]
    identity_file: Option<String>,
    #[serde(
        rename = "privateKeyPath",
        default,
        deserialize_with = "deserialize_loose_string"
    )]
    private_key_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TabbyGroup {
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ElectermObject {
    #[serde(default)]
    bookmarks: Vec<ElectermBookmark>,
    #[serde(default, rename = "bookmarkGroups", alias = "bookmark_groups")]
    bookmark_groups: Vec<ElectermGroup>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ElectermExport {
    Object(ElectermObject),
    Array(Vec<ElectermBookmark>),
}

#[derive(Debug, Default, Deserialize)]
struct ElectermBookmark {
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    id: Option<String>,
    #[serde(rename = "_id", default, deserialize_with = "deserialize_loose_string")]
    alternate_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    title: Option<String>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    host: Option<String>,
    #[serde(default, deserialize_with = "deserialize_port")]
    port: Option<u16>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    username: Option<String>,
    #[serde(
        rename = "authType",
        default,
        deserialize_with = "deserialize_loose_string"
    )]
    auth_type: Option<String>,
    #[serde(
        rename = "type",
        default,
        deserialize_with = "deserialize_loose_string"
    )]
    bookmark_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    category: Option<String>,
    #[serde(
        rename = "categoryId",
        default,
        deserialize_with = "deserialize_loose_string"
    )]
    category_id: Option<String>,
    #[serde(
        rename = "identityFile",
        default,
        deserialize_with = "deserialize_loose_string"
    )]
    identity_file: Option<String>,
    #[serde(
        rename = "privateKeyPath",
        default,
        deserialize_with = "deserialize_loose_string"
    )]
    private_key_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ElectermGroup {
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    id: Option<String>,
    #[serde(rename = "_id", default, deserialize_with = "deserialize_loose_string")]
    alternate_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    title: Option<String>,
    #[serde(default, deserialize_with = "deserialize_loose_string")]
    name: Option<String>,
    #[serde(
        rename = "bookmarkIds",
        alias = "bookmark_ids",
        default,
        deserialize_with = "deserialize_loose_strings"
    )]
    bookmark_ids: Vec<String>,
}

fn trimmed(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn first_trimmed(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    values.into_iter().find_map(trimmed)
}

fn auth_hint(value: Option<&str>, has_identity_file: bool) -> String {
    let normalized = value
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .replace(['_', ' '], "-");
    let hint = match normalized.as_str() {
        "password" | "pass" => "password",
        "keyboard-interactive" | "keyboardinteractive" | "interactive" => "keyboard-interactive",
        "public-key" | "publickey" | "private-key" | "privatekey" | "key" => "public-key",
        "agent" | "ssh-agent" | "sshagent" => "agent",
        _ if has_identity_file => "public-key",
        _ => "unknown",
    };
    hint.to_string()
}

fn concrete_identity_path(value: Option<String>) -> Option<String> {
    let value = trimmed(value)?;
    if value.len() > 4096 || value.contains(['\0', '\n', '\r']) {
        return None;
    }
    let uppercase = value.to_ascii_uppercase();
    if uppercase.contains("PRIVATE KEY") || value.starts_with("ssh-") {
        return None;
    }
    let bytes = value.as_bytes();
    let looks_like_path = value.starts_with(['~', '/', '\\', '.'])
        || value.contains(['/', '\\'])
        || bytes.get(1) == Some(&b':')
        || [".pem", ".ppk", ".key"]
            .iter()
            .any(|extension| value.to_ascii_lowercase().ends_with(extension));
    looks_like_path.then_some(value)
}

fn push_candidate(
    candidates: &mut Vec<ParsedCandidate>,
    seen: &mut HashSet<String>,
    candidate: ParsedCandidate,
) {
    if candidates.len() < MAX_IMPORT_ENTRIES && seen.insert(candidate.name.to_ascii_lowercase()) {
        candidates.push(candidate);
    }
}

fn parse_tabby(contents: &str) -> Result<Vec<ParsedCandidate>> {
    let config: TabbyConfig = serde_yml::from_str(contents).map_err(|_| {
        LumaError::InvalidInput("could not parse Tabby config: invalid YAML".into())
    })?;
    let group_names: HashMap<String, String> = config
        .groups
        .into_iter()
        .filter_map(|group| Some((trimmed(group.id)?, trimmed(group.name)?)))
        .collect();

    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for profile in config.profiles {
        if profile
            .profile_type
            .as_deref()
            .is_none_or(|value| !value.trim().eq_ignore_ascii_case("ssh"))
        {
            continue;
        }
        let Some(hostname) = trimmed(profile.options.host) else {
            continue;
        };
        let name = trimmed(profile.name).unwrap_or_else(|| hostname.clone());
        let group_id = first_trimmed([
            profile.group_id,
            profile.group,
            profile.options.group_id,
            profile.options.group,
        ]);
        let group = group_id.and_then(|id| group_names.get(&id).cloned());
        let identity_file = [
            profile.options.identity_file,
            profile.options.private_key_path,
        ]
        .into_iter()
        .find_map(concrete_identity_path);
        let hint = auth_hint(profile.options.auth.as_deref(), identity_file.is_some());
        push_candidate(
            &mut candidates,
            &mut seen,
            ParsedCandidate {
                name,
                hostname,
                port: profile.options.port.unwrap_or(22),
                username: trimmed(profile.options.user),
                group,
                auth_hint: hint,
                identity_file,
            },
        );
    }
    Ok(candidates)
}

fn electerm_is_ssh(bookmark_type: Option<&str>) -> bool {
    let normalized = bookmark_type
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if normalized == "ssh" {
        return true;
    }
    !matches!(
        normalized.as_str(),
        "serial" | "telnet" | "local" | "shell" | "terminal" | "sftp" | "rdp" | "vnc"
    )
}

fn parse_electerm(contents: &str) -> Result<Vec<ParsedCandidate>> {
    let export: ElectermExport = serde_json::from_str(contents).map_err(|error| {
        LumaError::InvalidInput(format!(
            "could not parse Electerm config: invalid JSON near line {}, column {}",
            error.line(),
            error.column()
        ))
    })?;
    let (bookmarks, groups) = match export {
        ElectermExport::Object(object) => (object.bookmarks, object.bookmark_groups),
        ElectermExport::Array(bookmarks) => (bookmarks, Vec::new()),
    };

    let mut group_names = HashMap::new();
    let mut bookmark_groups = HashMap::new();
    for group in groups {
        let group_id = first_trimmed([group.id, group.alternate_id]);
        let group_name = first_trimmed([group.title, group.name]);
        let (Some(group_id), Some(group_name)) = (group_id, group_name) else {
            continue;
        };
        group_names.insert(group_id.clone(), group_name.clone());
        for bookmark_id in group
            .bookmark_ids
            .into_iter()
            .filter_map(|id| trimmed(Some(id)))
        {
            bookmark_groups
                .entry(bookmark_id)
                .or_insert_with(|| group_name.clone());
        }
    }

    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for bookmark in bookmarks {
        if !electerm_is_ssh(bookmark.bookmark_type.as_deref()) {
            continue;
        }
        let Some(hostname) = trimmed(bookmark.host) else {
            continue;
        };
        let name =
            first_trimmed([bookmark.title, bookmark.name]).unwrap_or_else(|| hostname.clone());
        let bookmark_id = first_trimmed([bookmark.id, bookmark.alternate_id]);
        let category_id = first_trimmed([bookmark.category_id, bookmark.category]);
        let group = category_id
            .as_ref()
            .and_then(|id| group_names.get(id).cloned())
            .or_else(|| bookmark_id.and_then(|id| bookmark_groups.get(&id).cloned()));
        let identity_file = [bookmark.identity_file, bookmark.private_key_path]
            .into_iter()
            .find_map(concrete_identity_path);
        let auth_value = bookmark.auth_type.as_deref().or_else(|| {
            bookmark
                .bookmark_type
                .as_deref()
                .filter(|value| !value.trim().eq_ignore_ascii_case("ssh"))
        });
        let hint = auth_hint(auth_value, identity_file.is_some());
        push_candidate(
            &mut candidates,
            &mut seen,
            ParsedCandidate {
                name,
                hostname,
                port: bookmark.port.unwrap_or(22),
                username: trimmed(bookmark.username),
                group,
                auth_hint: hint,
                identity_file,
            },
        );
    }
    Ok(candidates)
}

pub(crate) fn validate_import_path(path: &str) -> Result<PathBuf> {
    if path.trim().is_empty() || path.contains('\0') || path.len() > 32_768 {
        return Err(LumaError::InvalidInput(
            "import file path is invalid".into(),
        ));
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(LumaError::InvalidInput(
            "import file path must be absolute".into(),
        ));
    }
    if !path.is_file() {
        return Err(LumaError::InvalidInput(
            "selected import file does not exist".into(),
        ));
    }
    Ok(path)
}

/// Decode an imported file's bytes as text.
///
/// `regedit` writes exports as UTF-16LE with a byte-order mark, so decoding
/// everything as UTF-8 would reject every `.reg` file ever exported. The other
/// sources are UTF-8 and pass through unchanged.
fn decode_text(bytes: Vec<u8>, label: &str) -> Result<String> {
    let invalid = || {
        LumaError::InvalidInput(format!(
            "could not parse {label} config: file is not valid text"
        ))
    };
    let decode_utf16 = |bytes: &[u8], little_endian: bool| -> Result<String> {
        if !bytes.len().is_multiple_of(2) {
            return Err(invalid());
        }
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| {
                if little_endian {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|_| invalid())
    };

    match bytes.as_slice() {
        // A UTF-32LE file also starts FF FE, so check the longer mark first.
        [0xFF, 0xFE, 0x00, 0x00, ..] => Err(invalid()),
        [0xFF, 0xFE, rest @ ..] => decode_utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => decode_utf16(rest, false),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8(rest.to_vec()).map_err(|_| invalid()),
        _ => String::from_utf8(bytes).map_err(|_| invalid()),
    }
}

fn read_candidates(source: ImportSource, path: Option<&str>) -> Result<Vec<ParsedCandidate>> {
    if !source.needs_path() {
        if path.is_some_and(|path| !path.is_empty()) {
            return Err(LumaError::InvalidInput(
                "the PuTTY auto-detect source does not take a file path".into(),
            ));
        }
        return Ok(putty::to_candidates(putty::live_sessions(
            MAX_IMPORT_ENTRIES,
        )));
    }

    let path = path.filter(|path| !path.is_empty()).ok_or_else(|| {
        LumaError::InvalidInput(format!("choose a {} file to import", source.label()))
    })?;
    let path = validate_import_path(path)?;
    if fs::metadata(&path)?.len() > MAX_IMPORT_FILE_BYTES {
        return Err(LumaError::InvalidInput(format!(
            "{} import file exceeds the size limit",
            source.label()
        )));
    }
    let contents = decode_text(fs::read(path)?, source.label())?;
    match source {
        ImportSource::Tabby => parse_tabby(&contents),
        ImportSource::Electerm => parse_electerm(&contents),
        ImportSource::Putty => Ok(putty::to_candidates(putty::reg_export_sessions(&contents))),
        ImportSource::PuttyLive => unreachable!("handled above"),
    }
}

pub async fn preview_hosts(
    pool: &SqlitePool,
    source: String,
    path: Option<String>,
    vault_id: &str,
) -> Result<Vec<ImportedHostCandidate>> {
    let source = ImportSource::parse(&source)?;
    let candidates = read_candidates(source, path.as_deref())?;
    let described = describe_key_files(&candidates);
    // A name only collides inside the vault being imported into.
    let rows = sqlx::query("SELECT name FROM hosts WHERE vault_id = ?1")
        .bind(vault_id)
        .fetch_all(pool)
        .await?;
    let existing: HashSet<String> = rows
        .iter()
        .map(|row| row.get::<String, _>("name").to_ascii_lowercase())
        .collect();
    Ok(candidates
        .iter()
        .map(|candidate| {
            candidate.public(
                existing.contains(&candidate.name.to_ascii_lowercase()),
                candidate
                    .identity_file
                    .as_ref()
                    .and_then(|path| described.get(path)),
            )
        })
        .collect())
}

fn expanded_identity_file(path: &str) -> String {
    let Some(home) = home_dir() else {
        return path.to_string();
    };
    let home_text = home.to_string_lossy();
    if path == "~" {
        return home_text.into_owned();
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return home.join(rest).to_string_lossy().into_owned();
    }
    path.replace("%d", &home_text)
}

fn authentication_type(links_key: bool) -> &'static str {
    if links_key {
        return "key";
    }
    "interactive"
}

/// How a candidate's referenced key will be stored.
enum KeyPlan {
    /// An OpenSSH key already on disk: referenced by path, never copied. This
    /// is what Tabby and Electerm imports have always done.
    LocalPath { local_path: String },
    /// A converted `.ppk`, as OpenSSH text bound for the encrypted keystore.
    Keystore(Box<KeystoreKeyPlan>),
    /// The key could not be used, so the host is imported without it.
    Unlinked { reason: String },
}

struct KeystoreKeyPlan {
    openssh: Zeroizing<String>,
    passphrase: Option<Zeroizing<String>>,
    public_key: String,
    fingerprint: String,
}

fn reason_for(error: &LumaError) -> String {
    match error {
        LumaError::InvalidInput(message) => message.clone(),
        other => other.to_string(),
    }
}

/// Cheap sniff of a file's first bytes, so the keystore-locked check can run
/// before any Argon2 work rather than after it.
fn file_is_ppk(raw_path: &str) -> bool {
    use std::io::Read as _;
    let Ok(mut file) = fs::File::open(expanded_identity_file(raw_path)) else {
        return false;
    };
    let mut head = [0u8; 32];
    let Ok(read) = file.read(&mut head) else {
        return false;
    };
    ppk::is_ppk(&head[..read])
}

/// Read and convert one referenced key.
///
/// Blocking on purpose: a v3 `.ppk` runs Argon2, which is expensive by design,
/// so callers hand this to `spawn_blocking`. A failure here is never fatal — it
/// downgrades that one host to an unlinked import so a single unreadable key
/// cannot abort a forty-host migration.
fn plan_key_blocking(raw_path: &str, passphrase: Option<String>) -> KeyPlan {
    let local_path = expanded_identity_file(raw_path);
    let metadata = match fs::metadata(&local_path) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => {
            return KeyPlan::Unlinked {
                reason: "the key file was not found".into(),
            }
        }
    };
    if metadata.len() > ppk::MAX_PPK_FILE_BYTES as u64 {
        return KeyPlan::Unlinked {
            reason: "the key file is too large to read".into(),
        };
    }
    let Ok(bytes) = fs::read(&local_path) else {
        return KeyPlan::Unlinked {
            reason: "the key file could not be read".into(),
        };
    };
    if !ppk::is_ppk(&bytes) {
        return KeyPlan::LocalPath { local_path };
    }

    let passphrase = passphrase
        .map(Zeroizing::new)
        .filter(|passphrase| !passphrase.is_empty());
    let converted = match ppk::convert(&bytes, passphrase.as_ref().map(|value| value.as_str())) {
        Ok(converted) => converted,
        Err(error) => {
            return KeyPlan::Unlinked {
                reason: reason_for(&error),
            }
        }
    };
    // Re-apply the passphrase the .ppk carried, so the converted key is no less
    // protected than the original and the temp file `ssh::identity_material`
    // materialises for russh stays encrypted at rest.
    match ppk::to_openssh(
        &converted.key,
        passphrase.as_ref().map(|value| value.as_str()),
    ) {
        Ok(openssh) => KeyPlan::Keystore(Box::new(KeystoreKeyPlan {
            openssh,
            passphrase,
            public_key: converted.public_key,
            fingerprint: converted.fingerprint,
        })),
        Err(error) => KeyPlan::Unlinked {
            reason: reason_for(&error),
        },
    }
}

struct PreparedHost {
    id: String,
    candidate: ParsedCandidate,
    input: HostInput,
    generated_key_id: Option<String>,
}

pub async fn apply_hosts(
    pool: &SqlitePool,
    keystore_state: &KeystoreState,
    source: String,
    path: Option<String>,
    request: ImportHostsRequest,
) -> Result<ImportedHostsResult> {
    let source = ImportSource::parse(&source)?;
    crate::storage::vaults::require(pool, &request.vault_id).await?;
    if request.selected_names.len() > MAX_IMPORT_ENTRIES {
        return Err(LumaError::InvalidInput(format!(
            "at most {MAX_IMPORT_ENTRIES} hosts can be imported at once"
        )));
    }
    let selected: HashSet<String> = request
        .selected_names
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    if selected.len() != request.selected_names.len() {
        return Err(LumaError::InvalidInput(
            "selectedNames contains duplicate entries".into(),
        ));
    }

    let parsed = read_candidates(source, path.as_deref())?;
    let available: HashSet<String> = parsed
        .iter()
        .map(|candidate| candidate.name.to_ascii_lowercase())
        .collect();
    if let Some(unknown) = selected.iter().find(|name| !available.contains(*name)) {
        return Err(LumaError::InvalidInput(format!(
            "import host entry was not found: {unknown}"
        )));
    }

    // Every distinct key path the selection references, in a stable order.
    let mut key_paths: Vec<String> = Vec::new();
    let mut seen_key_paths = HashSet::new();
    for candidate in &parsed {
        if !selected.contains(&candidate.name.to_ascii_lowercase()) {
            continue;
        }
        if let Some(identity_file) = &candidate.identity_file {
            if seen_key_paths.insert(identity_file.clone()) {
                key_paths.push(identity_file.clone());
            }
        }
    }

    // Bail before any conversion work: a locked keystore has nowhere to put a
    // converted key, and this error category drives the frontend's unlock gate.
    if !keystore::is_unlocked(keystore_state)
        && key_paths.iter().any(|raw_path| file_is_ppk(raw_path))
    {
        return Err(LumaError::KeystoreLocked(
            "unlock the keystore before importing PuTTY keys".into(),
        ));
    }

    let mut key_plans: HashMap<String, KeyPlan> = HashMap::new();
    for raw_path in key_paths {
        let passphrase = request.key_passphrases.get(&raw_path).cloned();
        let owned_path = raw_path.clone();
        let plan = tokio::task::spawn_blocking(move || plan_key_blocking(&owned_path, passphrase))
            .await
            .map_err(|_| {
                LumaError::InvalidInput("the key conversion task did not complete".into())
            })?;
        key_plans.insert(raw_path, plan);
    }

    let existing_host_rows = sqlx::query("SELECT name FROM hosts WHERE vault_id = ?1")
        .bind(&request.vault_id)
        .fetch_all(pool)
        .await?;
    let mut existing_names: HashSet<String> = existing_host_rows
        .iter()
        .map(|row| row.get::<String, _>("name").to_ascii_lowercase())
        .collect();
    let group_rows = sqlx::query("SELECT id, name FROM host_groups WHERE vault_id = ?1")
        .bind(&request.vault_id)
        .fetch_all(pool)
        .await?;
    let mut group_ids: HashMap<String, String> = group_rows
        .iter()
        .map(|row| {
            (
                row.get::<String, _>("name").to_ascii_lowercase(),
                row.get::<String, _>("id"),
            )
        })
        .collect();

    let mut skipped_existing = Vec::new();
    let mut new_groups = Vec::new();
    let mut prepared = Vec::new();
    let mut unlinked_keys = Vec::new();
    for candidate in parsed {
        let normalized_name = candidate.name.to_ascii_lowercase();
        if !selected.contains(&normalized_name) {
            continue;
        }
        if !existing_names.insert(normalized_name) {
            skipped_existing.push(candidate.name);
            continue;
        }

        let group_id = if let Some(group_name) = &candidate.group {
            let normalized_group = group_name.to_ascii_lowercase();
            if let Some(group_id) = group_ids.get(&normalized_group) {
                Some(group_id.clone())
            } else {
                host_groups::validate_name(group_name)?;
                let group_id = uuid::Uuid::new_v4().to_string();
                group_ids.insert(normalized_group, group_id.clone());
                new_groups.push((group_id.clone(), group_name.clone()));
                Some(group_id)
            }
        } else {
            None
        };

        let key_plan = candidate
            .identity_file
            .as_ref()
            .and_then(|identity_file| key_plans.get(identity_file));
        let links_key = matches!(
            key_plan,
            Some(KeyPlan::LocalPath { .. }) | Some(KeyPlan::Keystore(_))
        );
        if let (Some(KeyPlan::Unlinked { reason }), Some(identity_file)) =
            (key_plan, &candidate.identity_file)
        {
            unlinked_keys.push(UnlinkedKey {
                host: candidate.name.clone(),
                path: identity_file.clone(),
                reason: reason.clone(),
            });
        }

        let generated_key_id = links_key.then(|| uuid::Uuid::new_v4().to_string());
        if let Some(KeyPlan::LocalPath { local_path }) = key_plan {
            let mut key_name = format!("{} key", candidate.name);
            key_name.truncate(key_name.floor_char_boundary(128));
            key_references::validate(&KeyReferenceInput {
                vault_id: request.vault_id.clone(),
                name: key_name,
                public_key: None,
                storage_mode: "local-path".into(),
                local_path: Some(local_path.clone()),
                fingerprint: None,
                certificate: None,
                private_key: None,
                passphrase: None,
            })?;
        }
        let input = HostInput {
            vault_id: request.vault_id.clone(),
            name: candidate.name.clone(),
            hostname: candidate.hostname.clone(),
            port: i64::from(candidate.port),
            username: candidate.username.clone(),
            group_id,
            authentication_type: authentication_type(links_key).into(),
            key_id: generated_key_id.clone(),
            identity_id: None,
            proxy_jump_host_id: None,
            startup_command: None,
            working_directory: None,
            environment: None,
            tags: Vec::new(),
            favorite: false,
            tab_color: None,
            transport: "ssh".into(),
            mosh_server_path: None,
            mosh_port_range: None,
        };
        hosts::validate_fields(&input)?;
        prepared.push(PreparedHost {
            id: uuid::Uuid::new_v4().to_string(),
            candidate,
            input,
            generated_key_id,
        });
    }

    let mut transaction = pool.begin().await?;
    for (group_id, group_name) in &new_groups {
        sqlx::query(
            "INSERT INTO host_groups (id, vault_id, name, parent_id, sort_order) VALUES (?1, ?3, ?2, NULL, 0)",
        )
        .bind(group_id)
        .bind(group_name.trim())
        .bind(&request.vault_id)
        .execute(&mut *transaction)
        .await?;
    }

    // Hosts that share a key file share its key reference, so a passphrase is
    // only ever asked for, and a key only ever stored, once.
    let mut key_ids_by_path: HashMap<String, String> = HashMap::new();
    let mut imported_keys = Vec::new();
    for prepared_host in &prepared {
        let plan = prepared_host
            .candidate
            .identity_file
            .as_ref()
            .and_then(|identity_file| {
                key_plans
                    .get(identity_file)
                    .map(|plan| (identity_file, plan))
            });

        let key_id = match plan {
            Some((identity_file, KeyPlan::LocalPath { local_path })) => {
                if let Some(key_id) = key_ids_by_path.get(identity_file) {
                    Some(key_id.clone())
                } else if let Some(existing_key_id) = sqlx::query_scalar::<_, String>(
                    "SELECT id FROM key_references
                     WHERE storage_mode = 'local-path' AND local_path = ?1 AND vault_id = ?2 LIMIT 1",
                )
                .bind(local_path)
                .bind(&request.vault_id)
                .fetch_optional(&mut *transaction)
                .await?
                {
                    key_ids_by_path.insert(identity_file.clone(), existing_key_id.clone());
                    Some(existing_key_id)
                } else {
                    let key_id = prepared_host
                        .generated_key_id
                        .clone()
                        .expect("linked imports always prepare a key id");
                    let mut key_name = format!("{} key", prepared_host.candidate.name);
                    key_name.truncate(key_name.floor_char_boundary(128));
                    sqlx::query(
                        "INSERT INTO key_references (id, vault_id, name, storage_mode, local_path)
                         VALUES (?1, ?4, ?2, 'local-path', ?3)",
                    )
                    .bind(&key_id)
                    .bind(key_name)
                    .bind(local_path)
                    .bind(&request.vault_id)
                    .execute(&mut *transaction)
                    .await?;
                    key_ids_by_path.insert(identity_file.clone(), key_id.clone());
                    Some(key_id)
                }
            }
            Some((identity_file, KeyPlan::Keystore(plan))) => {
                if let Some(key_id) = key_ids_by_path.get(identity_file) {
                    Some(key_id.clone())
                } else if let Some(existing_key_id) = sqlx::query_scalar::<_, String>(
                    "SELECT id FROM key_references
                     WHERE storage_mode = 'encrypted-vault' AND fingerprint = ?1 AND vault_id = ?2
                     LIMIT 1",
                )
                .bind(&plan.fingerprint)
                .bind(&request.vault_id)
                .fetch_optional(&mut *transaction)
                .await?
                {
                    // The same key was already imported, perhaps from another
                    // client; reuse it rather than storing a second copy.
                    key_ids_by_path.insert(identity_file.clone(), existing_key_id.clone());
                    Some(existing_key_id)
                } else {
                    let mut key_name = format!("{} key", prepared_host.candidate.name);
                    key_name.truncate(key_name.floor_char_boundary(128));
                    // insert_metadata re-derives the public key and fingerprint
                    // from the converted private key through an independent
                    // parser, so a bad conversion cannot reach the database
                    // wearing correct-looking metadata.
                    let key_id = key_references::insert_metadata(
                        &mut *transaction,
                        KeyReferenceInput {
                            vault_id: request.vault_id.clone(),
                            name: key_name.clone(),
                            public_key: Some(plan.public_key.clone()),
                            storage_mode: "encrypted-vault".into(),
                            local_path: None,
                            fingerprint: Some(plan.fingerprint.clone()),
                            certificate: None,
                            private_key: Some(plan.openssh.to_string()),
                            passphrase: plan
                                .passphrase
                                .as_ref()
                                .map(|passphrase| passphrase.to_string()),
                        },
                        true,
                    )
                    .await?;
                    keystore::store(
                        &mut *transaction,
                        keystore_state,
                        "key",
                        &key_id,
                        "private-key",
                        &plan.openssh,
                    )
                    .await?;
                    if let Some(passphrase) = &plan.passphrase {
                        keystore::store(
                            &mut *transaction,
                            keystore_state,
                            "key",
                            &key_id,
                            "passphrase",
                            passphrase,
                        )
                        .await?;
                    }
                    key_ids_by_path.insert(identity_file.clone(), key_id.clone());
                    imported_keys.push(key_name);
                    Some(key_id)
                }
            }
            _ => None,
        };

        sqlx::query(
            "INSERT INTO hosts (
                 id, vault_id, name, hostname, port, username, group_id, auth_type, key_id,
                 proxy_jump_host_id, tags, favorite
             ) VALUES (?1, ?9, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, '[]', 0)",
        )
        .bind(&prepared_host.id)
        .bind(prepared_host.input.name.trim())
        .bind(prepared_host.input.hostname.trim())
        .bind(prepared_host.input.port)
        .bind(prepared_host.input.username.as_deref().map(str::trim))
        .bind(&prepared_host.input.group_id)
        .bind(&prepared_host.input.authentication_type)
        .bind(key_id)
        .bind(&request.vault_id)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;

    let mut imported_hosts = Vec::with_capacity(prepared.len());
    for prepared_host in prepared {
        if let Some(host) = hosts::get(pool, &prepared_host.id).await? {
            imported_hosts.push(host);
        }
    }
    Ok(ImportedHostsResult {
        imported_hosts,
        created_groups: new_groups.into_iter().map(|(_, name)| name).collect(),
        skipped_existing,
        imported_keys,
        unlinked_keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tabby_ssh_profiles_and_skips_other_entries() {
        let fixture = r#"
groups:
  - id: work-id
    name: " Work "
profiles:
  - type: ssh
    name: " Production "
    group: work-id
    options:
      host: " prod.example.com "
      port: "2222"
      user: " deploy "
      auth: password
  - type: ssh
    name: production
    options:
      host: duplicate.example.com
      auth: agent
  - type: serial
    name: Serial device
    options:
      host: serial.example.com
  - type: ssh
    name: Empty host
    options:
      host: "   "
  - type: ssh
    name: Key host
    options:
      host: key.example.com
      auth: publicKey
      identityFile: ~/.ssh/id_ed25519
"#;
        let candidates = parse_tabby(fixture).unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].name, "Production");
        assert_eq!(candidates[0].hostname, "prod.example.com");
        assert_eq!(candidates[0].port, 2222);
        assert_eq!(candidates[0].username.as_deref(), Some("deploy"));
        assert_eq!(candidates[0].group.as_deref(), Some("Work"));
        assert_eq!(candidates[0].auth_hint, "password");
        assert_eq!(candidates[1].auth_hint, "public-key");
        assert_eq!(
            candidates[1].identity_file.as_deref(),
            Some("~/.ssh/id_ed25519")
        );
    }

    #[test]
    fn parses_electerm_object_export_with_group_membership() {
        let fixture = r#"{
          "bookmarks": [
            {"id":"one","title":" Primary ","host":" one.example.com ","port":"2200","username":" alice ","type":"ssh","authType":"privateKey"},
            {"id":"two","name":"primary","host":"duplicate.example.com","type":"ssh","authType":"agent"},
            {"id":"three","title":"Telnet","host":"telnet.example.com","type":"telnet"},
            {"id":"four","title":"Empty","host":"  ","type":"ssh"},
            {"id":"five","title":"Keyboard","host":"kbd.example.com","type":"ssh","authType":"keyboard_interactive","category":"ops"}
          ],
          "bookmarkGroups": [
            {"id":"work","title":"Work","bookmarkIds":["one"]},
            {"id":"ops","name":"Operations","bookmarkIds":[]}
          ]
        }"#;
        let candidates = parse_electerm(fixture).unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].name, "Primary");
        assert_eq!(candidates[0].hostname, "one.example.com");
        assert_eq!(candidates[0].port, 2200);
        assert_eq!(candidates[0].username.as_deref(), Some("alice"));
        assert_eq!(candidates[0].group.as_deref(), Some("Work"));
        assert_eq!(candidates[0].auth_hint, "public-key");
        assert_eq!(candidates[1].group.as_deref(), Some("Operations"));
        assert_eq!(candidates[1].auth_hint, "keyboard-interactive");
    }

    #[test]
    fn parses_electerm_bare_array_and_ssh_shaped_bookmarks() {
        let fixture = r#"[
          {"title":"Agent","host":"agent.example.com","username":"root","authType":"agent"},
          {"name":"Password","host":"password.example.com","port":2022,"type":"password"},
          {"name":"Local","host":"localhost","type":"local"},
          {"name":"No host","type":"ssh"}
        ]"#;
        let candidates = parse_electerm(fixture).unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].port, 22);
        assert_eq!(candidates[0].auth_hint, "agent");
        assert_eq!(candidates[1].hostname, "password.example.com");
        assert_eq!(candidates[1].port, 2022);
        assert_eq!(candidates[1].auth_hint, "password");
    }

    #[test]
    fn maps_auth_hints_without_importing_secrets() {
        assert_eq!(auth_hint(Some("password"), false), "password");
        assert_eq!(
            auth_hint(Some("keyboard_interactive"), false),
            "keyboard-interactive"
        );
        assert_eq!(auth_hint(Some("publicKey"), false), "public-key");
        assert_eq!(auth_hint(Some("agent"), false), "agent");
        assert_eq!(auth_hint(Some("unsupported"), false), "unknown");

        // A candidate only claims key authentication once a key has actually
        // been linked; an auth hint on its own never promotes it.
        assert_eq!(authentication_type(false), "interactive");
        assert_eq!(authentication_type(true), "key");
    }

    // -----------------------------------------------------------------------
    // PuTTY
    // -----------------------------------------------------------------------

    const PPK_PASSPHRASE: &str = "luma test passphrase";
    const ENCRYPTED_PPK: &[u8] =
        include_bytes!("../../tests/fixtures/ppk/ed25519_v3_encrypted.ppk");
    const PLAIN_PPK: &[u8] = include_bytes!("../../tests/fixtures/ppk/ed25519_v2_plain.ppk");
    const OPENSSH_KEY: &str = include_str!("../../tests/fixtures/ppk/rsa2048_v3_plain.openssh");

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("luma-import-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, name: &str, contents: impl AsRef<[u8]>) -> String {
            let path = self.0.join(name);
            fs::write(&path, contents).unwrap();
            path.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    /// A `.reg` export naming the given key files, encoded the way `regedit`
    /// actually writes one: UTF-16LE with a byte-order mark.
    fn putty_reg_export(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut text = String::from("Windows Registry Editor Version 5.00\r\n\r\n");
        for (name, key_file) in entries {
            text.push_str(&format!(
                "[HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\\{name}]\r\n"
            ));
            text.push_str(&format!("\"HostName\"=\"{name}.example.com\"\r\n"));
            text.push_str("\"PortNumber\"=dword:00000016\r\n");
            text.push_str("\"UserName\"=\"deploy\"\r\n");
            text.push_str("\"Protocol\"=\"ssh\"\r\n");
            if !key_file.is_empty() {
                text.push_str(&format!(
                    "\"PublicKeyFile\"=\"{}\"\r\n",
                    key_file.replace('\\', "\\\\")
                ));
            }
            text.push_str("\r\n");
        }
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    async fn unlocked_keystore() -> (SqlitePool, KeystoreState) {
        let pool = crate::storage::init_in_memory().await.unwrap();
        let state = KeystoreState::default();
        crate::keystore::setup(&pool, &state, "test keystore password", false)
            .await
            .unwrap();
        (pool, state)
    }

    fn request(names: &[&str], passphrases: &[(&str, &str)]) -> ImportHostsRequest {
        ImportHostsRequest {
            vault_id: crate::storage::vaults::PERSONAL_VAULT_ID.to_string(),
            selected_names: names.iter().map(|name| name.to_string()).collect(),
            key_passphrases: passphrases
                .iter()
                .map(|(path, passphrase)| (path.to_string(), passphrase.to_string()))
                .collect(),
        }
    }

    #[test]
    fn decodes_utf16_and_utf8_import_files() {
        // regedit exports are UTF-16LE; decoding them as UTF-8 would reject
        // every .reg file ever produced.
        let export = putty_reg_export(&[("web", "")]);
        let text = decode_text(export, "PuTTY").unwrap();
        assert!(text.starts_with("Windows Registry Editor"));
        assert!(text.contains("web.example.com"));

        assert_eq!(decode_text(b"plain".to_vec(), "Tabby").unwrap(), "plain");
        assert_eq!(
            decode_text(vec![0xEF, 0xBB, 0xBF, b'h', b'i'], "Tabby").unwrap(),
            "hi"
        );
        assert!(decode_text(vec![0xFF, 0xFE, 0x00], "PuTTY").is_err());
    }

    #[tokio::test]
    async fn putty_previews_report_the_status_of_each_referenced_key() {
        let directory = TempDirectory::new();
        let encrypted = directory.write("encrypted.ppk", ENCRYPTED_PPK);
        let plain = directory.write("plain.ppk", PLAIN_PPK);
        let openssh = directory.write("id_rsa", OPENSSH_KEY);
        let missing = directory.0.join("gone.ppk").to_string_lossy().into_owned();
        let garbage = directory.write("broken.ppk", b"PuTTY-User-Key-File-3: nonsense");
        let export = directory.write(
            "putty.reg",
            putty_reg_export(&[
                ("locked", &encrypted),
                ("open", &plain),
                ("openssh", &openssh),
                ("absent", &missing),
                ("broken", &garbage),
            ]),
        );

        let pool = crate::storage::init_in_memory().await.unwrap();
        let candidates = preview_hosts(
            &pool,
            "putty".into(),
            Some(export),
            crate::storage::vaults::PERSONAL_VAULT_ID,
        )
        .await
        .unwrap();

        let status = |name: &str| {
            candidates
                .iter()
                .find(|candidate| candidate.name == name)
                .and_then(|candidate| candidate.key_status.clone())
                .unwrap()
        };
        assert_eq!(status("locked"), "ppk-encrypted");
        assert_eq!(status("open"), "ppk");
        assert_eq!(status("openssh"), "openssh");
        assert_eq!(status("absent"), "missing");
        // One unreadable key must not take the whole preview down with it.
        assert_eq!(status("broken"), "unreadable");

        let locked = candidates
            .iter()
            .find(|candidate| candidate.name == "locked")
            .unwrap();
        assert_eq!(locked.key_algorithm.as_deref(), Some("ssh-ed25519"));
        assert_eq!(locked.port, 22);
        assert_eq!(locked.username.as_deref(), Some("deploy"));
    }

    #[tokio::test]
    async fn apply_converts_a_ppk_into_the_keystore_and_links_it() {
        let directory = TempDirectory::new();
        let key_path = directory.write("prod.ppk", ENCRYPTED_PPK);
        let export = directory.write("putty.reg", putty_reg_export(&[("prod", &key_path)]));
        let (pool, keystore_state) = unlocked_keystore().await;

        let result = apply_hosts(
            &pool,
            &keystore_state,
            "putty".into(),
            Some(export),
            request(&["prod"], &[(key_path.as_str(), PPK_PASSPHRASE)]),
        )
        .await
        .unwrap();

        assert_eq!(result.imported_hosts.len(), 1);
        assert!(result.unlinked_keys.is_empty());
        assert_eq!(result.imported_keys, vec!["prod key".to_string()]);

        let host = &result.imported_hosts[0];
        assert_eq!(host.authentication_type, "key");
        let key_id = host.key_id.clone().expect("host links its converted key");

        let key = key_references::get(&pool, &key_id).await.unwrap().unwrap();
        assert_eq!(key.storage_mode, "encrypted-vault");
        assert!(key.has_private_key);
        assert!(
            key.local_path.is_none(),
            "no path to the original .ppk is kept"
        );
        assert!(key.fingerprint.is_some());

        // The stored secret has to be something russh can actually load, which
        // is the entire point of converting instead of storing the .ppk.
        let stored = crate::keystore::load(&pool, &keystore_state, "key", &key_id, "private-key")
            .await
            .unwrap()
            .expect("private key stored");
        assert!(
            !ppk::is_ppk(stored.as_bytes()),
            "a raw .ppk must never be stored"
        );
        let parsed = ssh_key::PrivateKey::from_openssh(&stored).unwrap();
        assert!(parsed.is_encrypted(), "the passphrase is carried over");
        let passphrase =
            crate::keystore::load(&pool, &keystore_state, "key", &key_id, "passphrase")
                .await
                .unwrap()
                .expect("passphrase stored");
        assert_eq!(passphrase, PPK_PASSPHRASE);
        let opened = parsed.decrypt(&passphrase).unwrap();
        let expected = ssh_key::PrivateKey::from_openssh(include_str!(
            "../../tests/fixtures/ppk/ed25519_v3_encrypted.openssh"
        ))
        .unwrap();
        assert_eq!(opened.key_data(), expected.key_data());
    }

    #[tokio::test]
    async fn apply_imports_the_host_unlinked_when_the_passphrase_is_missing() {
        let directory = TempDirectory::new();
        let key_path = directory.write("prod.ppk", ENCRYPTED_PPK);
        let export = directory.write("putty.reg", putty_reg_export(&[("prod", &key_path)]));
        let (pool, keystore_state) = unlocked_keystore().await;

        let result = apply_hosts(
            &pool,
            &keystore_state,
            "putty".into(),
            Some(export),
            request(&["prod"], &[]),
        )
        .await
        .unwrap();

        // The host is still worth having; only its key is missing.
        assert_eq!(result.imported_hosts.len(), 1);
        assert_eq!(result.imported_hosts[0].authentication_type, "interactive");
        assert!(result.imported_hosts[0].key_id.is_none());
        assert_eq!(result.unlinked_keys.len(), 1);
        assert_eq!(result.unlinked_keys[0].host, "prod");
        assert_eq!(
            result.unlinked_keys[0].reason,
            "this PuTTY key is encrypted and requires a passphrase"
        );
        assert!(key_references::list(&pool, None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn apply_fails_fast_when_the_keystore_is_locked() {
        let directory = TempDirectory::new();
        let key_path = directory.write("prod.ppk", PLAIN_PPK);
        let export = directory.write("putty.reg", putty_reg_export(&[("prod", &key_path)]));
        let pool = crate::storage::init_in_memory().await.unwrap();
        let keystore_state = KeystoreState::default();

        let error = apply_hosts(
            &pool,
            &keystore_state,
            "putty".into(),
            Some(export),
            request(&["prod"], &[]),
        )
        .await
        .unwrap_err();

        assert_eq!(error.category(), "keystore-locked");
        // Nothing may be half-written when we refuse.
        assert!(hosts::list(&pool, None).await.unwrap().is_empty());
        assert!(key_references::list(&pool, None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn two_sessions_sharing_a_key_import_it_once() {
        let directory = TempDirectory::new();
        let key_path = directory.write("shared.ppk", PLAIN_PPK);
        let export = directory.write(
            "putty.reg",
            putty_reg_export(&[("alpha", &key_path), ("beta", &key_path)]),
        );
        let (pool, keystore_state) = unlocked_keystore().await;

        let result = apply_hosts(
            &pool,
            &keystore_state,
            "putty".into(),
            Some(export),
            request(&["alpha", "beta"], &[]),
        )
        .await
        .unwrap();

        assert_eq!(result.imported_hosts.len(), 2);
        assert_eq!(result.imported_keys.len(), 1);
        let keys = key_references::list(&pool, None).await.unwrap();
        assert_eq!(keys.len(), 1, "one key reference for one key file");
        let key_id = keys[0].id.clone();
        for host in &result.imported_hosts {
            assert_eq!(host.key_id.as_deref(), Some(key_id.as_str()));
        }
    }

    #[tokio::test]
    async fn an_openssh_key_is_still_linked_by_path() {
        // The pre-existing behaviour for non-PuTTY keys must be untouched: they
        // are referenced where they sit, never copied into the keystore.
        let directory = TempDirectory::new();
        let key_path = directory.write("id_rsa", OPENSSH_KEY);
        let export = directory.write("putty.reg", putty_reg_export(&[("plain", &key_path)]));
        let (pool, keystore_state) = unlocked_keystore().await;

        let result = apply_hosts(
            &pool,
            &keystore_state,
            "putty".into(),
            Some(export),
            request(&["plain"], &[]),
        )
        .await
        .unwrap();

        assert!(result.imported_keys.is_empty());
        let keys = key_references::list(&pool, None).await.unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].storage_mode, "local-path");
        assert_eq!(keys[0].local_path.as_deref(), Some(key_path.as_str()));
        assert!(!keys[0].has_private_key);
    }

    #[tokio::test]
    async fn tabby_imports_carry_no_key_metadata() {
        let directory = TempDirectory::new();
        let config = directory.write(
            "tabby.yaml",
            "profiles:\n  - type: ssh\n    name: Web\n    options:\n      host: web.example.com\n",
        );
        let pool = crate::storage::init_in_memory().await.unwrap();
        let candidates = preview_hosts(
            &pool,
            "tabby".into(),
            Some(config),
            crate::storage::vaults::PERSONAL_VAULT_ID,
        )
        .await
        .unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].key_file.is_none());
        assert!(candidates[0].key_status.is_none());
    }

    #[tokio::test]
    async fn the_live_source_rejects_a_file_path() {
        let pool = crate::storage::init_in_memory().await.unwrap();
        let error = preview_hosts(
            &pool,
            "putty-live".into(),
            Some("C:\\somewhere\\putty.reg".into()),
            crate::storage::vaults::PERSONAL_VAULT_ID,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid input: the PuTTY auto-detect source does not take a file path"
        );

        // And a file source refuses to run without one.
        let error = preview_hosts(
            &pool,
            "putty".into(),
            None,
            crate::storage::vaults::PERSONAL_VAULT_ID,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid input: choose a PuTTY file to import"
        );
    }
}
