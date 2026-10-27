use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::fs::Metadata as RemoteMetadata;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType as RemoteFileType, OpenFlags, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use super::attach::sanitize_attachment_name;
use super::{remote_error, validate_identifier, validate_remote_path, SftpManager};
use crate::errors::{LumaError, Result};

/*
 * Remote file editing: a remote text file is downloaded to a private working
 * copy, opened in an external editor, and uploaded each time the editor saves.
 *
 * One actor task per open file owns all of its state. It polls the working copy
 * rather than holding a file watcher, so an editor that saves by writing a new
 * file and renaming it over the old one is seen like any other save.
 *
 * Before every upload the remote file is read back and its SHA-256 compared with
 * the version the working copy was based on. A mismatch pauses syncing as a
 * conflict until the user picks a side. The content is compared again after the
 * upload is staged, right before the swap, but SFTP has no compare-and-swap: a
 * write that lands between that last check and the rename can still be
 * overwritten.
 *
 * Uploads go to a temporary file beside the original and are renamed into
 * place, so an interrupted upload never truncates the remote file. SFTP v3
 * renames do not replace an existing file, so the swap moves the original to a
 * backup name first. The swap is two renames, not one atomic step; if the
 * second fails and the original cannot be put back, the error names where the
 * original now is.
 *
 * Each edit lives in `<app data>/remote-edits/<edit id>/`: an `edit.json`
 * manifest, and the working copy alone in `content/` so no remote file name can
 * collide with Luma's own files. The manifest is what lets unsynced edits
 * survive a crash or a quit and be offered again on the next launch.
 */

pub const MAX_EDIT_BYTES: u64 = 5 * 1024 * 1024;
pub const CHANGED_EVENT: &str = "remote-edit://changed";
pub const REMOVED_EVENT: &str = "remote-edit://removed";
const POLL_INTERVAL: Duration = Duration::from_millis(750);
/// Size and mtime can miss a same-length save inside one timestamp tick on
/// coarse filesystems, so the working copy is re-hashed every few polls anyway.
const FULL_CHECK_TICKS: u64 = 8;
/// An atomic save briefly removes the file; only a longer absence is reported.
const MISSING_TICKS: u32 = 8;
const MANIFEST_FILE: &str = "edit.json";
const CONTENT_DIR: &str = "content";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const BACKUP_DIR: &str = "backups";
const MAX_BACKUPS: usize = 50;
const MAX_STAGING_NAME_BYTES: usize = 200;

/// Text encodings Luma can round-trip. Bytes are never transcoded — the editor
/// receives exactly what the server had and Luma uploads exactly what the editor
/// saved — so this only gates what may be opened and how a diff is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextEncoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

impl TextEncoding {
    pub fn detect(bytes: &[u8]) -> std::result::Result<Self, &'static str> {
        if bytes.starts_with(&[0, 0, 0xFE, 0xFF]) || bytes.starts_with(&[0xFF, 0xFE, 0, 0]) {
            return Err("it is encoded as UTF-32, which is not supported");
        }
        const BINARY: &str = "it looks like a binary file";
        let utf16 = |rest: &[u8], read: fn([u8; 2]) -> u16, encoding: Self| match decode_utf16(
            rest, read,
        ) {
            Some(text) if text.contains('\0') => Err(BINARY),
            Some(_) => Ok(encoding),
            None => Err("it is not valid UTF-16"),
        };
        if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
            return utf16(rest, u16::from_le_bytes, Self::Utf16Le);
        }
        if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
            return utf16(rest, u16::from_be_bytes, Self::Utf16Be);
        }
        let (encoding, text) = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
            Some(rest) => (Self::Utf8Bom, rest),
            None => (Self::Utf8, bytes),
        };
        if text.contains(&0) {
            return Err(BINARY);
        }
        std::str::from_utf8(text)
            .map(|_| encoding)
            .map_err(|_| match encoding {
                Self::Utf8Bom => "it starts with a UTF-8 byte-order mark but is not valid UTF-8",
                _ => "it is not UTF-8 text, and other encodings are not supported",
            })
    }

    fn decode(self, bytes: &[u8]) -> Option<String> {
        match self {
            Self::Utf8 => std::str::from_utf8(bytes).ok().map(str::to_owned),
            Self::Utf8Bom => std::str::from_utf8(bytes.get(3..)?).ok().map(str::to_owned),
            Self::Utf16Le => decode_utf16(bytes.get(2..)?, u16::from_le_bytes),
            Self::Utf16Be => decode_utf16(bytes.get(2..)?, u16::from_be_bytes),
        }
    }
}

fn decode_utf16(bytes: &[u8], read: fn([u8; 2]) -> u16) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units = bytes.chunks_exact(2).map(|pair| read([pair[0], pair[1]]));
    char::decode_utf16(units)
        .collect::<std::result::Result<String, _>>()
        .ok()
}

fn decode_text(bytes: &[u8]) -> Option<String> {
    TextEncoding::detect(bytes).ok()?.decode(bytes)
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EditStatus {
    Synced,
    Uploading,
    /// Unsynced changes and no connected SFTP session for the host.
    Offline,
    Conflict,
    Error,
    /// Found on launch with changes that were never uploaded. Waits for the
    /// user rather than uploading a file they may have forgotten about.
    Recovered,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ConflictKind {
    Changed,
    Deleted,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Resolution {
    /// Overwrite the remote version that was reviewed with the working copy.
    KeepLocal,
    /// Replace the working copy with the remote version, after backing it up.
    UseRemote,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEditInfo {
    pub edit_id: String,
    pub host_id: String,
    pub remote_path: String,
    pub file_name: String,
    pub local_path: String,
    pub editor_id: Option<String>,
    pub encoding: TextEncoding,
    pub status: EditStatus,
    pub conflict: Option<ConflictKind>,
    pub message: Option<String>,
    /// The working copy differs from the version Luma last saw on the remote.
    pub unsynced: bool,
    pub connected: bool,
    /// Unix milliseconds.
    pub last_uploaded_at: Option<i64>,
    /// Where the working copy was saved before "use remote" replaced it.
    pub backup_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConflictReview {
    pub remote_text: Option<String>,
    pub local_text: Option<String>,
    pub remote_deleted: bool,
    /// Why one side cannot be compared as text.
    pub unavailable: Option<String>,
}

/// What the edit actors need from the rest of the app, behind a trait so the
/// sync logic can be exercised against an in-memory SFTP server.
pub trait EditHost: Send + Sync + 'static {
    /// A connected SFTP client for the host and the id of its session.
    fn client(&self, host_id: &str) -> Option<(String, Arc<SftpSession>)>;
    fn publish(&self, edit: &RemoteEditInfo);
    fn removed(&self, edit_id: &str);
}

pub struct AppEditHost(pub tauri::AppHandle);

impl EditHost for AppEditHost {
    fn client(&self, host_id: &str) -> Option<(String, Arc<SftpSession>)> {
        use tauri::Manager;
        self.0.state::<SftpManager>().client_for_host(host_id)
    }

    fn publish(&self, edit: &RemoteEditInfo) {
        use tauri::Emitter;
        let _ = self.0.emit(CHANGED_EVENT, edit);
    }

    fn removed(&self, edit_id: &str) {
        use tauri::Emitter;
        let _ = self.0.emit(REMOVED_EVENT, edit_id);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    host_id: String,
    remote_path: String,
    file_name: String,
    local_name: String,
    encoding: TextEncoding,
    /// SHA-256 of the remote content the working copy is based on; `None` when
    /// the user chose to recreate a file that was deleted remotely.
    base_hash: Option<String>,
    editor_id: Option<String>,
    last_uploaded_at: Option<i64>,
}

struct Shared {
    info: RemoteEditInfo,
    base_hash: Option<String>,
}

#[derive(Clone)]
struct EditHandle {
    shared: Arc<Mutex<Shared>>,
    requests: mpsc::Sender<Request>,
    dir: PathBuf,
    local_path: PathBuf,
}

enum Request {
    Review(oneshot::Sender<Result<ConflictReview>>),
    Resolve(Resolution, oneshot::Sender<Result<()>>),
    Upload(oneshot::Sender<Result<()>>),
    Close(bool, oneshot::Sender<Result<()>>),
    #[cfg(test)]
    Poll(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct RemoteEditManager {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    host: Arc<dyn EditHost>,
    poll_interval: Duration,
    edits: Mutex<HashMap<String, EditHandle>>,
    /// Serializes opens so two requests for one file cannot both download it.
    open_lock: tokio::sync::Mutex<()>,
    restored: tokio::sync::OnceCell<()>,
}

impl RemoteEditManager {
    pub fn new(root: PathBuf, host: Arc<dyn EditHost>) -> Self {
        Self::with_poll_interval(root, host, POLL_INTERVAL)
    }

    fn with_poll_interval(root: PathBuf, host: Arc<dyn EditHost>, poll_interval: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                root,
                host,
                poll_interval,
                edits: Mutex::new(HashMap::new()),
                open_lock: tokio::sync::Mutex::new(()),
                restored: tokio::sync::OnceCell::new(),
            }),
        }
    }

    pub async fn list(&self) -> Vec<RemoteEditInfo> {
        self.ensure_restored().await;
        let mut edits: Vec<_> = self
            .inner
            .edits
            .lock()
            .unwrap()
            .values()
            .map(|handle| handle.shared.lock().unwrap().info.clone())
            .collect();
        edits.sort_by(|left, right| {
            left.file_name
                .cmp(&right.file_name)
                .then_with(|| left.edit_id.cmp(&right.edit_id))
        });
        edits
    }

    /// Download `remote_path` into a new working copy and hand it to `launch`.
    /// A file that is already open for the same host is not downloaded again:
    /// its existing working copy is reopened instead.
    pub async fn open(
        &self,
        host_id: &str,
        client: &SftpSession,
        remote_path: &str,
        editor_id: &str,
        launch: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<RemoteEditInfo> {
        self.ensure_restored().await;
        validate_identifier(host_id, "hostId")?;
        let _opening = self.inner.open_lock.lock().await;
        let requested = validate_remote_path(remote_path)?;
        // Resolving symlinks up front means the rename on upload replaces the
        // real file instead of turning a link into a regular file.
        let canonical = client.canonicalize(requested).await.map_err(remote_error)?;
        validate_remote_path(&canonical)?;

        if let Some((edit_id, handle)) = self.find(host_id, &canonical) {
            launch(&handle.local_path)?;
            let info = {
                let mut shared = handle.shared.lock().unwrap();
                shared.info.editor_id = Some(editor_id.to_string());
                shared.info.clone()
            };
            self.inner.host.publish(&info);
            tracing::debug!(edit_id = %edit_id, "reopened remote edit");
            return Ok(info);
        }

        let file_name = remote_file_name(&canonical);
        let (bytes, _) = read_remote(client, &canonical).await?.ok_or_else(|| {
            LumaError::SftpFailed(format!("{file_name} no longer exists on the remote"))
        })?;
        let encoding = TextEncoding::detect(&bytes).map_err(|reason| {
            LumaError::InvalidInput(format!("{file_name} can't be edited because {reason}"))
        })?;

        let edit_id = uuid::Uuid::new_v4().to_string();
        let dir = self.inner.root.join(&edit_id);
        let local_name = local_file_name(&file_name);
        let local_path = working_copy_path(&dir, &local_name);
        let manifest = Manifest {
            host_id: host_id.to_string(),
            remote_path: canonical.clone(),
            file_name: file_name.clone(),
            local_name,
            encoding,
            base_hash: Some(digest(&bytes)),
            editor_id: Some(editor_id.to_string()),
            last_uploaded_at: None,
        };
        let prepared = (|| -> io::Result<()> {
            create_private_dir(&self.inner.root)?;
            create_private_dir(&dir.join(CONTENT_DIR))?;
            write_private_file(&local_path, &bytes)?;
            write_manifest(&dir, &manifest)
        })();
        if let Err(error) = prepared
            .map_err(LumaError::from)
            .and_then(|()| launch(&local_path))
        {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(error);
        }

        let local = LocalState::Read(digest(&bytes));
        let info = self.start(edit_id, dir, manifest, local, false);
        tracing::info!(edit_id = %info.edit_id, "opened remote file for editing");
        Ok(info)
    }

    pub async fn reopen(
        &self,
        edit_id: &str,
        editor_id: &str,
        launch: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<RemoteEditInfo> {
        let handle = self.handle(edit_id).await?;
        launch(&handle.local_path)?;
        let info = {
            let mut shared = handle.shared.lock().unwrap();
            shared.info.editor_id = Some(editor_id.to_string());
            shared.info.clone()
        };
        self.inner.host.publish(&info);
        Ok(info)
    }

    pub async fn review(&self, edit_id: &str) -> Result<ConflictReview> {
        self.request(edit_id, Request::Review).await
    }

    pub async fn resolve(&self, edit_id: &str, resolution: Resolution) -> Result<()> {
        self.request(edit_id, |reply| Request::Resolve(resolution, reply))
            .await
    }

    pub async fn upload(&self, edit_id: &str) -> Result<()> {
        self.request(edit_id, Request::Upload).await
    }

    /// Stop syncing and delete the working copy. Refuses while the working copy
    /// has changes the remote does not, unless `discard` says to drop them.
    pub async fn close(&self, edit_id: &str, discard: bool) -> Result<()> {
        self.request(edit_id, |reply| Request::Close(discard, reply))
            .await?;
        self.inner.edits.lock().unwrap().remove(edit_id);
        self.inner.host.removed(edit_id);
        tracing::info!(edit_id = %edit_id, "closed remote edit");
        Ok(())
    }

    /// The working copy, or the last backup "use remote" made of it.
    pub async fn local_path(&self, edit_id: &str, backup: bool) -> Result<PathBuf> {
        let handle = self.handle(edit_id).await?;
        if !backup {
            return Ok(handle.local_path);
        }
        let backup_path = handle.shared.lock().unwrap().info.backup_path.clone();
        backup_path
            .map(PathBuf::from)
            .ok_or_else(|| LumaError::InvalidInput("this edit has no backup".into()))
    }

    /// On quit: delete working copies that match the remote, keep the rest for
    /// the next launch to offer again. A copy that can't be read within the
    /// size limit is kept too.
    pub fn shutdown(&self) {
        for handle in self.inner.edits.lock().unwrap().values() {
            let base = handle.shared.lock().unwrap().base_hash.clone();
            let current = read_local_blocking(&handle.local_path)
                .ok()
                .map(|bytes| digest(&bytes));
            if current.is_some() && current == base {
                let _ = std::fs::remove_dir_all(&handle.dir);
            }
        }
    }

    async fn request<T>(
        &self,
        edit_id: &str,
        build: impl FnOnce(oneshot::Sender<Result<T>>) -> Request,
    ) -> Result<T> {
        let handle = self.handle(edit_id).await?;
        let (reply, response) = oneshot::channel();
        handle
            .requests
            .send(build(reply))
            .await
            .map_err(|_| LumaError::InvalidInput("unknown remote edit".into()))?;
        response
            .await
            .map_err(|_| LumaError::InvalidInput("unknown remote edit".into()))?
    }

    async fn handle(&self, edit_id: &str) -> Result<EditHandle> {
        self.ensure_restored().await;
        validate_identifier(edit_id, "editId")?;
        self.inner
            .edits
            .lock()
            .unwrap()
            .get(edit_id)
            .cloned()
            .ok_or_else(|| LumaError::InvalidInput("unknown remote edit".into()))
    }

    fn find(&self, host_id: &str, remote_path: &str) -> Option<(String, EditHandle)> {
        self.inner
            .edits
            .lock()
            .unwrap()
            .iter()
            .find(|(_, handle)| {
                let shared = handle.shared.lock().unwrap();
                shared.info.host_id == host_id && shared.info.remote_path == remote_path
            })
            .map(|(edit_id, handle)| (edit_id.clone(), handle.clone()))
    }

    fn start(
        &self,
        edit_id: String,
        dir: PathBuf,
        manifest: Manifest,
        local: LocalState,
        recovered: bool,
    ) -> RemoteEditInfo {
        let local_path = working_copy_path(&dir, &manifest.local_name);
        let shared = Arc::new(Mutex::new(Shared {
            info: RemoteEditInfo {
                edit_id: edit_id.clone(),
                host_id: manifest.host_id.clone(),
                remote_path: manifest.remote_path.clone(),
                file_name: manifest.file_name.clone(),
                local_path: local_path.to_string_lossy().into_owned(),
                editor_id: manifest.editor_id.clone(),
                encoding: manifest.encoding,
                status: EditStatus::Synced,
                conflict: None,
                message: None,
                unsynced: false,
                connected: false,
                last_uploaded_at: manifest.last_uploaded_at,
                backup_path: None,
            },
            base_hash: manifest.base_hash.clone(),
        }));
        let problem = (local == LocalState::TooLarge).then(too_large_message);
        let actor = Actor {
            host: Arc::clone(&self.inner.host),
            shared: Arc::clone(&shared),
            backups: self.inner.root.join(BACKUP_DIR),
            dir: dir.clone(),
            local_path: local_path.clone(),
            manifest,
            ticks: 0,
            seen: None,
            processed: None,
            local,
            missing_ticks: 0,
            last_attempt: None,
            paused: recovered.then_some(Pause::Recovered),
            conflict_remote: None,
            uploading: false,
            failure: None,
            problem,
            offline: false,
        };
        let info = actor.snapshot();
        shared.lock().unwrap().info = info.clone();
        let (requests, receiver) = mpsc::channel(8);
        self.inner.edits.lock().unwrap().insert(
            edit_id,
            EditHandle {
                shared,
                requests,
                dir,
                local_path,
            },
        );
        tokio::spawn(actor.run(receiver, self.inner.poll_interval));
        self.inner.host.publish(&info);
        info
    }

    async fn ensure_restored(&self) {
        self.inner.restored.get_or_init(|| self.restore()).await;
    }

    async fn restore(&self) {
        let Ok(mut entries) = tokio::fs::read_dir(&self.inner.root).await else {
            return;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if uuid::Uuid::parse_str(&name).is_err() {
                continue;
            }
            let dir = entry.path();
            match restore_manifest(&dir).await {
                Ok(Some((manifest, local))) => {
                    let recovered = !local.matches(manifest.base_hash.as_deref());
                    self.start(name, dir, manifest, local, recovered);
                }
                Ok(None) => {
                    let _ = tokio::fs::remove_dir_all(&dir).await;
                }
                Err(error) => {
                    tracing::warn!(%error, edit_id = %name, "could not restore remote edit; leaving its files in place");
                }
            }
        }
    }

    #[cfg(test)]
    async fn poll(&self, edit_id: &str) {
        let handle = self.handle(edit_id).await.unwrap();
        let (reply, done) = oneshot::channel();
        handle.requests.send(Request::Poll(reply)).await.unwrap();
        done.await.unwrap();
    }
}

/// `Ok(None)` means there is nothing left to recover.
async fn restore_manifest(dir: &Path) -> Result<Option<(Manifest, LocalState)>> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let metadata = match tokio::fs::metadata(&manifest_path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(LumaError::InvalidInput("edit manifest is too large".into()));
    }
    let manifest: Manifest = serde_json::from_slice(&tokio::fs::read(&manifest_path).await?)
        .map_err(|error| LumaError::InvalidInput(format!("edit manifest is invalid: {error}")))?;
    validate_identifier(&manifest.host_id, "hostId")?;
    validate_remote_path(&manifest.remote_path)?;
    if manifest.local_name != local_file_name(&manifest.local_name) {
        return Err(LumaError::InvalidInput(
            "edit manifest names an invalid working copy".into(),
        ));
    }
    // A folder without the content directory is not one this version wrote;
    // leave it alone rather than delete files it can't account for.
    if !tokio::fs::metadata(dir.join(CONTENT_DIR))
        .await
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(LumaError::InvalidInput(
            "edit folder has no working copy directory".into(),
        ));
    }
    let local = match read_local(&working_copy_path(dir, &manifest.local_name)).await {
        Ok(bytes) => LocalState::Read(digest(&bytes)),
        Err(LocalRead::TooLarge) => LocalState::TooLarge,
        Err(LocalRead::Io(error)) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(LocalRead::Io(error)) => return Err(error.into()),
    };
    Ok(Some((manifest, local)))
}

fn working_copy_path(dir: &Path, local_name: &str) -> PathBuf {
    dir.join(CONTENT_DIR).join(local_name)
}

/// What Luma last learned about the working copy.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalState {
    /// SHA-256 of its content.
    Read(String),
    /// Over the editing limit, so never hashed or uploaded.
    TooLarge,
    /// Missing or unreadable. Treated as unsynced: nothing proves the remote
    /// has what the user last saved.
    Unknown,
}

impl LocalState {
    fn matches(&self, base: Option<&str>) -> bool {
        matches!(self, Self::Read(hash) if base == Some(hash.as_str()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    len: u64,
    modified: Option<SystemTime>,
}

impl Fingerprint {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pause {
    Conflict(ConflictKind),
    Recovered,
}

enum LocalRead {
    TooLarge,
    Io(io::Error),
}

struct Actor {
    host: Arc<dyn EditHost>,
    shared: Arc<Mutex<Shared>>,
    dir: PathBuf,
    local_path: PathBuf,
    backups: PathBuf,
    manifest: Manifest,
    ticks: u64,
    /// The working copy's last observed size and mtime; a save is acted on
    /// only once two polls agree, so a half-written file is never uploaded.
    seen: Option<Fingerprint>,
    processed: Option<Fingerprint>,
    local: LocalState,
    missing_ticks: u32,
    /// What the last failed upload was tried with. A failure is retried only
    /// once the user saves again or a different session for the host appears.
    last_attempt: Option<(Option<Fingerprint>, String)>,
    paused: Option<Pause>,
    /// The remote content found when the conflict was detected.
    conflict_remote: Option<Vec<u8>>,
    uploading: bool,
    failure: Option<String>,
    problem: Option<String>,
    offline: bool,
}

impl Actor {
    async fn run(mut self, mut requests: mpsc::Receiver<Request>, poll_interval: Duration) {
        let mut interval =
            tokio::time::interval_at(tokio::time::Instant::now() + poll_interval, poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = interval.tick() => self.poll().await,
                request = requests.recv() => match request {
                    Some(Request::Close(discard, reply)) => {
                        let result = self.close(discard).await;
                        let closed = result.is_ok();
                        let _ = reply.send(result);
                        if closed {
                            break;
                        }
                    }
                    Some(Request::Review(reply)) => {
                        let _ = reply.send(self.review().await);
                    }
                    Some(Request::Resolve(resolution, reply)) => {
                        let _ = reply.send(self.resolve(resolution).await);
                    }
                    Some(Request::Upload(reply)) => {
                        let _ = reply.send(self.upload_now().await);
                    }
                    #[cfg(test)]
                    Some(Request::Poll(reply)) => {
                        self.poll().await;
                        let _ = reply.send(());
                    }
                    None => break,
                },
            }
        }
    }

    async fn poll(&mut self) {
        self.ticks += 1;
        let fingerprint = match tokio::fs::metadata(&self.local_path).await {
            Ok(metadata) if metadata.is_file() => Fingerprint::of(&metadata),
            Err(error) if error.kind() != io::ErrorKind::NotFound => return,
            _ => {
                self.missing_ticks += 1;
                if self.missing_ticks >= MISSING_TICKS {
                    self.problem = Some(
                        "The working copy is missing. Save the file from your editor again to resume syncing.".into(),
                    );
                    self.local = LocalState::Unknown;
                    self.processed = None;
                }
                self.publish();
                return;
            }
        };
        self.missing_ticks = 0;
        if self.seen != Some(fingerprint) {
            self.seen = Some(fingerprint);
            return;
        }
        if self.processed != Some(fingerprint) || self.ticks.is_multiple_of(FULL_CHECK_TICKS) {
            match read_local(&self.local_path).await {
                Ok(bytes) => {
                    self.accept_local(&bytes);
                }
                Err(LocalRead::TooLarge) => self.too_large(),
                // Typically an editor still holding the file mid-save.
                Err(LocalRead::Io(_)) => return,
            }
            self.processed = Some(fingerprint);
        }
        self.sync(false).await;
    }

    /// Record freshly read working-copy content. Returns its encoding, or
    /// `None` when it is no longer text Luma can round-trip — which stops it
    /// being uploaded until the next save.
    fn accept_local(&mut self, bytes: &[u8]) -> Option<TextEncoding> {
        self.local = LocalState::Read(digest(bytes));
        match TextEncoding::detect(bytes) {
            Ok(encoding) => {
                self.problem = None;
                Some(encoding)
            }
            Err(reason) => {
                self.problem = Some(format!("The saved file wasn't uploaded because {reason}."));
                None
            }
        }
    }

    fn too_large(&mut self) {
        self.local = LocalState::TooLarge;
        self.problem = Some(too_large_message());
    }

    /// Re-read the working copy now, outside the polling cadence.
    async fn refresh_local(&mut self) {
        match read_local(&self.local_path).await {
            Ok(bytes) => {
                self.accept_local(&bytes);
            }
            Err(LocalRead::TooLarge) => self.too_large(),
            Err(LocalRead::Io(_)) => self.local = LocalState::Unknown,
        }
    }

    async fn sync(&mut self, force: bool) {
        let unsynced = self.unsynced();
        if !unsynced || self.problem.is_some() || (self.paused.is_some() && !force) {
            if !unsynced {
                self.failure = None;
                self.offline = false;
                if self.paused == Some(Pause::Recovered) {
                    self.paused = None;
                }
            }
            self.publish();
            return;
        }
        let Some((session_id, client)) = self.host.client(&self.manifest.host_id) else {
            self.offline = true;
            self.publish();
            return;
        };
        self.offline = false;
        let attempt = (self.seen, session_id);
        if !force && self.failure.is_some() && self.last_attempt.as_ref() == Some(&attempt) {
            self.publish();
            return;
        }
        self.last_attempt = Some(attempt);
        self.upload(&client).await;
    }

    async fn upload(&mut self, client: &SftpSession) {
        let bytes = match read_local(&self.local_path).await {
            Ok(bytes) => bytes,
            Err(LocalRead::TooLarge) => {
                self.too_large();
                self.publish();
                return;
            }
            Err(LocalRead::Io(error)) => {
                self.local = LocalState::Unknown;
                self.failure = Some(format!("Couldn't read the working copy: {error}"));
                self.publish();
                return;
            }
        };
        let Some(encoding) = self.accept_local(&bytes) else {
            self.publish();
            return;
        };
        let hash = digest(&bytes);
        self.uploading = true;
        self.failure = None;
        self.publish();

        let base = self.manifest.base_hash.clone();
        let outcome = push(client, &self.manifest.remote_path, &bytes, base.as_deref()).await;
        self.uploading = false;
        match outcome {
            Ok(Pushed::Written) | Ok(Pushed::AlreadyCurrent) => {
                self.manifest.last_uploaded_at = Some(chrono::Utc::now().timestamp_millis());
                self.manifest.encoding = encoding;
                self.set_base(Some(hash));
                self.paused = None;
                self.conflict_remote = None;
            }
            Ok(Pushed::Conflict(remote)) => {
                let kind = if remote.is_some() {
                    ConflictKind::Changed
                } else {
                    ConflictKind::Deleted
                };
                self.paused = Some(Pause::Conflict(kind));
                self.conflict_remote = remote;
            }
            Err(error) => self.failure = Some(sentence(&error)),
        }
        self.publish();
    }

    async fn upload_now(&mut self) -> Result<()> {
        if matches!(self.paused, Some(Pause::Conflict(_))) {
            return Err(LumaError::InvalidInput(
                "resolve the conflict before uploading".into(),
            ));
        }
        self.refresh_local().await;
        self.paused = None;
        self.failure = None;
        self.last_attempt = None;
        self.sync(true).await;
        Ok(())
    }

    async fn review(&mut self) -> Result<ConflictReview> {
        let Some(Pause::Conflict(kind)) = self.paused else {
            return Err(LumaError::InvalidInput(
                "this file has no conflict to review".into(),
            ));
        };
        let local = read_local(&self.local_path).await.ok();
        let local_text = local.as_deref().and_then(decode_text);
        let remote_text = self.conflict_remote.as_deref().and_then(decode_text);
        let unavailable = if local_text.is_none() {
            Some("The working copy can't be read as text.".to_string())
        } else if kind == ConflictKind::Changed && remote_text.is_none() {
            Some("The remote version is no longer text Luma can compare.".to_string())
        } else {
            None
        };
        Ok(ConflictReview {
            remote_text,
            local_text,
            remote_deleted: kind == ConflictKind::Deleted,
            unavailable,
        })
    }

    async fn resolve(&mut self, resolution: Resolution) -> Result<()> {
        let Some(Pause::Conflict(kind)) = self.paused else {
            return Err(LumaError::InvalidInput(
                "this file has no conflict to resolve".into(),
            ));
        };
        match resolution {
            Resolution::KeepLocal => {
                // The reviewed remote version becomes the base, so the upload's
                // own check still stops if the remote changed again since.
                let base = self.conflict_remote.as_deref().map(digest);
                self.set_base(base);
                self.paused = None;
                self.conflict_remote = None;
                self.failure = None;
                self.last_attempt = None;
                self.refresh_local().await;
                self.sync(true).await;
                Ok(())
            }
            Resolution::UseRemote => {
                let remote = match (&self.conflict_remote, kind) {
                    (Some(remote), ConflictKind::Changed) => remote.clone(),
                    _ => {
                        return Err(LumaError::InvalidInput(
                            "the remote file was deleted, so there is no remote version to use"
                                .into(),
                        ))
                    }
                };
                let encoding = TextEncoding::detect(&remote).map_err(|reason| {
                    LumaError::InvalidInput(format!(
                        "the remote version can't be edited because {reason}"
                    ))
                })?;
                let backup = self.back_up_local()?;
                replace_local_file(&self.dir, &self.local_path, &remote)?;
                self.manifest.encoding = encoding;
                self.set_base(Some(digest(&remote)));
                self.local = LocalState::Read(digest(&remote));
                self.seen = None;
                self.processed = None;
                self.paused = None;
                self.conflict_remote = None;
                self.failure = None;
                self.problem = None;
                self.shared.lock().unwrap().info.backup_path = backup;
                self.publish();
                Ok(())
            }
        }
    }

    async fn close(&mut self, discard: bool) -> Result<()> {
        self.refresh_local().await;
        if self.unsynced() && !discard {
            self.publish();
            let reason = match self.local {
                LocalState::Read(_) => "this file has changes that haven't been uploaded",
                LocalState::TooLarge => "this file has saved changes that are too large to upload",
                LocalState::Unknown => {
                    "the working copy couldn't be read, so Luma can't tell whether it has changes that haven't been uploaded"
                }
            };
            return Err(LumaError::InvalidInput(reason.into()));
        }
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(LumaError::InvalidInput(format!(
                "couldn't delete the working copy: {error}"
            ))),
        }
    }

    fn unsynced(&self) -> bool {
        !self.local.matches(self.manifest.base_hash.as_deref())
    }

    fn set_base(&mut self, base: Option<String>) {
        self.manifest.base_hash = base.clone();
        self.shared.lock().unwrap().base_hash = base;
        if let Err(error) = write_manifest(&self.dir, &self.manifest) {
            tracing::warn!(%error, "could not update remote edit manifest");
        }
    }

    /// Copied rather than read, so a working copy over the editing limit is
    /// backed up whole.
    fn back_up_local(&self) -> Result<Option<String>> {
        create_private_dir(&self.backups)?;
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let path = self.backups.join(format!(
            "{stamp}-{}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..6],
            self.manifest.local_name
        ));
        match std::fs::copy(&self.local_path, &path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                return Err(error.into());
            }
        }
        prune_backups(&self.backups);
        Ok(Some(path.to_string_lossy().into_owned()))
    }

    /// Announce the public snapshot if anything changed.
    fn publish(&mut self) {
        let info = self.snapshot();
        let mut shared = self.shared.lock().unwrap();
        if info != shared.info {
            shared.info = info.clone();
            drop(shared);
            self.host.publish(&info);
        }
    }

    fn snapshot(&self) -> RemoteEditInfo {
        let unsynced = self.unsynced();
        let connected = self.host.client(&self.manifest.host_id).is_some();
        let (status, conflict, message) = if self.uploading {
            (EditStatus::Uploading, None, None)
        } else if let Some(Pause::Conflict(kind)) = self.paused {
            let message = match kind {
                ConflictKind::Changed => "The remote file changed since you opened it. Uploads are paused until you choose which version to keep.",
                ConflictKind::Deleted => "The remote file was deleted or moved since you opened it. Uploads are paused until you decide what to do.",
            };
            (EditStatus::Conflict, Some(kind), Some(message.to_string()))
        } else if let Some(problem) = &self.problem {
            (EditStatus::Error, None, Some(problem.clone()))
        } else if self.paused == Some(Pause::Recovered) && unsynced {
            (
                EditStatus::Recovered,
                None,
                Some("Luma closed before these changes were uploaded. Review them in your editor, then upload or discard them.".into()),
            )
        } else if let Some(failure) = &self.failure {
            (
                EditStatus::Error,
                None,
                Some(format!(
                    "{failure}. Your changes are kept locally; save again or retry to upload."
                )),
            )
        } else if unsynced && self.offline {
            (
                EditStatus::Offline,
                None,
                Some("Not connected to this host. Changes are kept locally and upload when you reconnect.".into()),
            )
        } else {
            (EditStatus::Synced, None, None)
        };

        let mut info = self.shared.lock().unwrap().info.clone();
        info.status = status;
        info.conflict = conflict;
        info.message = message;
        info.unsynced = unsynced;
        info.connected = connected;
        info.encoding = self.manifest.encoding;
        info.last_uploaded_at = self.manifest.last_uploaded_at;
        info
    }
}

#[derive(Debug)]
enum Pushed {
    Written,
    /// The remote already holds exactly these bytes.
    AlreadyCurrent,
    /// The remote no longer matches the base; carries its current content, or
    /// `None` when it is gone.
    Conflict(Option<Vec<u8>>),
}

async fn push(
    client: &SftpSession,
    path: &str,
    bytes: &[u8],
    base: Option<&str>,
) -> Result<Pushed> {
    let current = read_remote(client, path).await?;
    // Checked first: a remote that already holds these bytes has nothing to
    // lose, whoever wrote them.
    if current
        .as_ref()
        .is_some_and(|(remote, _)| remote.as_slice() == bytes)
    {
        return Ok(Pushed::AlreadyCurrent);
    }
    if !matches_base(&current, base) {
        return Ok(Pushed::Conflict(current.map(|(remote, _)| remote)));
    }

    let staging = staging_path(path, "edit")?;
    let original = current.map(|(_, metadata)| metadata);
    stage(client, &staging, bytes, original.as_ref()).await?;

    // Metadata can't prove the content held still during the upload, so the
    // content is compared again now that only the swap is left.
    let fresh = match read_remote(client, path).await {
        Ok(fresh) => fresh,
        Err(error) => {
            let _ = client.remove_file(staging).await;
            return Err(error);
        }
    };
    if fresh
        .as_ref()
        .is_some_and(|(remote, _)| remote.as_slice() == bytes)
    {
        let _ = client.remove_file(staging).await;
        return Ok(Pushed::AlreadyCurrent);
    }
    if !matches_base(&fresh, base) {
        let _ = client.remove_file(staging).await;
        return Ok(Pushed::Conflict(fresh.map(|(remote, _)| remote)));
    }
    if let (Some((_, now)), Some(before)) = (&fresh, &original) {
        // The staged copy carries the mode and owner read before the upload;
        // swapping it in now would quietly undo the change.
        if !same_access(now, before) {
            let _ = client.remove_file(staging).await;
            return Err(LumaError::SftpFailed(
                "the remote file's permissions or owner changed during the upload, so it was not replaced".into(),
            ));
        }
    }

    swap_into_place(client, &staging, path, fresh.is_some()).await?;
    Ok(Pushed::Written)
}

fn matches_base(current: &Option<(Vec<u8>, RemoteMetadata)>, base: Option<&str>) -> bool {
    match (current, base) {
        (None, None) => true,
        (Some((remote, _)), Some(base)) => digest(remote) == base,
        _ => false,
    }
}

fn same_access(left: &RemoteMetadata, right: &RemoteMetadata) -> bool {
    left.permissions == right.permissions && left.uid == right.uid && left.gid == right.gid
}

/// Move the staged upload over `path`. SFTP v3 won't rename onto an existing
/// file, so an existing original is first renamed aside: two steps, never
/// atomic. Every failure says where the original is; the staged copy is kept
/// only when the original could not be put back.
async fn swap_into_place(
    client: &SftpSession,
    staging: &str,
    path: &str,
    exists: bool,
) -> Result<()> {
    if !exists {
        return match client.rename(staging.to_string(), path.to_string()).await {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = client.remove_file(staging.to_string()).await;
                Err(LumaError::SftpFailed(format!(
                    "couldn't move the upload into place: {error}"
                )))
            }
        };
    }
    let backup = staging_path(path, "backup")?;
    if let Err(error) = client.rename(path.to_string(), backup.clone()).await {
        let _ = client.remove_file(staging.to_string()).await;
        return Err(LumaError::SftpFailed(format!(
            "couldn't move the original aside to replace it ({error}). The original file is unchanged"
        )));
    }
    let Err(error) = client.rename(staging.to_string(), path.to_string()).await else {
        if let Err(error) = client.remove_file(backup).await {
            tracing::warn!(%error, "uploaded an edit but could not remove the original's backup");
        }
        return Ok(());
    };
    match client.rename(backup.clone(), path.to_string()).await {
        Ok(()) => {
            let _ = client.remove_file(staging.to_string()).await;
            Err(LumaError::SftpFailed(format!(
                "couldn't move the upload into place ({error}). The original file is unchanged"
            )))
        }
        Err(rollback) => Err(LumaError::SftpFailed(format!(
            "couldn't move the upload into place ({error}) or put the original back ({rollback}). The original file is now at {backup}, and the upload at {staging}"
        ))),
    }
}

/// Write `bytes` to a new file at `staging`, then give it the original's mode
/// and owner. Removes the staged file on any failure.
async fn stage(
    client: &SftpSession,
    staging: &str,
    bytes: &[u8],
    original: Option<&RemoteMetadata>,
) -> Result<()> {
    let mut attributes = RemoteMetadata::empty();
    attributes.permissions = Some(0o600);
    let mut file = client
        .open_with_flags_and_attributes(
            staging.to_string(),
            OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
            attributes,
        )
        .await
        .map_err(|error| {
            LumaError::SftpFailed(format!(
                "couldn't create a temporary file next to the original ({error}). Luma uploads to a temporary file and renames it into place so an interrupted upload can't truncate the original, which needs write access to the folder"
            ))
        })?;
    let written = async {
        file.write_all(bytes).await?;
        file.flush().await?;
        file.shutdown().await
    }
    .await;
    let result = match written {
        Ok(()) => finish_staging(client, staging, bytes.len() as u64, original).await,
        Err(error) => Err(LumaError::SftpFailed(format!("upload failed: {error}"))),
    };
    if result.is_err() {
        let _ = client.remove_file(staging.to_string()).await;
    }
    result
}

async fn finish_staging(
    client: &SftpSession,
    staging: &str,
    expected_len: u64,
    original: Option<&RemoteMetadata>,
) -> Result<()> {
    if let Some(original) = original {
        if let Some(mode) = original.permissions {
            let mut wanted = RemoteMetadata::empty();
            wanted.permissions = Some(mode & 0o7777);
            client
                .set_metadata(staging.to_string(), wanted)
                .await
                .map_err(|error| {
                    LumaError::SftpFailed(format!(
                        "couldn't give the upload the original file's permissions: {}",
                        remote_error(error)
                    ))
                })?;
        }
    }
    let mut staged = client
        .symlink_metadata(staging.to_string())
        .await
        .map_err(remote_error)?;
    if let Some(original) = original {
        if let (Some(uid), Some(gid)) = (original.uid, original.gid) {
            if staged.uid != Some(uid) || staged.gid != Some(gid) {
                // Both travel together in SETSTAT; sending one alone would
                // send 0 for the other.
                let mut owner = RemoteMetadata::empty();
                owner.uid = Some(uid);
                owner.gid = Some(gid);
                let _ = client.set_metadata(staging.to_string(), owner).await;
                staged = client
                    .symlink_metadata(staging.to_string())
                    .await
                    .map_err(remote_error)?;
            }
            if staged.uid != Some(uid) || staged.gid != Some(gid) {
                return Err(LumaError::SftpFailed(
                    "saving would change the file's owner or group, because the file belongs to another account. Luma replaces the file with a new copy and can't give that copy the original owner".into(),
                ));
            }
        }
    }
    if staged.size != Some(expected_len) {
        return Err(LumaError::SftpFailed(
            "the uploaded copy has the wrong size".into(),
        ));
    }
    Ok(())
}

async fn stat_remote(client: &SftpSession, path: &str) -> Result<Option<RemoteMetadata>> {
    match client.symlink_metadata(path.to_string()).await {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if is_not_found(&error) => Ok(None),
        Err(error) => Err(remote_error(error)),
    }
}

/// The remote file's content and metadata, or `None` when it does not exist.
async fn read_remote(
    client: &SftpSession,
    path: &str,
) -> Result<Option<(Vec<u8>, RemoteMetadata)>> {
    let Some(metadata) = stat_remote(client, path).await? else {
        return Ok(None);
    };
    let file_name = remote_file_name(path);
    if metadata.file_type() != RemoteFileType::File {
        return Err(LumaError::InvalidInput(format!(
            "{file_name} is not a regular file"
        )));
    }
    if metadata.size.is_some_and(|size| size > MAX_EDIT_BYTES) {
        return Err(LumaError::InvalidInput(format!(
            "{file_name} is larger than the {} MiB editing limit",
            MAX_EDIT_BYTES / (1024 * 1024)
        )));
    }
    let file = match client.open(path.to_string()).await {
        Ok(file) => file,
        Err(error) if is_not_found(&error) => return Ok(None),
        Err(error) => return Err(remote_error(error)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_EDIT_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| LumaError::SftpFailed(format!("couldn't read {file_name}: {error}")))?;
    if bytes.len() as u64 > MAX_EDIT_BYTES {
        return Err(LumaError::InvalidInput(format!(
            "{file_name} is larger than the {} MiB editing limit",
            MAX_EDIT_BYTES / (1024 * 1024)
        )));
    }
    Ok(Some((bytes, metadata)))
}

async fn read_local(path: &Path) -> std::result::Result<Vec<u8>, LocalRead> {
    let file = tokio::fs::File::open(path).await.map_err(LocalRead::Io)?;
    let mut bytes = Vec::new();
    file.take(MAX_EDIT_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(LocalRead::Io)?;
    if bytes.len() as u64 > MAX_EDIT_BYTES {
        return Err(LocalRead::TooLarge);
    }
    Ok(bytes)
}

fn read_local_blocking(path: &Path) -> std::result::Result<Vec<u8>, LocalRead> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(LocalRead::Io)?;
    let mut bytes = Vec::new();
    file.take(MAX_EDIT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(LocalRead::Io)?;
    if bytes.len() as u64 > MAX_EDIT_BYTES {
        return Err(LocalRead::TooLarge);
    }
    Ok(bytes)
}

/// An error as a sentence for the status line, without the category prefix
/// its `Display` adds.
fn sentence(error: &LumaError) -> String {
    let text = match error {
        LumaError::SftpFailed(message) | LumaError::InvalidInput(message) => message.clone(),
        other => other.to_string(),
    };
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => text,
    }
}

fn too_large_message() -> String {
    format!(
        "The saved file is larger than the {} MiB editing limit, so it was not uploaded.",
        MAX_EDIT_BYTES / (1024 * 1024)
    )
}

fn is_not_found(error: &SftpError) -> bool {
    matches!(error, SftpError::Status(status) if status.status_code == StatusCode::NoSuchFile)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn remote_file_name(path: &str) -> String {
    match path.trim_end_matches('/').rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "file".to_string(),
    }
}

/// A hidden sibling of `path`, so the renames that finish an upload stay on
/// one filesystem.
fn staging_path(path: &str, purpose: &str) -> Result<String> {
    let (parent, name) = match path.rfind('/') {
        Some(index) => (&path[..=index], &path[index + 1..]),
        None => ("", path),
    };
    let mut short = String::new();
    for character in name.chars() {
        if short.len() + character.len_utf8() > MAX_STAGING_NAME_BYTES {
            break;
        }
        short.push(character);
    }
    let id = &uuid::Uuid::new_v4().simple().to_string()[..8];
    validate_remote_path(&format!("{parent}.{short}.luma-{purpose}-{id}"))
}

/// The working copy keeps the remote name, extension included, so the editor
/// can pick a syntax mode — reduced to characters that are valid in a file name
/// on every desktop OS.
fn local_file_name(remote_name: &str) -> String {
    let sanitized = sanitize_attachment_name(remote_name);
    let trimmed = sanitized.trim_end_matches(['.', ' ']);
    let name = if trimmed.is_empty() { "file" } else { trimmed };
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        format!("_{name}")
    } else {
        name.to_string()
    }
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
    let temp = dir.join(format!("{MANIFEST_FILE}.tmp"));
    write_private_file(&temp, &bytes)?;
    std::fs::rename(temp, dir.join(MANIFEST_FILE))
}

/// Swap new content into the working copy by rename so the editor sees one
/// change, falling back to an in-place write when the editor holds the file in
/// a way that blocks renaming over it.
fn replace_local_file(dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = dir.join(format!(".luma-replace-{}", uuid::Uuid::new_v4().simple()));
    write_private_file(&temp, bytes)?;
    if std::fs::rename(&temp, path).is_ok() {
        return Ok(());
    }
    let _ = std::fs::remove_file(&temp);
    write_private_file(path, bytes)
}

fn prune_backups(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<_> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    if names.len() <= MAX_BACKUPS {
        return;
    }
    // Names start with a sortable timestamp, so the oldest sort first.
    names.sort();
    for path in &names[..names.len() - MAX_BACKUPS] {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests;
