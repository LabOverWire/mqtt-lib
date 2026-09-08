//! File-based storage backend for MQTT broker persistence
//!
//! Provides durable storage using organized file structure with atomic operations.

use super::{
    ClientSession, InflightDirection, InflightMessage, QueueHandle, QueueLimits, QueueOp,
    QueueRegistry, QueueWriter, QueuedMessage, RetainedMessage, StorageBackend, SEQ_FLOOR,
};
use crate::error::{MqttError, Result};
use crate::validation::topic_matches_filter;
use serde_json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info, warn};

/// How long an inflight or queued row may sit unwritten so that its remove can cancel it.
const INFLIGHT_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);
/// Total pending rows (queue + inflight) that force a write-out before the settle window ends.
const INFLIGHT_SETTLE_MAX: usize = 8192;

type InflightKey = (String, u16, InflightDirection);
type QueueKey = (String, u64);

/// Queued-message writes waiting for the settle window, coalesced so a write immediately
/// followed by its delete never touches the disk. Applying an op only mutates this map, so the
/// writer drains its unbounded channel at memory speed and the map stays bounded by the number
/// of distinct live sequence numbers (already capped per client).
#[derive(Default)]
struct PendingQueue {
    pending: HashMap<QueueKey, Option<Arc<QueuedMessage>>>,
    on_disk: HashSet<QueueKey>,
}

impl PendingQueue {
    fn store(&mut self, client_id: String, seq: u64, body: Arc<QueuedMessage>) {
        self.pending.insert((client_id, seq), Some(body));
    }

    fn remove(&mut self, client_id: String, seq: u64) {
        let key = (client_id, seq);
        if matches!(self.pending.get(&key), Some(Some(_))) && !self.on_disk.contains(&key) {
            self.pending.remove(&key);
        } else {
            self.pending.insert(key, None);
        }
    }

    fn len(&self) -> usize {
        self.pending.len()
    }

    async fn write_out(&mut self, queues_dir: &Path) {
        for (key, op) in self.pending.drain() {
            let path = queues_dir.join(&key.0).join(format!("{:020}.json", key.1));
            if let Some(body) = op {
                if let Err(e) = FileBackend::write_atomic(path, &*body, false).await {
                    warn!(
                        client_id = key.0,
                        seq = key.1,
                        "Failed to persist queued message: {e}"
                    );
                } else {
                    self.on_disk.insert(key);
                }
            } else {
                FileBackend::remove_if_present(&path, || {
                    warn!(
                        client_id = key.0,
                        seq = key.1,
                        "Failed to delete queued message file"
                    );
                })
                .await;
                self.on_disk.remove(&key);
            }
        }
    }
}

/// Inflight rows waiting for the settle window: `Some` is a store that has not reached the
/// disk, `None` a remove that must delete whatever an earlier write-out (or boot) left there.
///
/// A remove may cancel a pending store only when nothing for that key has ever reached the
/// disk; once a row is written out (or was found on disk at boot) its remove must always
/// produce a delete, otherwise a stale row survives and is redelivered on restart.
#[derive(Default)]
struct PendingInflight {
    pending: HashMap<InflightKey, Option<Box<InflightMessage>>>,
    on_disk: HashSet<InflightKey>,
}

impl PendingInflight {
    fn store(&mut self, message: Box<InflightMessage>) {
        let key = (
            message.client_id.clone(),
            message.packet_id,
            message.direction,
        );
        self.pending.insert(key, Some(message));
    }

    fn remove(&mut self, client_id: String, packet_id: u16, direction: InflightDirection) {
        let key = (client_id, packet_id, direction);
        if matches!(self.pending.get(&key), Some(Some(_))) && !self.on_disk.contains(&key) {
            self.pending.remove(&key);
        } else {
            self.pending.insert(key, None);
        }
    }

    fn forget_client(&mut self, client_id: &str) {
        self.pending.retain(|key, _| key.0 != client_id);
        self.on_disk.retain(|key| key.0 != client_id);
    }

    async fn write_out(&mut self, inflight_dir: &Path) {
        for (key, op) in self.pending.drain() {
            let path = FileBackend::inflight_path(inflight_dir, &key);
            if let Some(message) = op {
                if let Err(e) = FileBackend::write_atomic(path, &*message, false).await {
                    warn!(
                        client_id = key.0,
                        packet_id = key.1,
                        "Failed to persist inflight message: {e}"
                    );
                } else {
                    self.on_disk.insert(key);
                }
            } else {
                FileBackend::remove_if_present(&path, || {
                    warn!(
                        client_id = key.0,
                        packet_id = key.1,
                        "Failed to delete inflight file"
                    );
                })
                .await;
                self.on_disk.remove(&key);
            }
        }
    }
}

enum QueueFileName {
    Seq(u64),
    Legacy { ts: u64, seq: u64 },
}

fn parse_queue_file_stem(stem: &str) -> Option<QueueFileName> {
    match stem.split_once('_') {
        Some((ts, seq)) => Some(QueueFileName::Legacy {
            ts: ts.parse().ok()?,
            seq: seq.parse().ok()?,
        }),
        None => stem.parse().ok().map(QueueFileName::Seq),
    }
}

/// Disambiguates concurrent writes to the same destination path.
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Storage format version
///
/// IMPORTANT: Only increment this version when the storage format changes:
/// - Modifying `RetainedMessage`, `ClientSession`, or `QueuedMessage` struct fields
/// - Changing file naming scheme (`topic_to_filename`, queue file names)
/// - Changing directory structure (retained/, sessions/, queues/)
///
/// Version History:
/// - 1: Initial version (0.10.0)
const STORAGE_VERSION: &str = "1";

/// File-based storage backend with write-behind session caching
pub struct FileBackend {
    _base_dir: PathBuf,
    retained_dir: PathBuf,
    sessions_dir: PathBuf,
    queues_dir: PathBuf,
    inflight_dir: PathBuf,
    sessions_cache: Arc<RwLock<HashMap<String, ClientSession>>>,
    dirty_sessions: Arc<RwLock<HashSet<String>>>,
    shutdown: Arc<AtomicBool>,
    queues: QueueRegistry,
    queue_writer: QueueWriter,
    queue_flush: mpsc::Sender<oneshot::Sender<()>>,
}

impl FileBackend {
    /// Create new file storage backend
    ///
    /// # Errors
    ///
    /// Returns error if directories cannot be created or version mismatch detected
    pub async fn new(base_dir: impl AsRef<Path>) -> Result<Self> {
        Self::with_queue_limits(base_dir, QueueLimits::default()).await
    }

    /// # Errors
    ///
    /// Returns error if directories cannot be created or version mismatch detected
    pub async fn with_queue_limits(
        base_dir: impl AsRef<Path>,
        limits: QueueLimits,
    ) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        let retained_dir = base_dir.join("retained");
        let sessions_dir = base_dir.join("sessions");
        let queues_dir = base_dir.join("queues");
        let inflight_dir = base_dir.join("inflight");

        Self::check_storage_version(&base_dir).await?;

        for dir in [&retained_dir, &sessions_dir, &queues_dir, &inflight_dir] {
            fs::create_dir_all(dir).await.map_err(|e| {
                MqttError::Configuration(format!("Failed to create dir {}: {e}", dir.display()))
            })?;
        }

        let (writer_tx, writer_rx) = mpsc::unbounded_channel();
        let (flush_tx, flush_rx) = mpsc::channel(4);
        tokio::spawn(Self::run_queue_writer(
            queues_dir.clone(),
            inflight_dir.clone(),
            writer_rx,
            flush_rx,
        ));

        let backend = Self {
            _base_dir: base_dir.clone(),
            retained_dir,
            sessions_dir,
            queues_dir,
            inflight_dir,
            sessions_cache: Arc::new(RwLock::new(HashMap::new())),
            dirty_sessions: Arc::new(RwLock::new(HashSet::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
            queues: QueueRegistry::new(limits, Some(writer_tx.clone())),
            queue_writer: writer_tx,
            queue_flush: flush_tx,
        };
        backend.scan_queues().await?;

        info!(
            "Initialized file storage backend at: {}",
            base_dir.display()
        );

        Ok(backend)
    }

    /// Rebuilds every client's queue index from disk, migrating legacy `{ts}_{seq}` names to
    /// `{seq:020}` so directory order is sequence order, and raises the sequence counter above
    /// everything found.
    async fn scan_queues(&self) -> Result<()> {
        let mut max_seq = 0u64;
        let mut client_dirs = match fs::read_dir(&self.queues_dir).await {
            Ok(dirs) => dirs,
            Err(e) => {
                warn!(
                    "Queues directory {} is unreadable ({e}); starting with empty queues",
                    self.queues_dir.display()
                );
                return Ok(());
            }
        };
        loop {
            let entry = match client_dirs.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(e) => {
                    warn!("Stopped scanning queue directories: {e}");
                    break;
                }
            };
            let client_dir = entry.path();
            if !fs::metadata(&client_dir).await.is_ok_and(|m| m.is_dir()) {
                continue;
            }
            let Some(client_id) = client_dir.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let files = match self.list_files(&client_dir, "json").await {
                Ok(files) => files,
                Err(e) => {
                    warn!(client_id, "Skipping unreadable queue directory: {e}");
                    continue;
                }
            };
            let mut new_format: Vec<(u64, PathBuf)> = Vec::new();
            let mut legacy: Vec<(u64, u64, PathBuf)> = Vec::new();
            for path in files {
                let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                match parse_queue_file_stem(stem) {
                    Some(QueueFileName::Seq(seq)) => new_format.push((seq, path)),
                    Some(QueueFileName::Legacy { ts, seq }) => legacy.push((ts, seq, path)),
                    None => warn!("Ignoring unrecognised queue file {}", path.display()),
                }
            }
            if !legacy.is_empty() {
                let floor = new_format
                    .iter()
                    .map(|(seq, _)| *seq)
                    .min()
                    .unwrap_or(SEQ_FLOOR)
                    .min(SEQ_FLOOR);
                legacy.sort_by_key(|(ts, seq, _)| (*ts, *seq));
                let count = legacy.len() as u64;
                let first = floor.checked_sub(count).unwrap_or(SEQ_FLOOR);
                for (index, (_, _, path)) in legacy.into_iter().enumerate() {
                    let seq = first + index as u64;
                    let target = client_dir.join(format!("{seq:020}.json"));
                    match fs::rename(&path, &target).await {
                        Ok(()) => new_format.push((seq, target)),
                        Err(e) => warn!(
                            "Could not migrate queue file {} to {}: {e}",
                            path.display(),
                            target.display()
                        ),
                    }
                }
                info!(
                    client_id,
                    migrated = count,
                    "Migrated legacy queue file names"
                );
            }
            new_format.sort_by_key(|(seq, _)| *seq);
            let queue = self.queues.handle(client_id);
            for (seq, path) in new_format {
                // Parse each file once so the entry is accounted by payload length (matching
                // push) and carries its expiry in memory. Metadata length would count the
                // pretty-printed JSON (~6-9x the payload) and wrongly evict most of a
                // within-cap backlog on the first push after restart.
                let (bytes, expires_at) = match self.read_file::<QueuedMessage>(path.clone()).await
                {
                    Ok(Some(mut message)) => {
                        message.recompute_expiry();
                        (message.payload.len(), message.expires_at)
                    }
                    _ => {
                        // Unreadable/corrupt: quarantined by read_file; skip the entry.
                        continue;
                    }
                };
                queue.push_scanned(seq, path, bytes, expires_at);
                max_seq = max_seq.max(seq);
            }
        }
        self.queues.seed_seq(max_seq);
        Ok(())
    }

    /// Applies queue writes and deletes as they arrive, and batches inflight rows: a store
    /// followed by its remove within one settle window never touches the disk, which is the
    /// normal life of an at-least-once message, so only rows that stay open long enough are
    /// written.
    async fn run_queue_writer(
        queues_dir: PathBuf,
        inflight_dir: PathBuf,
        mut ops: mpsc::UnboundedReceiver<QueueOp>,
        mut flushes: mpsc::Receiver<oneshot::Sender<()>>,
    ) {
        let mut inflight = PendingInflight::default();
        let mut queue = PendingQueue::default();
        let mut settle = tokio::time::interval(INFLIGHT_SETTLE);
        settle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                op = ops.recv() => {
                    let Some(op) = op else { break };
                    Self::apply_queue_op(&inflight_dir, &mut inflight, &mut queue, op).await;
                    if inflight.pending.len() + queue.len() >= INFLIGHT_SETTLE_MAX {
                        queue.write_out(&queues_dir).await;
                        inflight.write_out(&inflight_dir).await;
                    }
                }
                flush = flushes.recv() => {
                    let Some(done) = flush else { break };
                    while let Ok(op) = ops.try_recv() {
                        Self::apply_queue_op(&inflight_dir, &mut inflight, &mut queue, op).await;
                    }
                    queue.write_out(&queues_dir).await;
                    inflight.write_out(&inflight_dir).await;
                    let _ = done.send(());
                }
                _ = settle.tick() => {
                    queue.write_out(&queues_dir).await;
                    inflight.write_out(&inflight_dir).await;
                }
            }
        }
        queue.write_out(&queues_dir).await;
        inflight.write_out(&inflight_dir).await;
    }

    async fn apply_queue_op(
        inflight_dir: &Path,
        inflight: &mut PendingInflight,
        queue: &mut PendingQueue,
        op: QueueOp,
    ) {
        match op {
            QueueOp::Write {
                client_id,
                seq,
                body,
            } => queue.store(client_id, seq, body),
            QueueOp::Delete { client_id, seq } => queue.remove(client_id, seq),
            QueueOp::StoreInflight(message) => inflight.store(message),
            QueueOp::RemoveInflight {
                client_id,
                packet_id,
                direction,
            } => inflight.remove(client_id, packet_id, direction),
            QueueOp::RemoveAllInflight { client_id } => {
                inflight.forget_client(&client_id);
                let client_dir = inflight_dir.join(&client_id);
                match fs::remove_dir_all(&client_dir).await {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => warn!(client_id, "Failed to remove inflight directory: {e}"),
                }
            }
        }
    }

    /// Deletes `path`, ignoring a missing file and reporting any other error via `on_error`.
    async fn remove_if_present(path: &Path, on_error: impl FnOnce()) {
        match fs::remove_file(path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => on_error(),
        }
    }

    fn inflight_path(inflight_dir: &Path, key: &InflightKey) -> PathBuf {
        let direction_tag = match key.2 {
            InflightDirection::Inbound => "inbound",
            InflightDirection::Outbound => "outbound",
        };
        inflight_dir
            .join(&key.0)
            .join(format!("{direction_tag}_{}.json", key.1))
    }

    /// # Errors
    /// Returns an error if any session fails to persist.
    pub async fn flush_sessions(&self) -> Result<()> {
        let to_flush: Vec<String> = self.dirty_sessions.read().await.iter().cloned().collect();

        if to_flush.is_empty() {
            return Ok(());
        }

        let cache = self.sessions_cache.read().await;
        let mut failed = Vec::new();

        for client_id in to_flush {
            if let Some(session) = cache.get(&client_id) {
                let filename = format!("{client_id}.json");
                let path = self.sessions_dir.join(filename);
                if let Err(e) = self.write_file_atomic(path, session).await {
                    warn!("failed to persist session {}: {}", client_id, e);
                    failed.push(client_id);
                } else {
                    self.dirty_sessions.write().await.remove(&client_id);
                }
            } else {
                self.dirty_sessions.write().await.remove(&client_id);
            }
        }

        if failed.is_empty() {
            Ok(())
        } else {
            Err(MqttError::Io(format!(
                "failed to persist {} sessions",
                failed.len()
            )))
        }
    }

    pub fn start_flush_task(self: &Arc<Self>, flush_interval: std::time::Duration) {
        let backend = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(flush_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                if backend.shutdown.load(Ordering::Relaxed) {
                    if let Err(e) = backend.flush_sessions().await {
                        warn!("failed to flush sessions on shutdown: {e}");
                    }
                    break;
                }

                if let Err(e) = backend.flush_sessions().await {
                    warn!("failed to flush sessions: {e}");
                }
            }
        });
    }

    /// # Errors
    /// Returns an error if flushing sessions fails.
    pub async fn shutdown(&self) -> Result<()> {
        self.shutdown.store(true, Ordering::Relaxed);
        self.flush_queue_writes().await;
        self.flush_sessions().await
    }

    fn send_queue_op(&self, op: QueueOp) -> Result<()> {
        self.queue_writer
            .send(op)
            .map_err(|_| MqttError::Io("storage writer task is no longer running".to_string()))
    }

    /// Waits until every queued-message write and delete issued so far has reached disk.
    pub async fn flush_queue_writes(&self) {
        let (done_tx, done_rx) = oneshot::channel();
        if self.queue_flush.send(done_tx).await.is_ok() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), done_rx).await;
        }
    }

    async fn check_storage_version(base_dir: &Path) -> Result<()> {
        let version_file = base_dir.join(".storage_version");

        if version_file.exists() {
            let stored_version = fs::read_to_string(&version_file).await.map_err(|e| {
                MqttError::Configuration(format!("Failed to read storage version: {e}"))
            })?;

            let stored_version = stored_version.trim();

            if stored_version != STORAGE_VERSION {
                return Err(MqttError::Configuration(format!(
                    "Storage version mismatch: found version {}, expected version {}.\n\
                     \n\
                     The storage format has changed and is incompatible.\n\
                     \n\
                     To resolve this issue:\n\
                     1. Backup your data: mqttv5 storage backup --dir {} --output backup.json\n\
                     2. Remove the storage directory: rm -rf {}\n\
                     3. Restart the broker (it will create a new storage with version {})\n\
                     \n\
                     Note: Without backup, all retained messages and session data will be lost.",
                    stored_version,
                    STORAGE_VERSION,
                    base_dir.display(),
                    base_dir.display(),
                    STORAGE_VERSION
                )));
            }

            debug!("Storage version verified: {}", STORAGE_VERSION);
        } else {
            fs::create_dir_all(base_dir).await.map_err(|e| {
                MqttError::Configuration(format!("Failed to create storage dir: {e}"))
            })?;

            fs::write(&version_file, STORAGE_VERSION)
                .await
                .map_err(|e| {
                    MqttError::Configuration(format!("Failed to write storage version: {e}"))
                })?;

            info!("Created new storage with version {}", STORAGE_VERSION);
        }

        Ok(())
    }

    fn topic_to_filename(topic: &str) -> String {
        let mut result = String::with_capacity(topic.len());
        for ch in topic.chars() {
            match ch {
                '%' => result.push_str("%25"),
                '/' => result.push_str("%2F"),
                '+' => result.push_str("%2B"),
                '#' => result.push_str("%23"),
                '$' => result.push_str("%24"),
                '\\' => result.push_str("%5C"),
                ':' => result.push_str("%3A"),
                '*' => result.push_str("%2A"),
                '?' => result.push_str("%3F"),
                '"' => result.push_str("%22"),
                '<' => result.push_str("%3C"),
                '>' => result.push_str("%3E"),
                '|' => result.push_str("%7C"),
                '\0' => result.push_str("%00"),
                _ => result.push(ch),
            }
        }
        result
    }

    fn filename_to_topic(filename: &str) -> String {
        let mut result = String::with_capacity(filename.len());
        let mut chars = filename.chars();
        while let Some(ch) = chars.next() {
            if ch == '%' {
                let hex: String = chars.by_ref().take(2).collect();
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(char::from(byte));
                } else {
                    result.push('%');
                    result.push_str(&hex);
                }
            } else {
                result.push(ch);
            }
        }
        result
    }

    /// Serialises `data` and replaces `path` with it atomically and durably.
    ///
    /// The temp file is named uniquely per write. A name derived only from the destination
    /// would be shared by every concurrent writer of that path, letting one writer's
    /// `create` truncate another's in-flight file; the victim would then sync and rename
    /// zero or partial bytes into place, and a crash before its retry would make that
    /// permanent. The temp file is removed if the write or rename fails.
    async fn write_file_atomic<T: serde::Serialize>(&self, path: PathBuf, data: &T) -> Result<()> {
        Self::write_atomic(path, data, true).await
    }

    /// Serializes and atomically installs `data` at `path`; `durable` adds an fsync before the
    /// rename, which queue files skip because their loss is tolerated and their rate is high.
    async fn write_atomic<T: serde::Serialize>(
        path: PathBuf,
        data: &T,
        durable: bool,
    ) -> Result<()> {
        let serialized = serde_json::to_vec_pretty(data)
            .map_err(|e| MqttError::Configuration(format!("Failed to serialize data: {e}")))?;

        for attempt in 0..2u8 {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).await.map_err(|e| {
                    MqttError::Io(format!("Failed to create parent directory: {e}"))
                })?;
            }

            let temp_path = path.with_extension(format!(
                "tmp.{}.{}",
                std::process::id(),
                TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));

            match Self::write_temp_file(&temp_path, &serialized, durable).await {
                Ok(()) => {}
                Err(e) => {
                    let _ = fs::remove_file(&temp_path).await;
                    return Err(e);
                }
            }

            match fs::rename(&temp_path, &path).await {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && attempt == 0 => {
                    let _ = fs::remove_file(&temp_path).await;
                    debug!("Atomic write race detected, retrying: {e}");
                }
                Err(e) => {
                    let _ = fs::remove_file(&temp_path).await;
                    return Err(MqttError::Io(format!("Failed to rename temp file: {e}")));
                }
            }
        }

        Ok(())
    }

    /// Writes the payload to `temp_path` and makes it durable before it is renamed into place.
    async fn write_temp_file(temp_path: &Path, serialized: &[u8], durable: bool) -> Result<()> {
        let mut file = File::create(temp_path)
            .await
            .map_err(|e| MqttError::Io(format!("Failed to create temp file: {e}")))?;

        file.write_all(serialized)
            .await
            .map_err(|e| MqttError::Io(format!("Failed to write temp file: {e}")))?;

        file.flush()
            .await
            .map_err(|e| MqttError::Io(format!("Failed to flush temp file: {e}")))?;

        if durable {
            file.sync_data()
                .await
                .map_err(|e| MqttError::Io(format!("Failed to sync temp file: {e}")))?;
        }

        Ok(())
    }

    /// Reads and deserializes a stored item, reporting an unreadable one as absent.
    ///
    /// Persisted state is untrusted input: it can be truncated by a crash, a full disk, or a
    /// partial write, and it may have been written by an older schema. A single unreadable
    /// item must therefore never fail the surrounding load — that would let data the broker
    /// wrote itself prevent the broker from starting, an outage recoverable only by manually
    /// deleting files. The offending file is quarantined and treated as missing; callers
    /// already skip `None`. Genuine I/O errors still propagate.
    async fn read_file<T: serde::de::DeserializeOwned>(&self, path: PathBuf) -> Result<Option<T>> {
        match fs::read(&path).await {
            Ok(data) => match serde_json::from_slice(&data) {
                Ok(value) => Ok(Some(value)),
                Err(e) => {
                    self.quarantine_unreadable(&path, &e.to_string()).await;
                    Ok(None)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(MqttError::Io(format!(
                "Failed to read {}: {}",
                path.display(),
                e
            ))),
        }
    }

    /// Moves an unreadable file aside, keeping it for diagnosis instead of deleting it.
    ///
    /// The `.corrupt` extension also takes it out of the directory listings, which filter on
    /// the storage extension, so a bad file is reported once rather than on every load.
    async fn quarantine_unreadable(&self, path: &Path, reason: &str) {
        let quarantined = path.with_extension("corrupt");
        if let Err(e) = fs::rename(path, &quarantined).await {
            warn!(
                "Ignoring unreadable storage file {} ({reason}); could not quarantine it: {e}",
                path.display()
            );
            return;
        }
        warn!(
            "Quarantined unreadable storage file {} as {} and skipped it: {reason}",
            path.display(),
            quarantined.display()
        );
    }

    /// List all files in directory with extension
    async fn list_files(&self, dir: &Path, extension: &str) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        let mut entries = fs::read_dir(dir).await.map_err(|e| {
            MqttError::Io(format!("Failed to read directory {}: {}", dir.display(), e))
        })?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| MqttError::Io(format!("Failed to read directory entry: {e}")))?
        {
            let path = entry.path();
            let is_file = fs::metadata(&path).await.is_ok_and(|m| m.is_file());
            if is_file && path.extension().is_some_and(|ext| ext == extension) {
                files.push(path);
            }
        }

        Ok(files)
    }

    async fn cleanup_expired_inflight(&self) -> Result<usize> {
        let mut removed = 0;
        if self.inflight_dir.exists() {
            if let Ok(mut inflight_entries) = fs::read_dir(&self.inflight_dir).await {
                while let Ok(Some(entry)) = inflight_entries.next_entry().await {
                    let client_dir = entry.path();
                    let is_dir = fs::metadata(&client_dir).await.is_ok_and(|m| m.is_dir());
                    if is_dir {
                        let files = self.list_files(&client_dir, "json").await?;
                        for file_path in files {
                            if let Some(msg) =
                                self.read_file::<InflightMessage>(file_path.clone()).await?
                            {
                                if msg.is_expired() {
                                    if let Err(e) = fs::remove_file(&file_path).await {
                                        warn!("failed to remove expired inflight: {e}");
                                    } else {
                                        removed += 1;
                                    }
                                }
                            }
                        }

                        if let Ok(mut dir) = fs::read_dir(&client_dir).await {
                            if dir.next_entry().await.ok().flatten().is_none() {
                                let _ = fs::remove_dir(&client_dir).await;
                            }
                        }
                    }
                }
            }
        }
        Ok(removed)
    }
}

impl StorageBackend for FileBackend {
    async fn store_retained_message(&self, topic: &str, message: RetainedMessage) -> Result<()> {
        let filename = format!("{}.json", Self::topic_to_filename(topic));
        let path = self.retained_dir.join(filename);

        debug!("Storing retained message for topic: {}", topic);
        self.write_file_atomic(path, &message).await?;

        Ok(())
    }

    async fn get_retained_message(&self, topic: &str) -> Result<Option<RetainedMessage>> {
        let filename = format!("{}.json", Self::topic_to_filename(topic));
        let path = self.retained_dir.join(filename);

        let message: Option<RetainedMessage> = self.read_file(path).await?;

        // Check if message has expired
        if let Some(ref msg) = message {
            if msg.is_expired() {
                self.remove_retained_message(topic).await?;
                return Ok(None);
            }
        }

        Ok(message)
    }

    async fn remove_retained_message(&self, topic: &str) -> Result<()> {
        let filename = format!("{}.json", Self::topic_to_filename(topic));
        let path = self.retained_dir.join(filename);

        if path.exists() {
            fs::remove_file(&path).await.map_err(|e| {
                MqttError::Io(format!("Failed to remove retained message file: {e}"))
            })?;
            debug!("Removed retained message for topic: {}", topic);
        }

        Ok(())
    }

    async fn get_retained_messages(
        &self,
        topic_filter: &str,
    ) -> Result<Vec<(String, RetainedMessage)>> {
        let files = self.list_files(&self.retained_dir, "json").await?;
        let mut messages = Vec::new();

        for file_path in files {
            if let Some(filename) = file_path.file_stem().and_then(|s| s.to_str()) {
                let topic = Self::filename_to_topic(filename);

                if topic_matches_filter(&topic, topic_filter) {
                    if let Some(message) = self.read_file::<RetainedMessage>(file_path).await? {
                        if !message.is_expired() {
                            messages.push((topic, message));
                        }
                    }
                }
            }
        }

        Ok(messages)
    }

    async fn store_session(&self, session: ClientSession) -> Result<()> {
        let client_id = session.client_id.clone();
        self.sessions_cache
            .write()
            .await
            .insert(client_id.clone(), session);
        self.dirty_sessions.write().await.insert(client_id);
        Ok(())
    }

    async fn get_session(&self, client_id: &str) -> Result<Option<ClientSession>> {
        let cached = self.sessions_cache.read().await.get(client_id).cloned();
        if let Some(session) = cached {
            if session.is_expired() {
                self.remove_session(client_id).await?;
                return Ok(None);
            }
            return Ok(Some(session));
        }

        let filename = format!("{client_id}.json");
        let path = self.sessions_dir.join(filename);
        let session: Option<ClientSession> = self.read_file(path).await?;

        if let Some(ref sess) = session {
            if sess.is_expired() {
                self.remove_session(client_id).await?;
                return Ok(None);
            }
            self.sessions_cache
                .write()
                .await
                .insert(client_id.to_string(), sess.clone());
        }

        Ok(session)
    }

    async fn remove_session(&self, client_id: &str) -> Result<()> {
        self.sessions_cache.write().await.remove(client_id);
        self.dirty_sessions.write().await.remove(client_id);

        let filename = format!("{client_id}.json");
        let path = self.sessions_dir.join(filename);
        if path.exists() {
            fs::remove_file(&path)
                .await
                .map_err(|e| MqttError::Io(format!("Failed to remove session file: {e}")))?;
            debug!("Removed session for client: {}", client_id);
        }

        Ok(())
    }

    fn queue_handle(&self, client_id: &str) -> QueueHandle {
        self.queues.handle(client_id)
    }

    fn queue_message(
        &self,
        message: QueuedMessage,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let client_id = message.client_id.clone();
        self.queues.handle(&client_id).push(message);
        debug!("Queued message for client: {}", client_id);
        std::future::ready(Ok(()))
    }

    async fn get_queued_messages(&self, client_id: &str) -> Result<Vec<QueuedMessage>> {
        Ok(self.queues.handle(client_id).peek_all().await)
    }

    fn remove_queued_messages(
        &self,
        client_id: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.queues.handle(client_id).clear(None);
        debug!("Removed all queued messages for client: {}", client_id);
        std::future::ready(Ok(()))
    }

    fn store_inflight_message(
        &self,
        message: InflightMessage,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(self.send_queue_op(QueueOp::StoreInflight(Box::new(message))))
    }

    async fn get_inflight_messages(&self, client_id: &str) -> Result<Vec<InflightMessage>> {
        self.flush_queue_writes().await;
        let client_dir = self.inflight_dir.join(client_id);
        if !client_dir.exists() {
            return Ok(Vec::new());
        }

        let files = self.list_files(&client_dir, "json").await?;
        let mut messages = Vec::new();

        for file_path in files {
            if let Some(msg) = self.read_file::<InflightMessage>(file_path.clone()).await? {
                if !msg.is_expired() {
                    messages.push(msg);
                } else if let Err(e) = fs::remove_file(&file_path).await {
                    warn!("failed to remove expired inflight file: {e}");
                }
            }
        }

        Ok(messages)
    }

    fn remove_inflight_message(
        &self,
        client_id: &str,
        packet_id: u16,
        direction: InflightDirection,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(self.send_queue_op(QueueOp::RemoveInflight {
            client_id: client_id.to_string(),
            packet_id,
            direction,
        }))
    }

    fn remove_all_inflight_messages(
        &self,
        client_id: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(self.send_queue_op(QueueOp::RemoveAllInflight {
            client_id: client_id.to_string(),
        }))
    }

    async fn cleanup_expired(&self) -> Result<()> {
        let mut removed_count = 0;

        // Clean expired retained messages
        let retained_files = self.list_files(&self.retained_dir, "json").await?;
        for file_path in retained_files {
            if let Some(message) = self.read_file::<RetainedMessage>(file_path.clone()).await? {
                if message.is_expired() {
                    if let Err(e) = fs::remove_file(&file_path).await {
                        warn!("Failed to remove expired retained message: {e}");
                    } else {
                        removed_count += 1;
                    }
                }
            }
        }

        // Clean expired sessions
        let session_files = self.list_files(&self.sessions_dir, "json").await?;
        for file_path in session_files {
            if let Some(session) = self.read_file::<ClientSession>(file_path.clone()).await? {
                if session.is_expired() {
                    if let Err(e) = fs::remove_file(&file_path).await {
                        warn!("Failed to remove expired session: {e}");
                    } else {
                        removed_count += 1;
                    }
                }
            }
        }

        for queue in self.queues.handles() {
            // Scanned entries carry their expiry in memory (recorded during scan_queues), so
            // purge_expired covers them too; no per-tick re-read of every queued file.
            removed_count += queue.purge_expired();
        }
        self.queues.evict_idle();

        removed_count += self.cleanup_expired_inflight().await?;

        if removed_count > 0 {
            info!("Cleaned up {} expired storage entries", removed_count);
        }

        Ok(())
    }

    async fn flush_sessions(&self) -> Result<()> {
        FileBackend::flush_sessions(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::storage::RetainedMessage;
    use crate::packet::publish::PublishPacket;
    use crate::QoS;

    fn retained(topic: &str, payload: Vec<u8>) -> RetainedMessage {
        let mut packet = PublishPacket::new(topic.to_string(), payload, QoS::AtMostOnce);
        packet.retain = true;
        RetainedMessage::new(packet)
    }

    fn queued(client: &str, tag: &str) -> QueuedMessage {
        QueuedMessage::new(
            PublishPacket::new(
                format!("q/{tag}"),
                tag.as_bytes().to_vec(),
                QoS::AtLeastOnce,
            ),
            client.to_string(),
            QoS::AtLeastOnce,
            None,
        )
    }

    fn topics(messages: &[QueuedMessage]) -> Vec<&str> {
        messages.iter().map(|m| m.topic.as_str()).collect()
    }

    #[tokio::test]
    async fn take_before_the_writer_persists_still_returns_the_message() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path()).await.unwrap();
        let queue = backend.queue_handle("fast");
        queue.push(queued("fast", "m1"));
        let taken = queue.take(1).await;
        assert_eq!(topics(&taken), ["q/m1"]);
        assert_eq!(queue.count(), 0);
        backend.flush_queue_writes().await;
        let client_dir = backend.queues_dir.join("fast");
        let files = if client_dir.exists() {
            backend.list_files(&client_dir, "json").await.unwrap()
        } else {
            Vec::new()
        };
        assert!(
            files.is_empty(),
            "a taken message must leave no file behind"
        );
    }

    #[tokio::test]
    async fn restart_rebuilds_count_and_order_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        {
            let backend = FileBackend::new(dir.path()).await.unwrap();
            let queue = backend.queue_handle("persist");
            for tag in ["a", "b", "c", "d", "e"] {
                queue.push(queued("persist", tag));
            }
            let taken = queue.take(2).await;
            assert_eq!(topics(&taken), ["q/a", "q/b"]);
            queue.requeue_front(vec![queued("persist", "r1"), queued("persist", "r2")]);
            backend.flush_queue_writes().await;
        }
        let reopened = FileBackend::new(dir.path()).await.unwrap();
        let queue = reopened.queue_handle("persist");
        assert_eq!(queue.count(), 5);
        let taken = queue.take(10).await;
        assert_eq!(topics(&taken), ["q/r1", "q/r2", "q/c", "q/d", "q/e"]);
        queue.push(queued("persist", "after"));
        assert!(queue.next_seq() > SEQ_FLOOR);
    }

    #[tokio::test]
    async fn restart_accounts_scanned_bytes_by_payload_not_file_size() {
        // Payloads total 1000 bytes, well under the 4000-byte cap, but each pretty-JSON file
        // is several times its payload. If the scan counted file size the whole backlog would
        // be evicted on the first push after restart.
        let limits = QueueLimits {
            max_messages: 1000,
            max_bytes: 4000,
        };
        let dir = tempfile::tempdir().unwrap();
        {
            let backend = FileBackend::with_queue_limits(dir.path(), limits)
                .await
                .unwrap();
            let queue = backend.queue_handle("c");
            for tag in ["a", "b", "c", "d", "e"] {
                queue.push(QueuedMessage::new(
                    PublishPacket::new(format!("q/{tag}"), vec![b'x'; 200], QoS::AtLeastOnce),
                    "c".to_string(),
                    QoS::AtLeastOnce,
                    None,
                ));
            }
            assert_eq!(queue.count(), 5);
            backend.flush_queue_writes().await;
        }
        let reopened = FileBackend::with_queue_limits(dir.path(), limits)
            .await
            .unwrap();
        let queue = reopened.queue_handle("c");
        assert_eq!(
            queue.count(),
            5,
            "restart must not evict a within-cap backlog"
        );
        queue.push(QueuedMessage::new(
            PublishPacket::new("q/f", vec![b'x'; 200], QoS::AtLeastOnce),
            "c".to_string(),
            QoS::AtLeastOnce,
            None,
        ));
        assert_eq!(queue.count(), 6, "one more within-cap push evicts nothing");
    }

    #[tokio::test]
    async fn legacy_queue_files_are_migrated_and_ordered_first() {
        let dir = tempfile::tempdir().unwrap();
        let client_dir = dir.path().join("queues").join("legacy");
        tokio::fs::create_dir_all(&client_dir).await.unwrap();
        for (index, tag) in ["old0", "old1", "old2"].iter().enumerate() {
            let message = queued("legacy", tag);
            let path = client_dir.join(format!("1700000000000_{index}.json"));
            tokio::fs::write(&path, serde_json::to_vec(&message).unwrap())
                .await
                .unwrap();
        }
        let backend = FileBackend::new(dir.path()).await.unwrap();
        let queue = backend.queue_handle("legacy");
        assert_eq!(queue.count(), 3);
        queue.push(queued("legacy", "new"));
        let taken = queue.take(10).await;
        assert_eq!(topics(&taken), ["q/old0", "q/old1", "q/old2", "q/new"]);
        let remaining = backend.list_files(&client_dir, "json").await.unwrap();
        assert!(
            remaining
                .iter()
                .all(|p| !p.file_name().unwrap().to_str().unwrap().contains('_')),
            "legacy names must be gone after migration"
        );
    }

    /// A file the broker cannot read must not stop it from loading the rest.
    ///
    /// This is the failure that took a broker down: an empty retained file made
    /// `get_retained_messages` fail, which failed `router.initialize()`, which made `run()`
    /// return before any listener was bound.
    #[tokio::test]
    async fn unreadable_retained_file_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path()).await.unwrap();

        backend
            .store_retained_message("good/topic", retained("good/topic", b"keep me".to_vec()))
            .await
            .unwrap();

        let corrupt = dir.path().join("retained").join(format!(
            "{}.json",
            FileBackend::topic_to_filename("bad/topic")
        ));
        fs::write(&corrupt, b"").await.unwrap();

        let messages = backend
            .get_retained_messages("#")
            .await
            .expect("an unreadable file must not fail the load");

        assert_eq!(messages.len(), 1, "the readable message must still load");
        assert_eq!(messages[0].0, "good/topic");
        assert!(!corrupt.exists(), "the bad file must be moved aside");
        assert!(
            corrupt.with_extension("corrupt").exists(),
            "the bad file must be quarantined for diagnosis, not deleted"
        );
    }

    /// Truncated (rather than empty) JSON must be tolerated the same way.
    #[tokio::test]
    async fn partially_written_retained_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path()).await.unwrap();

        let corrupt = dir.path().join("retained").join(format!(
            "{}.json",
            FileBackend::topic_to_filename("bad/topic")
        ));
        fs::write(&corrupt, b"{\"payload\":[1,2").await.unwrap();

        let messages = backend.get_retained_messages("#").await.unwrap();
        assert!(messages.is_empty());
        assert!(corrupt.with_extension("corrupt").exists());
    }

    /// Concurrent writers to one topic must not truncate each other's temp file.
    ///
    /// A temp name derived only from the destination is shared by every writer of that path,
    /// so one writer's `create` truncates another's in-flight bytes and the victim renames a
    /// zero-length file into place. That is how the zero-byte file was produced.
    #[tokio::test]
    async fn concurrent_writes_to_one_topic_never_leave_it_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(FileBackend::new(dir.path()).await.unwrap());

        for _ in 0..20 {
            let writers: Vec<_> = (0..8)
                .map(|i| {
                    let backend = Arc::clone(&backend);
                    tokio::spawn(async move {
                        backend
                            .store_retained_message("hot/topic", retained("hot/topic", vec![i; 64]))
                            .await
                    })
                })
                .collect();
            for w in writers {
                w.await.unwrap().unwrap();
            }

            let messages = backend.get_retained_messages("hot/topic").await.unwrap();
            assert_eq!(
                messages.len(),
                1,
                "a concurrently written retained message must always be readable"
            );
        }
    }

    /// A failed write must not leave its temp file behind now that temp names are unique.
    #[tokio::test]
    async fn successful_write_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path()).await.unwrap();
        backend
            .store_retained_message("some/topic", retained("some/topic", b"payload".to_vec()))
            .await
            .unwrap();

        let mut entries = fs::read_dir(dir.path().join("retained")).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(!name.contains(".tmp"), "temp file left behind: {name}");
        }
    }
}
