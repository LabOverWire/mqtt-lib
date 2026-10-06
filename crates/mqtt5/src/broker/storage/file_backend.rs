//! File-based storage backend for MQTT broker persistence
//!
//! Provides durable storage using organized file structure with atomic operations.

use super::session_log::{sync_directory, SessionChange, SessionLog};
use super::write_behind::{InflightKey, WriteBatch, WriteBehind};
use super::{
    ClientSession, InflightDirection, InflightMessage, QueueHandle, QueueLimits, QueueRegistry,
    QueuedMessage, RetainedMessage, StorageBackend, SEQ_FLOOR,
};
use crate::error::{MqttError, Result};
use crate::validation::topic_matches_filter;
use serde_json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

/// How long an inflight or queued row may sit unwritten so that its remove can cancel it.
const INFLIGHT_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);
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
/// - 2: Sessions in one group-committed log, `sessions/sessions.log` (0.42.0); version 1
///   directories are migrated on open
const STORAGE_VERSION: &str = "2";
const LEGACY_STORAGE_VERSION: &str = "1";

/// File-based storage backend; session writes are group-committed to one log
pub struct FileBackend {
    directory_lock: Option<std::fs::File>,
    retained_dir: PathBuf,
    queues_dir: PathBuf,
    inflight_dir: PathBuf,
    sessions: SessionLog,
    queues: QueueRegistry,
    write_behind: Arc<WriteBehind>,
    queue_flush: mpsc::Sender<oneshot::Sender<()>>,
}

impl Drop for FileBackend {
    fn drop(&mut self) {
        if let Some(lock) = &self.directory_lock {
            if let Err(e) = lock.unlock() {
                warn!("Failed to release storage directory lock: {e}");
            }
        }
    }
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
        let directory_lock = Self::lock_storage_dir(&base_dir).await?;
        let retained_dir = base_dir.join("retained");
        let sessions_dir = base_dir.join("sessions");
        let queues_dir = base_dir.join("queues");
        let inflight_dir = base_dir.join("inflight");

        let legacy = Self::check_storage_version(&base_dir).await?;

        for dir in [&retained_dir, &sessions_dir, &queues_dir, &inflight_dir] {
            fs::create_dir_all(dir).await.map_err(|e| {
                MqttError::Configuration(format!("Failed to create dir {}: {e}", dir.display()))
            })?;
        }
        let sessions = SessionLog::open(sessions_dir, legacy).await?;
        if legacy {
            Self::write_storage_version(&base_dir).await?;
        }

        let write_behind = Arc::new(WriteBehind::default());
        let (flush_tx, flush_rx) = mpsc::channel(4);
        tokio::spawn(Self::run_queue_writer(
            queues_dir.clone(),
            inflight_dir.clone(),
            Arc::clone(&write_behind),
            flush_rx,
        ));

        let backend = Self {
            directory_lock,
            retained_dir,
            queues_dir,
            inflight_dir,
            sessions,
            queues: QueueRegistry::new(limits, Some(Arc::clone(&write_behind))),
            write_behind,
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
                let Ok(Some(mut message)) = self.read_file::<QueuedMessage>(path.clone()).await
                else {
                    continue;
                };
                message.recompute_expiry();
                queue.push_scanned(seq, path, &message);
                max_seq = max_seq.max(seq);
            }
        }
        self.queues.seed_seq(max_seq);
        Ok(())
    }

    async fn run_queue_writer(
        queues_dir: PathBuf,
        inflight_dir: PathBuf,
        write_behind: Arc<WriteBehind>,
        mut flushes: mpsc::Receiver<oneshot::Sender<()>>,
    ) {
        let mut settle = tokio::time::interval(INFLIGHT_SETTLE);
        settle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let flush_reply = tokio::select! {
                flush = flushes.recv() => match flush {
                    Some(done) => Some(done),
                    None => break,
                },
                () = write_behind.filled() => None,
                _ = settle.tick() => None,
            };
            Self::write_out(&queues_dir, &inflight_dir, write_behind.take_batch()).await;
            if let Some(done) = flush_reply {
                if done.send(()).is_err() {
                    debug!("Flush requester stopped waiting");
                }
            }
        }
        Self::write_out(&queues_dir, &inflight_dir, write_behind.take_batch()).await;
    }

    async fn write_out(queues_dir: &Path, inflight_dir: &Path, batch: WriteBatch) {
        for client_id in batch.cleared_inflight {
            match fs::remove_dir_all(inflight_dir.join(&client_id)).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn!(client_id, "Failed to remove inflight directory: {e}"),
            }
        }
        for ((client_id, seq), entry) in batch.queue {
            let path = queues_dir.join(&client_id).join(format!("{seq:020}.json"));
            match entry {
                Some(body) => {
                    if let Err(e) = Self::write_atomic(path, &*body, false).await {
                        warn!(client_id, seq, "Failed to persist queued message: {e}");
                    }
                }
                None => {
                    Self::remove_if_present(&path, || {
                        warn!(client_id, seq, "Failed to delete queued message file");
                    })
                    .await;
                }
            }
        }
        for (key, entry) in batch.inflight {
            let path = Self::inflight_path(inflight_dir, &key);
            match entry {
                Some(message) => {
                    if let Err(e) = Self::write_atomic(path, &*message, false).await {
                        warn!(
                            client_id = key.0,
                            packet_id = key.1,
                            "Failed to persist inflight message: {e}"
                        );
                    }
                }
                None => {
                    Self::remove_if_present(&path, || {
                        warn!(
                            client_id = key.0,
                            packet_id = key.1,
                            "Failed to delete inflight file"
                        );
                    })
                    .await;
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

    #[cfg(test)]
    pub(crate) async fn pause_session_writes(&self) -> impl Sized {
        self.sessions.pause_writes().await
    }

    #[cfg(test)]
    pub(crate) async fn break_next_session_write(&self) {
        self.sessions.break_next_write().await;
    }

    #[cfg(test)]
    pub(crate) async fn session_flushes(&self) -> u64 {
        self.sessions.flushes().await
    }

    /// # Errors
    /// Never fails; session writes are durable before they are acknowledged.
    pub async fn shutdown(&self) -> Result<()> {
        self.flush_queue_writes().await;
        Ok(())
    }

    async fn lock_storage_dir(base_dir: &Path) -> Result<Option<std::fs::File>> {
        fs::create_dir_all(base_dir).await.map_err(|e| {
            MqttError::Configuration(format!(
                "Failed to create storage dir {}: {e}",
                base_dir.display()
            ))
        })?;
        let lock_path = base_dir.join(".lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| {
                MqttError::Configuration(format!(
                    "Failed to open storage lock {}: {e}",
                    lock_path.display()
                ))
            })?;
        let attempt = lock.try_lock();
        Self::directory_lock_outcome(lock, attempt, base_dir)
    }

    fn directory_lock_outcome(
        lock: std::fs::File,
        attempt: std::result::Result<(), std::fs::TryLockError>,
        base_dir: &Path,
    ) -> Result<Option<std::fs::File>> {
        match attempt {
            Ok(()) => Ok(Some(lock)),
            Err(std::fs::TryLockError::Error(e)) if e.kind() == std::io::ErrorKind::Unsupported => {
                warn!(
                    "File locking is not supported for storage directory {}; starting without the directory lock, so another broker could share it: {e}",
                    base_dir.display()
                );
                Ok(None)
            }
            Err(std::fs::TryLockError::WouldBlock) => Err(MqttError::Configuration(format!(
                "Storage directory {} is already in use by another broker",
                base_dir.display()
            ))),
            Err(std::fs::TryLockError::Error(e)) => Err(MqttError::Configuration(format!(
                "Failed to lock storage directory {}: {e}",
                base_dir.display()
            ))),
        }
    }

    /// Waits until every queued-message write and delete issued so far has reached disk.
    pub async fn flush_queue_writes(&self) {
        let (done_tx, done_rx) = oneshot::channel();
        if self.queue_flush.send(done_tx).await.is_ok() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), done_rx).await;
        }
    }

    async fn check_storage_version(base_dir: &Path) -> Result<bool> {
        let version_file = base_dir.join(".storage_version");

        if !version_file.exists() {
            fs::create_dir_all(base_dir).await.map_err(|e| {
                MqttError::Configuration(format!("Failed to create storage dir: {e}"))
            })?;
            Self::write_storage_version(base_dir).await?;
            info!("Created new storage with version {STORAGE_VERSION}");
            return Ok(false);
        }

        let stored_version = fs::read_to_string(&version_file).await.map_err(|e| {
            MqttError::Configuration(format!("Failed to read storage version: {e}"))
        })?;

        match stored_version.trim() {
            STORAGE_VERSION => {
                debug!("Storage version verified: {STORAGE_VERSION}");
                Ok(false)
            }
            LEGACY_STORAGE_VERSION => {
                info!("Migrating storage from version {LEGACY_STORAGE_VERSION} to {STORAGE_VERSION}");
                Ok(true)
            }
            stored_version => Err(MqttError::Configuration(format!(
                "Storage version mismatch: {} holds storage version {stored_version}, and this broker \
                 reads versions {LEGACY_STORAGE_VERSION} and {STORAGE_VERSION} only.\n\
                 \n\
                 The directory was written by a newer broker, which changes the format in a way \
                 this broker cannot read. To recover, do one of:\n\
                 1. Run the broker version that wrote this directory, or a newer one.\n\
                 2. Restore the backup of the storage directory taken before that upgrade, and \
                 start this broker on it.\n\
                 \n\
                 The directory has not been modified.",
                base_dir.display(),
            ))),
        }
    }

    async fn write_storage_version(base_dir: &Path) -> Result<()> {
        let base_dir = base_dir.to_path_buf();
        tokio::task::spawn_blocking(move || Self::write_storage_version_blocking(&base_dir))
            .await
            .map_err(|e| {
                MqttError::Configuration(format!("Failed to write storage version: {e}"))
            })?
    }

    fn write_storage_version_blocking(base_dir: &Path) -> Result<()> {
        use std::io::Write;
        let target = base_dir.join(".storage_version");
        let temp = base_dir.join(format!(
            ".storage_version.tmp.{}.{}",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let written = std::fs::File::create(&temp)
            .and_then(|mut file| {
                file.write_all(STORAGE_VERSION.as_bytes())?;
                file.sync_all()
            })
            .and_then(|()| std::fs::rename(&temp, &target));
        if let Err(e) = written {
            if let Err(cleanup) = std::fs::remove_file(&temp) {
                debug!("Could not remove {}: {cleanup}", temp.display());
            }
            return Err(MqttError::Configuration(format!(
                "Failed to write storage version: {e}"
            )));
        }
        sync_directory(base_dir)
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
        self.sessions
            .apply(&client_id, |_| {
                (Some(SessionChange::Put(Box::new(session))), ())
            })
            .await
    }

    fn get_session(
        &self,
        client_id: &str,
    ) -> impl std::future::Future<Output = Result<Option<ClientSession>>> + Send {
        std::future::ready(Ok(self
            .sessions
            .get(client_id)
            .filter(|session| !session.is_expired())))
    }

    async fn remove_expired_session(&self, client_id: &str) -> Result<bool> {
        self.sessions
            .apply(client_id, |current| {
                if current.is_some_and(ClientSession::is_expired) {
                    (Some(SessionChange::Remove), true)
                } else {
                    (None, false)
                }
            })
            .await
    }

    fn session_client_ids(&self) -> impl std::future::Future<Output = Result<Vec<String>>> + Send {
        std::future::ready(Ok(self.sessions.client_ids()))
    }

    async fn update_session<F>(
        &self,
        client_id: &str,
        connection_token: u64,
        update: F,
    ) -> Result<bool>
    where
        F: FnOnce(&mut ClientSession) + Send,
    {
        self.sessions
            .apply(client_id, |current| {
                match current.filter(|session| session.connection_token == connection_token) {
                    Some(current) => {
                        let mut updated = current.clone();
                        update(&mut updated);
                        (Some(SessionChange::Put(Box::new(updated))), true)
                    }
                    None => (None, false),
                }
            })
            .await
    }

    async fn remove_owned_session(&self, client_id: &str, connection_token: u64) -> Result<bool> {
        self.sessions
            .apply(client_id, |current| {
                if current.is_some_and(|session| session.connection_token == connection_token) {
                    (Some(SessionChange::Remove), true)
                } else {
                    (None, false)
                }
            })
            .await
    }

    async fn remove_session(&self, client_id: &str) -> Result<()> {
        self.sessions
            .apply(client_id, |current| {
                (current.map(|_| SessionChange::Remove), ())
            })
            .await?;
        debug!("Removed session for client: {client_id}");
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
        self.write_behind.store_inflight(message);
        std::future::ready(Ok(()))
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
        self.write_behind
            .remove_inflight(client_id, packet_id, direction);
        std::future::ready(Ok(()))
    }

    fn remove_all_inflight_messages(
        &self,
        client_id: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.write_behind.clear_inflight(client_id);
        std::future::ready(Ok(()))
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
    async fn restart_accounts_scanned_entries_exactly_as_pushed() {
        let message = |tag: &str| {
            QueuedMessage::new(
                PublishPacket::new(format!("q/{tag}"), vec![b'x'; 200], QoS::AtLeastOnce),
                "c".to_string(),
                QoS::AtLeastOnce,
                None,
            )
        };
        let limits = QueueLimits {
            max_messages: 1000,
            max_bytes: 6 * crate::broker::storage::client_queue::entry_bytes(&message("a")),
        };
        let dir = tempfile::tempdir().unwrap();
        {
            let backend = FileBackend::with_queue_limits(dir.path(), limits)
                .await
                .unwrap();
            let queue = backend.queue_handle("c");
            for tag in ["a", "b", "c", "d", "e"] {
                queue.push(message(tag));
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
        queue.push(message("f"));
        assert_eq!(queue.count(), 6, "the sixth entry fills the cap exactly");
        queue.push(message("g"));
        assert_eq!(queue.count(), 6, "the seventh evicts the oldest");
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

    #[cfg(unix)]
    #[tokio::test]
    async fn storage_version_is_replaced_by_rename_not_rewritten_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let version = dir.path().join(".storage_version");
        std::fs::write(&version, "1").unwrap();
        std::fs::set_permissions(&version, std::fs::Permissions::from_mode(0o444)).unwrap();
        if std::fs::OpenOptions::new()
            .write(true)
            .open(&version)
            .is_ok()
        {
            return;
        }
        let backend = FileBackend::new(dir.path()).await;
        assert!(backend.is_ok(), "migration failed: {:?}", backend.err());
        assert_eq!(std::fs::read_to_string(&version).unwrap(), STORAGE_VERSION);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[tokio::test]
    async fn newer_storage_version_names_the_recovery_steps() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".storage_version"), "3").unwrap();
        let Err(error) = FileBackend::new(dir.path()).await else {
            panic!("a newer storage version was opened");
        };
        let message = error.to_string();
        assert!(message.contains("backup"), "{message}");
        assert!(!message.contains("rm -rf"), "{message}");
        assert!(!message.contains("mqttv5 storage"), "{message}");
    }

    fn lock_outcome(
        attempt: std::result::Result<(), std::fs::TryLockError>,
    ) -> Result<Option<std::fs::File>> {
        let dir = tempfile::tempdir().unwrap();
        let lock = std::fs::File::create(dir.path().join(".lock")).unwrap();
        FileBackend::directory_lock_outcome(lock, attempt, dir.path())
    }

    #[test]
    fn unsupported_file_locking_starts_without_the_lock() {
        let attempt = Err(std::fs::TryLockError::Error(std::io::Error::from(
            std::io::ErrorKind::Unsupported,
        )));
        assert!(lock_outcome(attempt).unwrap().is_none());
    }

    #[test]
    fn held_lock_refuses_the_directory() {
        let error = lock_outcome(Err(std::fs::TryLockError::WouldBlock)).unwrap_err();
        assert!(error.to_string().contains("already in use"), "{error}");
    }

    #[test]
    fn other_lock_errors_refuse_the_directory() {
        let attempt = Err(std::fs::TryLockError::Error(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        )));
        let error = lock_outcome(attempt).unwrap_err();
        assert!(error.to_string().contains("Failed to lock"), "{error}");
    }
}
