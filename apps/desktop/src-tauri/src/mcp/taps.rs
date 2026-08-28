//! Recent-output buffers for panes shared with an agent.
//!
//! Terminal bytes never pass through React state, so there is no scrollback in
//! Rust to read. This taps the byte stream at the same point `agent_events`
//! taps it — the session's data callback — and keeps a bounded buffer of what
//! recently arrived.
//!
//! Two properties matter more than anything else here:
//!
//! * **A pane nobody shared must cost nothing.** These callbacks run on the SSH
//!   session task and the PTY reader thread at up to 64 KiB per read, so the
//!   unshared path is one relaxed atomic load — no allocation, no map lookup,
//!   no lock. The ring is not allocated until the user shares the pane.
//! * **What an agent reads is text, not a screen.** A terminal stream is screen
//!   *mutations* — cursor moves, colour, erase-in-display — so the raw bytes
//!   are unreadable. The escape sequences are stripped as they arrive, by a
//!   state machine that persists across chunks, because a sequence split over
//!   two reads is the normal case rather than the exception.
//!
//! Stripping makes line-oriented output (build logs, `tail -f`, shell prompts)
//! read correctly. It cannot make a full-screen TUI read correctly — that would
//! need a real terminal emulator — and the tool description says so.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::Notify;

/// Per-pane buffer of recent output. Roughly a screenful of scrollback for a
/// wide terminal; enough for an agent to read a command's output, not enough to
/// be worth persisting.
const RING_CAPACITY: usize = 256 * 1024;

pub(crate) fn strip_terminal_output(bytes: &[u8]) -> String {
    let mut stripper = AnsiStripper::default();
    let mut output = Vec::with_capacity(bytes.len());
    stripper.strip(bytes, &mut output);
    String::from_utf8_lossy(&output).into_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedPane {
    pub session_id: String,
    pub title: String,
    /// `"ssh"` or `"local"`.
    pub kind: &'static str,
    pub host_id: Option<String>,
    pub grant_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneKind {
    Ssh { host_id: String },
    Local,
}

impl PaneKind {
    fn label(&self) -> &'static str {
        match self {
            PaneKind::Ssh { .. } => "ssh",
            PaneKind::Local => "local",
        }
    }

    fn host_id(&self) -> Option<String> {
        match self {
            PaneKind::Ssh { host_id } => Some(host_id.clone()),
            PaneKind::Local => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TapRead {
    /// Oldest sequence still buffered.
    pub first_seq: u64,
    /// Cursor to pass as `sinceSeq` on the next read.
    pub next_seq: u64,
    /// Bytes that fell out of the buffer before the caller read them.
    pub dropped: u64,
    pub text: String,
}

/// Slot shared between the session's data callback and the registry.
pub(crate) struct PaneSlot {
    /// Read on every output chunk; everything else is behind it.
    enabled: AtomicBool,
    buffer: Mutex<Option<PaneBuffer>>,
    notify: Notify,
}

impl PaneSlot {
    fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            buffer: Mutex::new(None),
            notify: Notify::new(),
        }
    }

    #[inline]
    fn push(&self, bytes: &[u8]) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        {
            let mut guard = self.buffer.lock().unwrap();
            let Some(buffer) = guard.as_mut() else {
                return;
            };
            buffer.push(bytes);
        }
        self.notify.notify_waiters();
    }

    fn enable(&self) {
        let mut guard = self.buffer.lock().unwrap();
        if guard.is_none() {
            *guard = Some(PaneBuffer::new(RING_CAPACITY));
        }
        drop(guard);
        self.enabled.store(true, Ordering::Relaxed);
    }

    fn disable(&self) {
        self.enabled.store(false, Ordering::Relaxed);
        *self.buffer.lock().unwrap() = None;
        // Wake any long-poll so it re-checks and reports the pane is gone.
        self.notify.notify_waiters();
    }

    fn read(&self, since: Option<u64>, max_bytes: usize) -> Option<TapRead> {
        self.buffer
            .lock()
            .unwrap()
            .as_ref()
            .map(|buffer| buffer.read(since, max_bytes))
    }

    /// Sequence number just past everything buffered so far.
    fn cursor(&self) -> Option<u64> {
        self.buffer
            .lock()
            .unwrap()
            .as_ref()
            .map(|buffer| buffer.next_seq())
    }
}

/// Ring of stripped output plus the escape-sequence state machine feeding it.
struct PaneBuffer {
    text: VecDeque<u8>,
    capacity: usize,
    /// Sequence number of `text.front()`. Counts stripped bytes, so it is the
    /// cursor an agent resumes from.
    first_seq: u64,
    stripper: AnsiStripper,
}

impl PaneBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            text: VecDeque::new(),
            capacity,
            first_seq: 0,
            stripper: AnsiStripper::default(),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        let mut out = Vec::with_capacity(bytes.len());
        self.stripper.strip(bytes, &mut out);
        for byte in out {
            if self.text.len() == self.capacity {
                self.text.pop_front();
                self.first_seq += 1;
            }
            self.text.push_back(byte);
        }
    }

    fn next_seq(&self) -> u64 {
        self.first_seq + self.text.len() as u64
    }

    fn read(&self, since: Option<u64>, max_bytes: usize) -> TapRead {
        let end = self.next_seq();
        // A caller with no cursor wants what is buffered; one with a stale
        // cursor gets clamped forward and told how much it missed; one with a
        // cursor from the future (a pane that was re-shared, say) is clamped
        // back rather than reading past the end.
        let requested = since.unwrap_or(self.first_seq).min(end);
        let start = requested.max(self.first_seq);
        let dropped = start.saturating_sub(requested);

        let offset = (start - self.first_seq) as usize;
        let take = (self.text.len() - offset).min(max_bytes);
        let bytes: Vec<u8> = self.text.iter().skip(offset).take(take).copied().collect();

        TapRead {
            first_seq: self.first_seq,
            next_seq: start + take as u64,
            dropped,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
enum StripState {
    #[default]
    Text,
    /// Saw ESC; the next byte decides what kind of sequence this is.
    Escape,
    /// Inside `ESC [ ... final`, ending on a byte in `@`..=`~`.
    Csi,
    /// Inside a string sequence (OSC/DCS/SOS/PM/APC), ending on BEL or `ESC \`.
    String,
    /// Saw ESC inside a string sequence; `\` terminates it.
    StringEscape,
    /// Saw CR; held back so `\r\n` collapses to a single newline.
    CarriageReturn,
}

/// Strips terminal control sequences, keeping printable text.
///
/// State persists across calls: a sequence split over two reads is normal, and
/// a stripper that reset per chunk would leak its tail into the output.
#[derive(Debug, Default)]
struct AnsiStripper {
    state: StripState,
}

impl AnsiStripper {
    fn strip(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        for &byte in bytes {
            self.step(byte, out);
        }
    }

    fn step(&mut self, byte: u8, out: &mut Vec<u8>) {
        match self.state {
            StripState::CarriageReturn => {
                // A lone CR is a line rewrite (progress bars); treat it as a
                // newline so successive updates stay readable, but do not
                // double up when it was really CRLF.
                out.push(b'\n');
                self.state = StripState::Text;
                if byte != b'\n' {
                    self.step(byte, out);
                }
            }
            StripState::Text => match byte {
                0x1b => self.state = StripState::Escape,
                b'\r' => self.state = StripState::CarriageReturn,
                b'\n' | b'\t' => out.push(byte),
                // Remaining C0 controls and DEL carry no text.
                0x00..=0x1f | 0x7f => {}
                _ => out.push(byte),
            },
            StripState::Escape => {
                self.state = match byte {
                    b'[' => StripState::Csi,
                    // OSC, DCS, SOS, PM, APC all run until a string terminator.
                    b']' | b'P' | b'X' | b'^' | b'_' => StripState::String,
                    // ESC ESC restarts rather than consuming the next byte.
                    0x1b => StripState::Escape,
                    // Two-byte escapes (ESC =, ESC 7, charset selection, ...).
                    _ => StripState::Text,
                };
            }
            StripState::Csi => {
                if (0x40..=0x7e).contains(&byte) {
                    self.state = StripState::Text;
                }
            }
            StripState::String => match byte {
                0x07 => self.state = StripState::Text,
                0x1b => self.state = StripState::StringEscape,
                _ => {}
            },
            StripState::StringEscape => {
                if byte == b'\\' {
                    self.state = StripState::Text;
                } else {
                    // Not a terminator: the ESC may start a fresh sequence.
                    self.state = StripState::Escape;
                    self.step(byte, out);
                }
            }
        }
    }
}

struct PaneEntry {
    slot: Arc<PaneSlot>,
    kind: PaneKind,
    title: String,
    /// `Some` while the pane is shared with that grant.
    grant_id: Option<String>,
}

/// Live panes, and which of them are shared with which grant.
///
/// Shares are in-memory only: a session id means nothing once the session ends,
/// so there is nothing here worth persisting.
#[derive(Default)]
pub(crate) struct TapRegistry {
    panes: Mutex<HashMap<String, PaneEntry>>,
}

impl TapRegistry {
    pub(crate) fn new_tap(self: &Arc<Self>, kind: PaneKind) -> PaneTap {
        PaneTap {
            slot: Arc::new(PaneSlot::new()),
            registry: Arc::clone(self),
            kind,
            session_id: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn share(&self, session_id: &str, grant_id: &str, title: &str) -> bool {
        let mut panes = self.panes.lock().unwrap();
        let Some(entry) = panes.get_mut(session_id) else {
            return false;
        };
        entry.grant_id = Some(grant_id.to_string());
        if !title.is_empty() {
            entry.title = title.to_string();
        }
        entry.slot.enable();
        true
    }

    pub(crate) fn unshare(&self, session_id: &str) -> bool {
        let mut panes = self.panes.lock().unwrap();
        let Some(entry) = panes.get_mut(session_id) else {
            return false;
        };
        let was_shared = entry.grant_id.take().is_some();
        entry.slot.disable();
        was_shared
    }

    /// Drop every share at once — used when the keystore locks and when a grant
    /// is revoked, so access cannot outlive the thing that authorised it.
    pub(crate) fn unshare_all(&self, grant_id: Option<&str>) {
        let mut panes = self.panes.lock().unwrap();
        for entry in panes.values_mut() {
            let matches = match (grant_id, entry.grant_id.as_deref()) {
                (Some(wanted), Some(current)) => wanted == current,
                (None, Some(_)) => true,
                _ => false,
            };
            if matches {
                entry.grant_id = None;
                entry.slot.disable();
            }
        }
    }

    pub(crate) fn shared_panes(&self, grant_id: Option<&str>) -> Vec<SharedPane> {
        let panes = self.panes.lock().unwrap();
        let mut shared: Vec<SharedPane> = panes
            .iter()
            .filter_map(|(session_id, entry)| {
                let entry_grant = entry.grant_id.as_deref()?;
                if grant_id.is_some_and(|wanted| wanted != entry_grant) {
                    return None;
                }
                Some(SharedPane {
                    session_id: session_id.clone(),
                    title: entry.title.clone(),
                    kind: entry.kind.label(),
                    host_id: entry.kind.host_id(),
                    grant_id: entry_grant.to_string(),
                })
            })
            .collect();
        shared.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        shared
    }

    /// The slot for a pane, if it is currently shared with `grant_id`.
    fn shared_slot(&self, session_id: &str, grant_id: &str) -> Option<Arc<PaneSlot>> {
        let panes = self.panes.lock().unwrap();
        let entry = panes.get(session_id)?;
        (entry.grant_id.as_deref() == Some(grant_id)).then(|| Arc::clone(&entry.slot))
    }

    pub(crate) fn is_shared_with(&self, session_id: &str, grant_id: &str) -> bool {
        self.shared_slot(session_id, grant_id).is_some()
    }

    /// Where a reader should resume to see only what happens *after* now.
    ///
    /// Taken before writing to a pane, so the caller reads the output of its
    /// own input rather than replaying what was already on screen.
    pub(crate) fn cursor(&self, session_id: &str, grant_id: &str) -> Option<u64> {
        self.shared_slot(session_id, grant_id)?.cursor()
    }

    pub(crate) fn pane_kind(&self, session_id: &str) -> Option<PaneKind> {
        self.panes
            .lock()
            .unwrap()
            .get(session_id)
            .map(|entry| entry.kind.clone())
    }

    pub(crate) fn title(&self, session_id: &str) -> Option<String> {
        self.panes
            .lock()
            .unwrap()
            .get(session_id)
            .map(|entry| entry.title.clone())
    }

    /// Read buffered output, optionally waiting up to `timeout` for something
    /// new when the caller is already caught up.
    pub(crate) async fn read(
        &self,
        session_id: &str,
        grant_id: &str,
        since: Option<u64>,
        max_bytes: usize,
        timeout: std::time::Duration,
    ) -> Option<TapRead> {
        let slot = self.shared_slot(session_id, grant_id)?;
        let read = slot.read(since, max_bytes)?;
        if !read.text.is_empty() || timeout.is_zero() {
            return Some(read);
        }
        // Register interest before re-reading, so output arriving in between
        // wakes this wait instead of being missed.
        let notified = slot.notify.notified();
        if let Some(fresh) = slot.read(since, max_bytes) {
            if !fresh.text.is_empty() {
                return Some(fresh);
            }
        }
        let _ = tokio::time::timeout(timeout, notified).await;
        // The pane may have been unshared while waiting.
        let slot = self.shared_slot(session_id, grant_id)?;
        slot.read(since, max_bytes)
    }
}

/// Handle held by a session's callbacks.
///
/// Created before spawn (the data callback closes over it) and attached to the
/// session id once spawn returns, mirroring `AgentEventSink`.
#[derive(Clone)]
pub struct PaneTap {
    slot: Arc<PaneSlot>,
    registry: Arc<TapRegistry>,
    kind: PaneKind,
    session_id: Arc<Mutex<Option<String>>>,
}

impl PaneTap {
    #[inline]
    pub(crate) fn push(&self, bytes: &[u8]) {
        self.slot.push(bytes);
    }

    pub(crate) fn attach(&self, session_id: &str, title: &str) {
        *self.session_id.lock().unwrap() = Some(session_id.to_string());
        self.registry.panes.lock().unwrap().insert(
            session_id.to_string(),
            PaneEntry {
                slot: Arc::clone(&self.slot),
                kind: self.kind.clone(),
                title: title.to_string(),
                grant_id: None,
            },
        );
    }

    /// Called from the session's exit callback, which both the SSH and PTY
    /// layers guarantee fires exactly once on every termination path.
    pub(crate) fn detach(&self) {
        let session_id = self.session_id.lock().unwrap().take();
        if let Some(session_id) = session_id {
            self.registry.panes.lock().unwrap().remove(&session_id);
        }
        self.slot.disable();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(chunks: &[&[u8]]) -> String {
        let mut stripper = AnsiStripper::default();
        let mut out = Vec::new();
        for chunk in chunks {
            stripper.strip(chunk, &mut out);
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn strips_colour_and_keeps_text() {
        assert_eq!(strip(&[b"\x1b[31mred\x1b[0m text"]), "red text");
        assert_eq!(
            strip_terminal_output(b"\x1b[31mred\x1b[0m text"),
            "red text"
        );
    }

    #[test]
    fn strips_osc_with_both_terminators() {
        assert_eq!(strip(&[b"\x1b]0;window title\x07after"]), "after");
        assert_eq!(strip(&[b"\x1b]0;window title\x1b\\after"]), "after");
    }

    #[test]
    fn collapses_crlf_and_promotes_lone_cr() {
        assert_eq!(strip(&[b"one\r\ntwo\r\n"]), "one\ntwo\n");
        assert_eq!(strip(&[b"10%\r20%\r30%"]), "10%\n20%\n30%");
    }

    #[test]
    fn keeps_utf8_and_tabs_drops_other_controls() {
        assert_eq!(strip(&[b"caf\xc3\xa9\tok\x07\x08"]), "café\tok");
    }

    #[test]
    fn two_byte_escapes_consume_exactly_one_byte() {
        assert_eq!(strip(&[b"\x1b=keep"]), "keep");
        assert_eq!(strip(&[b"\x1b(Bkeep"]), "Bkeep");
    }

    /// A sequence split across reads is the normal case, not the exception.
    #[test]
    fn handles_every_split_point() {
        let stream: &[u8] = b"\x1b[1;32mbuild\x1b[0m \x1b]0;title\x07done\r\nnext";
        let expected = strip(&[stream]);
        assert_eq!(expected, "build done\nnext");
        for split in 1..stream.len() {
            assert_eq!(
                strip(&[&stream[..split], &stream[split..]]),
                expected,
                "mismatch when split at {split}"
            );
        }
    }

    #[test]
    fn unshared_slot_buffers_nothing() {
        let slot = PaneSlot::new();
        slot.push(b"ignored");
        assert!(slot.read(None, 1024).is_none());
    }

    #[test]
    fn reading_advances_the_cursor() {
        let slot = PaneSlot::new();
        slot.enable();
        slot.push(b"hello ");
        let first = slot.read(None, 1024).unwrap();
        assert_eq!(first.text, "hello ");
        assert_eq!(first.first_seq, 0);
        assert_eq!(first.next_seq, 6);
        assert_eq!(first.dropped, 0);

        slot.push(b"world");
        let second = slot.read(Some(first.next_seq), 1024).unwrap();
        assert_eq!(second.text, "world");
        assert_eq!(second.next_seq, 11);
        assert_eq!(second.dropped, 0);

        // Caught up: no new bytes, cursor unchanged.
        let third = slot.read(Some(second.next_seq), 1024).unwrap();
        assert_eq!(third.text, "");
        assert_eq!(third.next_seq, 11);
    }

    /// The cursor handed back by `pane_send` has to point at the *end* of what
    /// is buffered. Pointing at the start would make the next read replay
    /// everything already on screen as though the agent's input had produced it.
    #[test]
    fn cursor_points_past_existing_output() {
        let registry = Arc::new(TapRegistry::default());
        let tap = registry.new_tap(PaneKind::Local);
        tap.attach("session-1", "bash");
        registry.share("session-1", "grant-a", "bash");

        tap.push(b"output from before\n");
        let cursor = registry.cursor("session-1", "grant-a").unwrap();
        assert_eq!(cursor, 19);

        tap.push(b"after\n");
        let slot = registry.shared_slot("session-1", "grant-a").unwrap();
        let read = slot.read(Some(cursor), 1024).unwrap();
        assert_eq!(read.text, "after\n");
        assert_eq!(read.dropped, 0);

        // Unknown or unshared panes have no cursor to report.
        assert!(registry.cursor("session-1", "grant-b").is_none());
        assert!(registry.cursor("missing", "grant-a").is_none());
    }

    #[test]
    fn max_bytes_splits_a_read_without_losing_data() {
        let slot = PaneSlot::new();
        slot.enable();
        slot.push(b"abcdef");
        let first = slot.read(None, 4).unwrap();
        assert_eq!(first.text, "abcd");
        assert_eq!(first.next_seq, 4);
        let second = slot.read(Some(first.next_seq), 4).unwrap();
        assert_eq!(second.text, "ef");
        assert_eq!(second.next_seq, 6);
    }

    #[test]
    fn overflow_reports_dropped_bytes_rather_than_a_silent_gap() {
        let mut buffer = PaneBuffer::new(4);
        buffer.push(b"abcdefg");
        // Only the tail survives.
        let read = buffer.read(Some(0), 1024);
        assert_eq!(read.text, "defg");
        assert_eq!(read.first_seq, 3);
        assert_eq!(read.dropped, 3);
        assert_eq!(read.next_seq, 7);
    }

    #[test]
    fn a_cursor_from_the_future_is_clamped_to_the_end() {
        let mut buffer = PaneBuffer::new(64);
        buffer.push(b"abc");
        let read = buffer.read(Some(9_999), 1024);
        assert_eq!(read.text, "");
        assert_eq!(read.next_seq, 3);
        assert_eq!(read.dropped, 0);
    }

    #[test]
    fn disable_frees_the_buffer_and_a_reshare_starts_clean() {
        let slot = PaneSlot::new();
        slot.enable();
        slot.push(b"first");
        slot.disable();
        assert!(slot.read(None, 1024).is_none());

        slot.enable();
        let read = slot.read(None, 1024).unwrap();
        assert_eq!(read.text, "");
        assert_eq!(read.first_seq, 0);
    }

    #[test]
    fn registry_scopes_panes_to_the_granting_agent() {
        let registry = Arc::new(TapRegistry::default());
        let tap = registry.new_tap(PaneKind::Ssh {
            host_id: "host-1".into(),
        });
        tap.attach("session-1", "prod");
        tap.push(b"before sharing");

        assert!(registry.shared_panes(None).is_empty());
        assert!(!registry.is_shared_with("session-1", "grant-a"));

        assert!(registry.share("session-1", "grant-a", "prod"));
        assert!(registry.is_shared_with("session-1", "grant-a"));
        // A second grant cannot reach a pane shared with the first.
        assert!(!registry.is_shared_with("session-1", "grant-b"));

        let shared = registry.shared_panes(Some("grant-a"));
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].kind, "ssh");
        assert_eq!(shared[0].host_id.as_deref(), Some("host-1"));
        assert!(registry.shared_panes(Some("grant-b")).is_empty());

        // Output from before the share is not retroactively exposed.
        let slot = registry.shared_slot("session-1", "grant-a").unwrap();
        assert_eq!(slot.read(None, 1024).unwrap().text, "");
    }

    #[test]
    fn detach_removes_the_pane_from_the_registry() {
        let registry = Arc::new(TapRegistry::default());
        let tap = registry.new_tap(PaneKind::Local);
        tap.attach("session-1", "bash");
        registry.share("session-1", "grant-a", "bash");
        assert!(registry.is_shared_with("session-1", "grant-a"));

        tap.detach();
        assert!(!registry.is_shared_with("session-1", "grant-a"));
        assert!(registry.shared_panes(None).is_empty());
        // Late writes from a racing callback are harmless.
        tap.push(b"after exit");
    }

    #[test]
    fn unshare_all_revokes_only_the_named_grant() {
        let registry = Arc::new(TapRegistry::default());
        let first = registry.new_tap(PaneKind::Local);
        first.attach("session-1", "one");
        let second = registry.new_tap(PaneKind::Local);
        second.attach("session-2", "two");
        registry.share("session-1", "grant-a", "one");
        registry.share("session-2", "grant-b", "two");

        registry.unshare_all(Some("grant-a"));
        assert!(!registry.is_shared_with("session-1", "grant-a"));
        assert!(registry.is_shared_with("session-2", "grant-b"));

        registry.unshare_all(None);
        assert!(registry.shared_panes(None).is_empty());
    }
}
