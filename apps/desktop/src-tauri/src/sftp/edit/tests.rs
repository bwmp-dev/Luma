use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};

use super::*;

/*
 * These tests run the real russh-sftp client against an in-memory SFTP server,
 * so every upload, rename and stat goes through the protocol exactly as it
 * would against a host — without touching any real remote file.
 */

const SERVER_UID: u32 = 1000;
const SERVER_GID: u32 = 1000;

type Hook = Box<dyn FnOnce(&mut FakeFs) + Send>;

#[derive(Clone)]
struct FakeFile {
    data: Vec<u8>,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: u32,
}

#[derive(Default)]
struct FakeFs {
    files: HashMap<String, FakeFile>,
    dirs: HashSet<String>,
    /// Directories where creating a file is refused, like a folder the user
    /// can read but not write.
    read_only_dirs: HashSet<String>,
    /// Fail any write that would extend a file past this many bytes, the way a
    /// dropped connection ends an upload part-way.
    fail_writes_after: Option<usize>,
    /// Runs once, right after the first write to a staged upload: another
    /// writer acting while the upload is in flight.
    during_upload: Option<Hook>,
    /// Renames whose source path contains any of these fail.
    fail_renames_from: Vec<&'static str>,
    clock: u32,
}

impl FakeFs {
    fn tick(&mut self) -> u32 {
        self.clock += 1;
        self.clock
    }

    fn put(&mut self, path: &str, data: &[u8], mode: u32) {
        let mtime = self.tick();
        self.files.insert(
            path.to_string(),
            FakeFile {
                data: data.to_vec(),
                mode,
                uid: SERVER_UID,
                gid: SERVER_GID,
                mtime,
            },
        );
    }

    fn data(&self, path: &str) -> Option<Vec<u8>> {
        self.files.get(path).map(|file| file.data.clone())
    }

    fn attrs(&self, path: &str) -> Option<FileAttributes> {
        if let Some(file) = self.files.get(path) {
            let mut attrs = FileAttributes::empty();
            attrs.size = Some(file.data.len() as u64);
            attrs.permissions = Some(0o100000 | file.mode);
            attrs.uid = Some(file.uid);
            attrs.gid = Some(file.gid);
            attrs.mtime = Some(file.mtime);
            attrs.atime = Some(file.mtime);
            return Some(attrs);
        }
        self.dirs.contains(path).then(|| {
            let mut attrs = FileAttributes::empty();
            attrs.permissions = Some(0o040755);
            attrs
        })
    }

    /// Everything Luma created on the remote besides the files themselves.
    fn leftovers(&self) -> Vec<String> {
        self.files
            .keys()
            .filter(|path| path.contains(".luma-"))
            .cloned()
            .collect()
    }
}

fn parent(path: &str) -> String {
    match path.rfind('/') {
        Some(0) => "/".into(),
        Some(index) => path[..index].into(),
        None => "/".into(),
    }
}

struct FakeServer {
    fs: Arc<Mutex<FakeFs>>,
    handles: HashMap<String, String>,
    next_handle: u32,
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
    }
}

impl russh_sftp::server::Handler for FakeServer {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> std::result::Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: OpenFlags,
        attrs: FileAttributes,
    ) -> std::result::Result<Handle, Self::Error> {
        let mut fs = self.fs.lock().unwrap();
        let exists = fs.files.contains_key(&filename);
        if exists {
            if flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUDE) {
                return Err(StatusCode::Failure);
            }
            if flags.contains(OpenFlags::TRUNCATE) {
                let mtime = fs.tick();
                let file = fs.files.get_mut(&filename).unwrap();
                file.data.clear();
                file.mtime = mtime;
            }
        } else if flags.contains(OpenFlags::CREATE) {
            let dir = parent(&filename);
            if !fs.dirs.contains(&dir) {
                return Err(StatusCode::NoSuchFile);
            }
            if fs.read_only_dirs.contains(&dir) {
                return Err(StatusCode::PermissionDenied);
            }
            fs.put(&filename, b"", attrs.permissions.unwrap_or(0o644) & 0o7777);
        } else {
            return Err(StatusCode::NoSuchFile);
        }
        self.next_handle += 1;
        let handle = format!("h{}", self.next_handle);
        self.handles.insert(handle.clone(), filename);
        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> std::result::Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> std::result::Result<Data, Self::Error> {
        let path = self.handles.get(&handle).ok_or(StatusCode::Failure)?;
        let fs = self.fs.lock().unwrap();
        let data = &fs.files.get(path).ok_or(StatusCode::NoSuchFile)?.data;
        let start = offset as usize;
        if start >= data.len() {
            return Err(StatusCode::Eof);
        }
        let end = (start + len as usize).min(data.len());
        Ok(Data {
            id,
            data: data[start..end].to_vec(),
        })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> std::result::Result<Status, Self::Error> {
        let path = self
            .handles
            .get(&handle)
            .ok_or(StatusCode::Failure)?
            .clone();
        let mut fs = self.fs.lock().unwrap();
        let end = offset as usize + data.len();
        if fs.fail_writes_after.is_some_and(|limit| end > limit) {
            return Err(StatusCode::ConnectionLost);
        }
        let mtime = fs.tick();
        let file = fs.files.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
        if file.data.len() < end {
            file.data.resize(end, 0);
        }
        file.data[offset as usize..end].copy_from_slice(&data);
        file.mtime = mtime;
        if path.contains(".luma-edit-") {
            if let Some(hook) = fs.during_upload.take() {
                hook(&mut fs);
            }
        }
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> std::result::Result<Attrs, Self::Error> {
        let fs = self.fs.lock().unwrap();
        let attrs = fs.attrs(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(Attrs { id, attrs })
    }

    async fn stat(&mut self, id: u32, path: String) -> std::result::Result<Attrs, Self::Error> {
        self.lstat(id, path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> std::result::Result<Attrs, Self::Error> {
        let path = self
            .handles
            .get(&handle)
            .ok_or(StatusCode::Failure)?
            .clone();
        self.lstat(id, path).await
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> std::result::Result<Status, Self::Error> {
        let mut fs = self.fs.lock().unwrap();
        let file = fs.files.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
        if let (Some(uid), Some(gid)) = (attrs.uid, attrs.gid) {
            // An unprivileged account cannot give a file away.
            if uid != SERVER_UID || gid != SERVER_GID {
                return Err(StatusCode::PermissionDenied);
            }
            file.uid = uid;
            file.gid = gid;
        }
        if let Some(mode) = attrs.permissions {
            file.mode = mode & 0o7777;
        }
        Ok(ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> std::result::Result<Status, Self::Error> {
        let path = self
            .handles
            .get(&handle)
            .ok_or(StatusCode::Failure)?
            .clone();
        self.setstat(id, path, attrs).await
    }

    async fn remove(
        &mut self,
        id: u32,
        filename: String,
    ) -> std::result::Result<Status, Self::Error> {
        let mut fs = self.fs.lock().unwrap();
        fs.files.remove(&filename).ok_or(StatusCode::NoSuchFile)?;
        Ok(ok(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> std::result::Result<Status, Self::Error> {
        let mut fs = self.fs.lock().unwrap();
        if fs
            .fail_renames_from
            .iter()
            .any(|source| oldpath.contains(source))
        {
            return Err(StatusCode::Failure);
        }
        // SFTP v3 (and OpenSSH) refuse to rename over an existing file.
        if fs.files.contains_key(&newpath) {
            return Err(StatusCode::Failure);
        }
        let file = fs.files.remove(&oldpath).ok_or(StatusCode::NoSuchFile)?;
        fs.files.insert(newpath, file);
        Ok(ok(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> std::result::Result<Name, Self::Error> {
        let path = if path.starts_with('/') {
            path
        } else {
            format!("/home/user/{path}")
        };
        Ok(Name {
            id,
            files: vec![File::dummy(path)],
        })
    }
}

struct TestHost {
    client: Mutex<Option<(String, Arc<SftpSession>)>>,
}

impl EditHost for TestHost {
    fn client(&self, _host_id: &str) -> Option<(String, Arc<SftpSession>)> {
        self.client.lock().unwrap().clone()
    }

    fn publish(&self, _edit: &RemoteEditInfo) {}

    fn removed(&self, _edit_id: &str) {}
}

struct Fixture {
    fs: Arc<Mutex<FakeFs>>,
    client: Arc<SftpSession>,
    host: Arc<TestHost>,
    root: PathBuf,
    manager: RemoteEditManager,
}

const HOST: &str = "host-1";
const PATH: &str = "/srv/app/config.yml";

impl Fixture {
    async fn new() -> Self {
        let fs = Arc::new(Mutex::new(FakeFs::default()));
        {
            let mut fs = fs.lock().unwrap();
            fs.dirs.insert("/".into());
            fs.dirs.insert("/srv".into());
            fs.dirs.insert("/srv/app".into());
            fs.put(PATH, b"port: 80\n", 0o640);
        }
        let client = connect(&fs).await;
        let host = Arc::new(TestHost {
            client: Mutex::new(Some(("session-1".into(), Arc::clone(&client)))),
        });
        let root = std::env::temp_dir().join(format!("luma-edit-test-{}", uuid::Uuid::new_v4()));
        let manager = test_manager(&root, &host);
        Self {
            fs,
            client,
            host,
            root,
            manager,
        }
    }

    async fn open(&self) -> RemoteEditInfo {
        self.manager
            .open(HOST, &self.client, PATH, "test-editor", |_| Ok(()))
            .await
            .unwrap()
    }

    fn remote(&self) -> Vec<u8> {
        self.fs.lock().unwrap().data(PATH).unwrap()
    }

    /// Two polls: the first notices the new size and mtime, the second acts
    /// once they have held still.
    async fn settle(&self, edit_id: &str) {
        self.manager.poll(edit_id).await;
        self.manager.poll(edit_id).await;
    }

    async fn info(&self, edit_id: &str) -> RemoteEditInfo {
        self.manager
            .list()
            .await
            .into_iter()
            .find(|edit| edit.edit_id == edit_id)
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn test_manager(root: &Path, host: &Arc<TestHost>) -> RemoteEditManager {
    // Polls are driven by the tests; the interval only has to stay out of the way.
    RemoteEditManager::with_poll_interval(
        root.to_path_buf(),
        Arc::clone(host) as Arc<dyn EditHost>,
        Duration::from_secs(3600),
    )
}

async fn connect(fs: &Arc<Mutex<FakeFs>>) -> Arc<SftpSession> {
    let (client_io, server_io) = tokio::io::duplex(1 << 20);
    russh_sftp::server::run(
        server_io,
        FakeServer {
            fs: Arc::clone(fs),
            handles: HashMap::new(),
            next_handle: 0,
        },
    )
    .await;
    Arc::new(SftpSession::new(client_io).await.unwrap())
}

fn save(info: &RemoteEditInfo, content: &[u8]) {
    std::fs::write(&info.local_path, content).unwrap();
}

#[tokio::test]
async fn editing_a_file_named_edit_json_keeps_content_separate_from_the_manifest() {
    let fixture = Fixture::new().await;
    let path = "/srv/app/edit.json";
    let content = br#"{"setting":true}"#;
    fixture.fs.lock().unwrap().put(path, content, 0o600);
    let info = fixture
        .manager
        .open(HOST, &fixture.client, path, "test-editor", |_| Ok(()))
        .await
        .unwrap();
    assert_eq!(std::fs::read(&info.local_path).unwrap(), content);
    fixture.settle(&info.edit_id).await;
    assert_eq!(fixture.fs.lock().unwrap().data(path).unwrap(), content);
}

#[tokio::test]
async fn closing_an_oversized_saved_copy_requires_explicit_discard() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture.settle(&info.edit_id).await;
    save(&info, &vec![b'x'; MAX_EDIT_BYTES as usize + 1]);
    fixture.settle(&info.edit_id).await;
    assert!(fixture.manager.close(&info.edit_id, false).await.is_err());
    assert!(Path::new(&info.local_path).exists());
}

#[tokio::test]
async fn remote_names_matching_luma_files_sync_and_survive_a_restart() {
    let fixture = Fixture::new().await;
    let names = [
        "EDIT.JSON",
        "Edit.Json",
        "edit.json.tmp",
        "content",
        "backups",
    ];
    let mut opened = Vec::new();
    for name in names {
        let path = format!("/srv/app/{name}");
        fixture.fs.lock().unwrap().put(&path, b"original\n", 0o644);
        let info = fixture
            .manager
            .open(HOST, &fixture.client, &path, "test-editor", |_| Ok(()))
            .await
            .unwrap();
        save(&info, format!("saved {name}\n").as_bytes());
        fixture.settle(&info.edit_id).await;
        opened.push((path, info));
    }
    for (path, info) in &opened {
        let name = path.rsplit('/').next().unwrap();
        assert_eq!(
            fixture.fs.lock().unwrap().data(path).unwrap(),
            format!("saved {name}\n").as_bytes()
        );
        assert_eq!(fixture.info(&info.edit_id).await.status, EditStatus::Synced);
    }

    // The manifests are intact, so a restart finds every edit again.
    let restarted = test_manager(&fixture.root, &fixture.host);
    let mut restored: Vec<_> = restarted
        .list()
        .await
        .into_iter()
        .map(|edit| edit.remote_path)
        .collect();
    restored.sort();
    let mut expected: Vec<_> = opened.into_iter().map(|(path, _)| path).collect();
    expected.sort();
    assert_eq!(restored, expected);
}

#[tokio::test]
async fn an_oversized_save_is_reported_as_unsynced_and_kept_across_a_restart() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    let big = vec![b'x'; MAX_EDIT_BYTES as usize + 1];
    save(&info, &big);
    fixture.settle(&info.edit_id).await;

    let failed = fixture.info(&info.edit_id).await;
    assert_eq!(failed.status, EditStatus::Error);
    assert!(failed.unsynced);
    assert!(failed.message.unwrap().contains("editing limit"));
    assert_eq!(fixture.remote(), b"port: 80\n");
    let refused = fixture
        .manager
        .close(&info.edit_id, false)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("too large"));

    fixture.manager.shutdown();
    let restarted = test_manager(&fixture.root, &fixture.host);
    let restored = restarted.list().await;
    assert_eq!(restored.len(), 1);
    assert!(restored[0].unsynced);
    assert_eq!(std::fs::read(&info.local_path).unwrap(), big);

    restarted.close(&info.edit_id, true).await.unwrap();
    assert!(!Path::new(&info.local_path).exists());
}

#[tokio::test]
async fn a_save_of_exactly_the_size_limit_uploads() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    let limit = vec![b'y'; MAX_EDIT_BYTES as usize];
    save(&info, &limit);
    fixture.settle(&info.edit_id).await;

    assert_eq!(fixture.remote(), limit);
    assert_eq!(fixture.info(&info.edit_id).await.status, EditStatus::Synced);
}

#[tokio::test]
async fn a_missing_working_copy_is_not_deleted_as_if_it_were_synced() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    std::fs::remove_file(&info.local_path).unwrap();

    let refused = fixture
        .manager
        .close(&info.edit_id, false)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("couldn't be read"));
    assert!(fixture.info(&info.edit_id).await.unsynced);
    assert!(Path::new(&info.local_path).parent().unwrap().exists());

    fixture.manager.close(&info.edit_id, true).await.unwrap();
    assert!(fixture.manager.list().await.is_empty());
}

#[tokio::test]
async fn a_save_that_is_no_longer_text_is_not_uploaded() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    save(&info, b"port: 80\n\x00\x01");
    fixture.settle(&info.edit_id).await;

    let refused = fixture.info(&info.edit_id).await;
    assert_eq!(refused.status, EditStatus::Error);
    assert!(refused.unsynced);
    assert!(refused.message.unwrap().contains("binary"));
    assert_eq!(fixture.remote(), b"port: 80\n");
    fixture.manager.upload(&info.edit_id).await.unwrap();
    assert_eq!(fixture.remote(), b"port: 80\n");

    // Converting to another supported encoding is a normal save.
    let utf16 = [0xFF, 0xFE, b'o', 0, b'k', 0, b'\n', 0];
    save(&info, &utf16);
    fixture.settle(&info.edit_id).await;
    assert_eq!(fixture.remote(), utf16);
    let synced = fixture.info(&info.edit_id).await;
    assert_eq!(synced.status, EditStatus::Synced);
    assert_eq!(synced.encoding, TextEncoding::Utf16Le);
}

#[tokio::test]
async fn a_same_size_remote_write_during_the_upload_is_a_conflict() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    // Same length, mtime untouched: invisible to a metadata check.
    fixture.fs.lock().unwrap().during_upload = Some(Box::new(|fs: &mut FakeFs| {
        fs.files.get_mut(PATH).unwrap().data = b"port: 99\n".to_vec();
    }));

    save(&info, b"port: 8080\n");
    fixture.settle(&info.edit_id).await;

    assert_eq!(fixture.remote(), b"port: 99\n");
    assert!(fixture.fs.lock().unwrap().leftovers().is_empty());
    let paused = fixture.info(&info.edit_id).await;
    assert_eq!(paused.conflict, Some(ConflictKind::Changed));
    let review = fixture.manager.review(&info.edit_id).await.unwrap();
    assert_eq!(review.remote_text.as_deref(), Some("port: 99\n"));
}

#[tokio::test]
async fn a_permission_change_during_the_upload_is_not_undone() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture.fs.lock().unwrap().during_upload = Some(Box::new(|fs: &mut FakeFs| {
        fs.files.get_mut(PATH).unwrap().mode = 0o600;
    }));

    save(&info, b"port: 8080\n");
    fixture.settle(&info.edit_id).await;

    {
        let fs = fixture.fs.lock().unwrap();
        assert_eq!(fs.files[PATH].data, b"port: 80\n");
        assert_eq!(fs.files[PATH].mode, 0o600);
        assert!(fs.leftovers().is_empty());
    }
    let failed = fixture.info(&info.edit_id).await;
    assert_eq!(failed.status, EditStatus::Error);
    assert!(failed
        .message
        .unwrap()
        .contains("permissions or owner changed"));

    fixture.manager.upload(&info.edit_id).await.unwrap();
    let fs = fixture.fs.lock().unwrap();
    assert_eq!(fs.files[PATH].data, b"port: 8080\n");
    assert_eq!(fs.files[PATH].mode, 0o600);
}

#[tokio::test]
async fn a_failed_swap_puts_the_original_back() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture.fs.lock().unwrap().fail_renames_from = vec![".luma-edit-"];

    save(&info, b"port: 8080\n");
    fixture.settle(&info.edit_id).await;

    assert_eq!(fixture.remote(), b"port: 80\n");
    assert!(fixture.fs.lock().unwrap().leftovers().is_empty());
    let failed = fixture.info(&info.edit_id).await;
    assert_eq!(failed.status, EditStatus::Error);
    assert!(failed.unsynced);
    assert!(failed
        .message
        .unwrap()
        .contains("original file is unchanged"));
}

#[tokio::test]
async fn a_failed_rollback_reports_where_the_original_is() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture.fs.lock().unwrap().fail_renames_from = vec![".luma-edit-", ".luma-backup-"];

    save(&info, b"port: 8080\n");
    fixture.settle(&info.edit_id).await;

    let find = |tag: &str| {
        let fs = fixture.fs.lock().unwrap();
        let path = fs
            .leftovers()
            .into_iter()
            .find(|path| path.contains(tag))
            .unwrap();
        (path.clone(), fs.data(&path).unwrap())
    };
    let (backup, original) = find(".luma-backup-");
    let (staged, upload) = find(".luma-edit-");
    assert!(fixture.fs.lock().unwrap().data(PATH).is_none());
    assert_eq!(original, b"port: 80\n");
    assert_eq!(upload, b"port: 8080\n");
    let message = fixture.info(&info.edit_id).await.message.unwrap();
    assert!(message.contains(&backup), "{message}");
    assert!(message.contains(&staged), "{message}");
}

#[tokio::test]
async fn each_save_is_uploaded_through_a_staged_rename_that_keeps_the_mode() {
    let fixture = Fixture::new().await;
    let launched = Mutex::new(Vec::new());
    let info = fixture
        .manager
        .open(HOST, &fixture.client, PATH, "test-editor", |path| {
            launched.lock().unwrap().push(path.to_path_buf());
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        launched.lock().unwrap().as_slice(),
        [PathBuf::from(&info.local_path)]
    );
    assert_eq!(std::fs::read(&info.local_path).unwrap(), b"port: 80\n");
    assert!(info.local_path.ends_with("config.yml"));

    save(&info, b"port: 8080\nhost: 0.0.0.0\n");
    fixture.settle(&info.edit_id).await;

    assert_eq!(fixture.remote(), b"port: 8080\nhost: 0.0.0.0\n");
    {
        let fs = fixture.fs.lock().unwrap();
        assert_eq!(fs.files[PATH].mode, 0o640);
        assert!(fs.leftovers().is_empty(), "{:?}", fs.leftovers());
    }
    let info = fixture.info(&info.edit_id).await;
    assert_eq!(info.status, EditStatus::Synced);
    assert!(!info.unsynced);
    assert!(info.last_uploaded_at.is_some());
}

#[tokio::test]
async fn an_editor_saving_by_rename_is_seen_as_a_save() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    let local = PathBuf::from(&info.local_path);
    let temp = local.with_file_name(".config.yml.swp");
    std::fs::write(&temp, b"port: 443\ntls: true\n").unwrap();
    std::fs::rename(&temp, &local).unwrap();

    fixture.settle(&info.edit_id).await;

    assert_eq!(fixture.remote(), b"port: 443\ntls: true\n");
}

#[tokio::test]
async fn a_remote_change_pauses_uploads_and_keep_local_overwrites_the_reviewed_version() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture
        .fs
        .lock()
        .unwrap()
        .put(PATH, b"port: 81\n# edited on the server\n", 0o640);

    save(&info, b"port: 8080\n");
    fixture.settle(&info.edit_id).await;

    let paused = fixture.info(&info.edit_id).await;
    assert_eq!(paused.status, EditStatus::Conflict);
    assert_eq!(paused.conflict, Some(ConflictKind::Changed));
    assert_eq!(fixture.remote(), b"port: 81\n# edited on the server\n");

    // Saving again while paused still does not upload.
    save(&info, b"port: 8081\n");
    fixture.settle(&info.edit_id).await;
    assert_eq!(fixture.remote(), b"port: 81\n# edited on the server\n");

    let review = fixture.manager.review(&info.edit_id).await.unwrap();
    assert_eq!(
        review.remote_text.as_deref(),
        Some("port: 81\n# edited on the server\n")
    );
    assert_eq!(review.local_text.as_deref(), Some("port: 8081\n"));
    assert!(!review.remote_deleted);

    fixture
        .manager
        .resolve(&info.edit_id, Resolution::KeepLocal)
        .await
        .unwrap();
    assert_eq!(fixture.remote(), b"port: 8081\n");
    assert_eq!(fixture.info(&info.edit_id).await.status, EditStatus::Synced);
}

#[tokio::test]
async fn keep_local_checks_again_when_the_remote_moved_after_review() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture
        .fs
        .lock()
        .unwrap()
        .put(PATH, b"first writer\n", 0o640);
    save(&info, b"mine\n");
    fixture.settle(&info.edit_id).await;
    fixture
        .fs
        .lock()
        .unwrap()
        .put(PATH, b"second writer!\n", 0o640);

    fixture
        .manager
        .resolve(&info.edit_id, Resolution::KeepLocal)
        .await
        .unwrap();

    assert_eq!(fixture.remote(), b"second writer!\n");
    assert_eq!(
        fixture.info(&info.edit_id).await.status,
        EditStatus::Conflict
    );
    let review = fixture.manager.review(&info.edit_id).await.unwrap();
    assert_eq!(review.remote_text.as_deref(), Some("second writer!\n"));
}

#[tokio::test]
async fn use_remote_backs_up_the_working_copy_and_never_writes_the_remote() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture.fs.lock().unwrap().put(PATH, b"theirs\n", 0o640);
    save(&info, b"mine, unsaved elsewhere\n");
    fixture.settle(&info.edit_id).await;
    let writes_before = fixture.fs.lock().unwrap().clock;

    fixture
        .manager
        .resolve(&info.edit_id, Resolution::UseRemote)
        .await
        .unwrap();

    assert_eq!(std::fs::read(&info.local_path).unwrap(), b"theirs\n");
    assert_eq!(fixture.fs.lock().unwrap().clock, writes_before);
    let resolved = fixture.info(&info.edit_id).await;
    assert_eq!(resolved.status, EditStatus::Synced);
    assert!(!resolved.unsynced);
    let backup = resolved.backup_path.expect("backup path");
    assert_eq!(std::fs::read(backup).unwrap(), b"mine, unsaved elsewhere\n");

    // The working copy is based on the remote version now, so the next save
    // uploads without a conflict.
    save(&info, b"theirs\nand mine\n");
    fixture.settle(&info.edit_id).await;
    assert_eq!(fixture.remote(), b"theirs\nand mine\n");
}

#[tokio::test]
async fn a_touched_but_identical_remote_file_is_not_a_conflict() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    {
        let mut fs = fixture.fs.lock().unwrap();
        let mtime = fs.tick() + 1000;
        fs.files.get_mut(PATH).unwrap().mtime = mtime;
    }

    save(&info, b"port: 9000\n");
    fixture.settle(&info.edit_id).await;

    assert_eq!(fixture.remote(), b"port: 9000\n");
    assert_eq!(fixture.info(&info.edit_id).await.status, EditStatus::Synced);
}

#[tokio::test]
async fn the_same_edit_made_remotely_is_not_a_conflict() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture.fs.lock().unwrap().put(PATH, b"port: 8443\n", 0o640);

    save(&info, b"port: 8443\n");
    fixture.settle(&info.edit_id).await;

    let synced = fixture.info(&info.edit_id).await;
    assert_eq!(synced.status, EditStatus::Synced);
    assert!(!synced.unsynced);
}

#[tokio::test]
async fn an_interrupted_upload_leaves_the_remote_file_intact() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture.fs.lock().unwrap().fail_writes_after = Some(4);

    save(&info, b"a much longer replacement\n");
    fixture.settle(&info.edit_id).await;

    assert_eq!(fixture.remote(), b"port: 80\n");
    assert!(fixture.fs.lock().unwrap().leftovers().is_empty());
    let failed = fixture.info(&info.edit_id).await;
    assert_eq!(failed.status, EditStatus::Error);
    assert!(failed.unsynced);
    assert_eq!(
        std::fs::read(&info.local_path).unwrap(),
        b"a much longer replacement\n"
    );

    // Nothing changed, so polling does not hammer the server with retries...
    fixture.fs.lock().unwrap().fail_writes_after = None;
    fixture.settle(&info.edit_id).await;
    assert_eq!(fixture.remote(), b"port: 80\n");

    // ...but an explicit retry goes through.
    fixture.manager.upload(&info.edit_id).await.unwrap();
    assert_eq!(fixture.remote(), b"a much longer replacement\n");
}

#[tokio::test]
async fn a_folder_without_write_access_is_reported_and_changes_are_kept() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture
        .fs
        .lock()
        .unwrap()
        .read_only_dirs
        .insert("/srv/app".into());

    save(&info, b"port: 1\n");
    fixture.settle(&info.edit_id).await;

    let failed = fixture.info(&info.edit_id).await;
    assert_eq!(failed.status, EditStatus::Error);
    assert!(failed.message.unwrap().contains("temporary file"));
    assert_eq!(fixture.remote(), b"port: 80\n");
    assert_eq!(std::fs::read(&info.local_path).unwrap(), b"port: 1\n");
}

#[tokio::test]
async fn a_file_owned_by_another_account_is_not_silently_given_away() {
    let fixture = Fixture::new().await;
    fixture.fs.lock().unwrap().files.get_mut(PATH).unwrap().uid = 0;
    let info = fixture.open().await;

    save(&info, b"port: 2\n");
    fixture.settle(&info.edit_id).await;

    let failed = fixture.info(&info.edit_id).await;
    assert_eq!(failed.status, EditStatus::Error);
    assert!(failed.message.unwrap().contains("owner"));
    let fs = fixture.fs.lock().unwrap();
    assert_eq!(fs.files[PATH].data, b"port: 80\n");
    assert_eq!(fs.files[PATH].uid, 0);
    assert!(fs.leftovers().is_empty());
}

#[tokio::test]
async fn a_deleted_remote_file_is_a_conflict_that_can_recreate_it() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture.fs.lock().unwrap().files.remove(PATH);

    save(&info, b"port: 3\n");
    fixture.settle(&info.edit_id).await;

    let paused = fixture.info(&info.edit_id).await;
    assert_eq!(paused.conflict, Some(ConflictKind::Deleted));
    assert!(fixture
        .manager
        .resolve(&info.edit_id, Resolution::UseRemote)
        .await
        .is_err());
    fixture
        .manager
        .resolve(&info.edit_id, Resolution::KeepLocal)
        .await
        .unwrap();
    assert_eq!(fixture.remote(), b"port: 3\n");
}

#[tokio::test]
async fn edits_made_offline_upload_once_a_session_is_back() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    let session = fixture.host.client.lock().unwrap().take();

    save(&info, b"port: 4\n");
    fixture.settle(&info.edit_id).await;
    let offline = fixture.info(&info.edit_id).await;
    assert_eq!(offline.status, EditStatus::Offline);
    assert!(!offline.connected);
    assert_eq!(fixture.remote(), b"port: 80\n");

    *fixture.host.client.lock().unwrap() = session;
    fixture.manager.poll(&info.edit_id).await;
    assert_eq!(fixture.remote(), b"port: 4\n");
    assert_eq!(fixture.info(&info.edit_id).await.status, EditStatus::Synced);
}

#[tokio::test]
async fn opening_the_same_file_twice_reuses_its_working_copy() {
    let fixture = Fixture::new().await;
    let first = fixture.open().await;
    save(&first, b"port: 5\n");
    let launched = Mutex::new(None);
    let second = fixture
        .manager
        .open(HOST, &fixture.client, PATH, "other-editor", |path| {
            *launched.lock().unwrap() = Some(path.to_path_buf());
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(second.edit_id, first.edit_id);
    assert_eq!(second.editor_id.as_deref(), Some("other-editor"));
    assert_eq!(
        launched.lock().unwrap().as_deref(),
        Some(Path::new(&first.local_path))
    );
    // Reopening never re-downloads over the user's edits.
    assert_eq!(std::fs::read(&first.local_path).unwrap(), b"port: 5\n");
    assert_eq!(fixture.manager.list().await.len(), 1);
}

#[tokio::test]
async fn a_failed_launch_leaves_nothing_behind() {
    let fixture = Fixture::new().await;
    let error = fixture
        .manager
        .open(HOST, &fixture.client, PATH, "test-editor", |_| {
            Err(LumaError::InvalidInput("no editor".into()))
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no editor"));
    assert!(fixture.manager.list().await.is_empty());
    let leftover = std::fs::read_dir(&fixture.root)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(leftover, 0);
}

#[tokio::test]
async fn closing_refuses_to_drop_unsynced_changes_unless_told_to() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture.host.client.lock().unwrap().take();
    save(&info, b"port: 6\n");

    let refused = fixture
        .manager
        .close(&info.edit_id, false)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("haven't been uploaded"));
    assert!(Path::new(&info.local_path).exists());

    fixture.manager.close(&info.edit_id, true).await.unwrap();
    assert!(!Path::new(&info.local_path).exists());
    assert!(fixture.manager.list().await.is_empty());
}

#[tokio::test]
async fn unsynced_edits_survive_a_restart_and_wait_for_an_explicit_upload() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    let session = fixture.host.client.lock().unwrap().take();
    save(&info, b"port: 7\n");
    fixture.settle(&info.edit_id).await;
    fixture.manager.shutdown();
    *fixture.host.client.lock().unwrap() = session;

    let restarted = test_manager(&fixture.root, &fixture.host);
    let restored = restarted.list().await;
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].edit_id, info.edit_id);
    assert_eq!(restored[0].status, EditStatus::Recovered);
    assert_eq!(restored[0].remote_path, PATH);

    restarted.poll(&info.edit_id).await;
    restarted.poll(&info.edit_id).await;
    assert_eq!(fixture.remote(), b"port: 80\n");

    restarted.upload(&info.edit_id).await.unwrap();
    assert_eq!(fixture.remote(), b"port: 7\n");
}

#[tokio::test]
async fn quitting_cleans_up_working_copies_that_match_the_remote() {
    let fixture = Fixture::new().await;
    let info = fixture.open().await;
    fixture.manager.shutdown();
    assert!(!Path::new(&info.local_path).exists());
    assert!(test_manager(&fixture.root, &fixture.host)
        .list()
        .await
        .is_empty());
}

#[tokio::test]
async fn binary_and_unsupported_files_are_refused_before_anything_is_written() {
    let fixture = Fixture::new().await;
    fixture
        .fs
        .lock()
        .unwrap()
        .put("/srv/app/app.bin", b"\x7fELF\x00\x01", 0o755);
    fixture
        .fs
        .lock()
        .unwrap()
        .put("/srv/app/latin1.txt", b"caf\xe9\n", 0o644);

    for path in [
        "/srv/app/app.bin",
        "/srv/app/latin1.txt",
        "/srv/app/missing",
    ] {
        let result = fixture
            .manager
            .open(HOST, &fixture.client, path, "test-editor", |_| {
                panic!("an editor must not be launched for {path}")
            })
            .await;
        assert!(result.is_err(), "{path} opened");
    }
    assert!(fixture.manager.list().await.is_empty());
}

#[tokio::test]
async fn an_oversized_file_is_refused() {
    let fixture = Fixture::new().await;
    let big = vec![b'a'; MAX_EDIT_BYTES as usize + 1];
    fixture
        .fs
        .lock()
        .unwrap()
        .put("/srv/app/big.log", &big, 0o644);
    let error = fixture
        .manager
        .open(
            HOST,
            &fixture.client,
            "/srv/app/big.log",
            "test-editor",
            |_| Ok(()),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("editing limit"));
}

#[test]
fn encoding_detection_accepts_round_trippable_text_only() {
    assert_eq!(TextEncoding::detect(b"plain\r\n"), Ok(TextEncoding::Utf8));
    assert_eq!(TextEncoding::detect(b""), Ok(TextEncoding::Utf8));
    assert_eq!(
        TextEncoding::detect(b"\xEF\xBB\xBFwith bom"),
        Ok(TextEncoding::Utf8Bom)
    );
    let utf16le = [0xFF, 0xFE, b'h', 0, b'i', 0];
    assert_eq!(TextEncoding::detect(&utf16le), Ok(TextEncoding::Utf16Le));
    assert_eq!(decode_text(&utf16le).as_deref(), Some("hi"));
    let utf16be = [0xFE, 0xFF, 0, b'h', 0, b'i'];
    assert_eq!(TextEncoding::detect(&utf16be), Ok(TextEncoding::Utf16Be));
    assert_eq!(decode_text(&utf16be).as_deref(), Some("hi"));

    assert!(TextEncoding::detect(b"bin\x00ary").is_err());
    assert!(TextEncoding::detect(b"\xEF\xBB\xBFbin\x00ary").is_err());
    assert!(TextEncoding::detect(&[0xFF, 0xFE, b'a', 0, 0, 0, b'b', 0]).is_err());
    assert!(TextEncoding::detect(&[0xFE, 0xFF, 0, b'a', 0, 0]).is_err());
    assert!(TextEncoding::detect(b"caf\xe9").is_err());
    assert!(TextEncoding::detect(&[0xFF, 0xFE, 0, 0, b'a', 0, 0, 0]).is_err());
    // An odd byte count or an unpaired surrogate is not valid UTF-16.
    assert!(TextEncoding::detect(&[0xFF, 0xFE, b'a']).is_err());
    assert!(TextEncoding::detect(&[0xFF, 0xFE, 0x00, 0xD8]).is_err());
}

#[test]
fn working_copy_names_are_safe_on_every_desktop_os() {
    assert_eq!(local_file_name("config.yml"), "config.yml");
    assert_eq!(local_file_name(".env"), ".env");
    assert_eq!(local_file_name("a:b|c?.txt"), "a_b_c_.txt");
    assert_eq!(local_file_name("con.txt"), "_con.txt");
    assert_eq!(local_file_name("COM1"), "_COM1");
    assert_eq!(local_file_name("console.log"), "console.log");
    assert_eq!(local_file_name("trailing."), "trailing");
    assert_eq!(local_file_name(".."), "attachment");
}

#[test]
fn staging_files_are_hidden_siblings() {
    let staging = staging_path("/etc/nginx/nginx.conf", "edit").unwrap();
    assert!(staging.starts_with("/etc/nginx/.nginx.conf.luma-edit-"));
    let long = format!("/tmp/{}", "x".repeat(400));
    let staging = staging_path(&long, "edit").unwrap();
    let name = staging.rsplit('/').next().unwrap();
    assert!(name.len() < 255);
}
