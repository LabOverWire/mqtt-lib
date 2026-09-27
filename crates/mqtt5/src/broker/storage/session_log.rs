use super::{unix_millis_now, ClientSession};
use crate::error::{MqttError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex as AsyncMutex, OwnedMutexGuard};
use tracing::{debug, error, info, warn};

pub(super) const LOG_FILE: &str = "sessions.log";
const COMPACT_FLOOR: u64 = 1 << 20;
const CHECKSUM_LEN: usize = 8;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut index: u32 = 0;
    while index < 256 {
        let mut crc = index;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[index as usize] = crc;
        index += 1;
    }
    table
};

fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(u32::MAX, |crc, byte| {
        CRC_TABLE[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8)
    })
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Op {
    Put,
    Remove,
}

#[derive(Serialize)]
struct RecordRef<'a> {
    op: Op,
    id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<&'a ClientSession>,
}

#[derive(Deserialize)]
struct Record {
    op: Op,
    id: String,
    #[serde(default)]
    session: Option<ClientSession>,
}

enum Replayed {
    Put(String, Box<ClientSession>),
    Remove(String),
}

pub(super) enum SessionChange {
    Put(Box<ClientSession>),
    Remove,
}

enum Entry {
    Barrier,
    Write {
        client_id: String,
        line: Vec<u8>,
        live: bool,
        undo: Option<Box<ClientSession>>,
    },
}

struct Pending {
    seq: u64,
    entry: Entry,
    done: oneshot::Sender<Result<()>>,
}

struct LogState {
    sessions: HashMap<String, ClientSession>,
    pending: Vec<Pending>,
    next_seq: u64,
    settled_seq: u64,
}

impl LogState {
    fn all_settled(&self) -> bool {
        self.settled_seq + 1 == self.next_seq
    }

    fn enqueue(&mut self, entry: Entry) -> oneshot::Receiver<Result<()>> {
        let (done, settled) = oneshot::channel();
        let seq = self.next_seq;
        self.next_seq += 1;
        self.pending.push(Pending { seq, entry, done });
        settled
    }
}

struct LogLine {
    client_id: String,
    line: Vec<u8>,
    live: bool,
}

struct LogWriter {
    dir: PathBuf,
    path: PathBuf,
    file: Option<File>,
    len: u64,
    durable: HashMap<String, Vec<u8>>,
    live_bytes: u64,
    health: Health,
    flushes: u64,
    #[cfg(test)]
    gate: Option<Arc<std::sync::Barrier>>,
    #[cfg(test)]
    faults: Faults,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Health {
    Sound,
    NeedsRepair,
    DamagedOriginal,
}

#[cfg(test)]
#[derive(Default)]
struct Faults {
    sync: bool,
    truncate: bool,
}

impl LogWriter {
    fn append(&mut self, lines: &[LogLine]) -> Result<()> {
        #[cfg(test)]
        if let Some(gate) = self.gate.take() {
            gate.wait();
        }
        if self.health != Health::Sound {
            self.repair()?;
        }
        let bytes: Vec<u8> = lines
            .iter()
            .flat_map(|line| line.line.iter().copied())
            .collect();
        if !bytes.is_empty() {
            if let Err(e) = self.write_and_sync(&bytes) {
                self.discard_failed_append();
                return Err(e);
            }
            self.len += bytes.len() as u64;
            self.flushes += 1;
            debug!(
                records = lines.len(),
                flushes = self.flushes,
                "Group-committed session writes"
            );
        }
        for line in lines {
            self.record_durable(&line.client_id, &line.line, line.live);
        }
        Ok(())
    }

    fn write_and_sync(&mut self, bytes: &[u8]) -> Result<()> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| MqttError::Io("session log is not open".to_string()))?;
        file.write_all(bytes)
            .map_err(|e| MqttError::Io(format!("Failed to append to the session log: {e}")))?;
        #[cfg(test)]
        if std::mem::take(&mut self.faults.sync) {
            return Err(MqttError::Io("injected fsync failure".to_string()));
        }
        file.sync_data()
            .map_err(|e| MqttError::Io(format!("Failed to sync the session log: {e}")))
    }

    fn discard_failed_append(&mut self) {
        match self.truncate_to_durable() {
            Ok(()) => {
                debug!(
                    bytes = self.len,
                    "Truncated the session log back to its durable length"
                );
                return;
            }
            Err(e) => warn!("Could not truncate the failed session write away: {e}"),
        }
        self.health = Health::NeedsRepair;
        if let Err(e) = self.repair() {
            error!("Session log repair failed; session writes are refused until it succeeds: {e}");
        }
    }

    fn truncate_to_durable(&mut self) -> Result<()> {
        #[cfg(test)]
        if std::mem::take(&mut self.faults.truncate) {
            return Err(MqttError::Io("injected truncate failure".to_string()));
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| MqttError::Io("session log is not open".to_string()))?;
        file.set_len(self.len)
            .and_then(|()| file.sync_all())
            .map_err(|e| MqttError::Io(format!("Failed to truncate the session log: {e}")))
    }

    fn record_durable(&mut self, client_id: &str, record: &[u8], keep: bool) {
        let previous = if keep {
            self.durable.insert(client_id.to_string(), record.to_vec())
        } else {
            self.durable.remove(client_id)
        };
        if let Some(previous) = previous {
            self.live_bytes = self.live_bytes.saturating_sub(previous.len() as u64);
        }
        if keep {
            self.live_bytes += record.len() as u64;
        }
    }

    fn needs_compaction(&self) -> bool {
        self.len > COMPACT_FLOOR.max(self.live_bytes.saturating_mul(2))
    }

    fn repair(&mut self) -> Result<()> {
        if self.health == Health::DamagedOriginal {
            let copy = preserve_copy(&self.dir, &self.path)?;
            self.health = Health::NeedsRepair;
            error!(
                "Session log {} held damaged records; the original is kept as {}",
                self.path.display(),
                copy.display()
            );
        }
        self.compact()
    }

    fn compact(&mut self) -> Result<()> {
        let temp = self.dir.join(format!(
            "{LOG_FILE}.tmp.{}.{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        if let Err(e) = write_snapshot(&temp, self.durable.values()) {
            remove_quietly(&temp);
            self.mark_for_repair();
            return Err(e);
        }
        self.file = None;
        if let Err(e) = fs::rename(&temp, &self.path) {
            remove_quietly(&temp);
            self.mark_for_repair();
            return Err(MqttError::Io(format!(
                "Failed to install the compacted session log: {e}"
            )));
        }
        let reopened = sync_directory(&self.dir).and_then(|()| open_append(&self.path));
        match reopened {
            Ok(file) => {
                self.file = Some(file);
                self.len = self.live_bytes;
                self.health = Health::Sound;
                debug!(bytes = self.len, "Compacted the session log");
                Ok(())
            }
            Err(e) => {
                self.mark_for_repair();
                Err(e)
            }
        }
    }

    fn mark_for_repair(&mut self) {
        if self.health == Health::Sound {
            self.health = Health::NeedsRepair;
        }
    }
}

struct Shared {
    state: parking_lot::Mutex<LogState>,
    writer: Arc<AsyncMutex<LogWriter>>,
}

pub(super) struct SessionLog {
    shared: Arc<Shared>,
}

impl SessionLog {
    pub(super) async fn open(dir: PathBuf, import_legacy: bool) -> Result<Self> {
        let (writer, sessions) =
            tokio::task::spawn_blocking(move || open_blocking(&dir, import_legacy))
                .await
                .map_err(|e| MqttError::Io(format!("Session log loader failed: {e}")))??;
        Ok(Self {
            shared: Arc::new(Shared {
                state: parking_lot::Mutex::new(LogState {
                    sessions,
                    pending: Vec::new(),
                    next_seq: 1,
                    settled_seq: 0,
                }),
                writer: Arc::new(AsyncMutex::new(writer)),
            }),
        })
    }

    #[cfg(test)]
    pub(super) async fn pause_writes(&self) -> impl Sized {
        Arc::clone(&self.shared.writer).lock_owned().await
    }

    #[cfg(test)]
    pub(super) async fn break_next_write(&self) {
        self.shared.writer.lock().await.file = None;
    }

    #[cfg(test)]
    pub(super) async fn flushes(&self) -> u64 {
        self.shared.writer.lock().await.flushes
    }

    pub(super) fn get(&self, client_id: &str) -> Option<ClientSession> {
        self.shared.state.lock().sessions.get(client_id).cloned()
    }

    pub(super) fn client_ids(&self) -> Vec<String> {
        self.shared.state.lock().sessions.keys().cloned().collect()
    }

    pub(super) async fn apply<R>(
        &self,
        client_id: &str,
        decide: impl FnOnce(Option<&ClientSession>) -> (Option<SessionChange>, R),
    ) -> Result<R> {
        let (settled, outcome) = {
            let mut state = self.shared.state.lock();
            let (change, outcome) = decide(state.sessions.get(client_id));
            let entry = match change {
                Some(SessionChange::Put(session)) => {
                    let line = encode(client_id, Some(&session))?;
                    let undo = state
                        .sessions
                        .insert(client_id.to_string(), *session)
                        .map(Box::new);
                    Entry::Write {
                        client_id: client_id.to_string(),
                        line,
                        live: true,
                        undo,
                    }
                }
                Some(SessionChange::Remove) => {
                    let line = encode(client_id, None)?;
                    let undo = state.sessions.remove(client_id).map(Box::new);
                    Entry::Write {
                        client_id: client_id.to_string(),
                        line,
                        live: false,
                        undo,
                    }
                }
                None if state.all_settled() => return Ok(outcome),
                None => Entry::Barrier,
            };
            (state.enqueue(entry), outcome)
        };
        tokio::spawn(flush(Arc::clone(&self.shared)));
        match settled.await {
            Ok(Ok(())) => Ok(outcome),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(MqttError::Io(
                "session log writer stopped before the write settled".to_string(),
            )),
        }
    }
}

async fn flush(shared: Arc<Shared>) {
    let writer = Arc::clone(&shared.writer).lock_owned().await;
    let batch = std::mem::take(&mut shared.state.lock().pending);
    let Some(last_seq) = batch.last().map(|pending| pending.seq) else {
        return;
    };
    let lines: Vec<LogLine> = batch
        .iter()
        .filter_map(|pending| match &pending.entry {
            Entry::Barrier => None,
            Entry::Write {
                client_id,
                line,
                live,
                ..
            } => Some(LogLine {
                client_id: client_id.clone(),
                line: line.clone(),
                live: *live,
            }),
        })
        .collect();
    let appended = tokio::task::spawn_blocking(move || {
        let mut writer = writer;
        let result = writer.append(&lines);
        (writer, result)
    })
    .await;
    let (writer, result) = match appended {
        Ok((writer, result)) => (Some(writer), result),
        Err(e) => (
            None,
            Err(MqttError::Io(format!("Session log writer failed: {e}"))),
        ),
    };
    match result {
        Ok(()) => settle(&shared, batch, last_seq),
        Err(e) => {
            warn!("Session log write failed; rejecting the pending session writes: {e}");
            fail(&shared, batch, &e);
        }
    }
    if let Some(writer) = writer {
        compact_if_needed(writer).await;
    }
}

fn settle(shared: &Shared, batch: Vec<Pending>, last_seq: u64) {
    shared.state.lock().settled_seq = last_seq;
    for pending in batch {
        if pending.done.send(Ok(())).is_err() {
            debug!("Session write settled after its caller went away");
        }
    }
}

fn fail(shared: &Shared, batch: Vec<Pending>, error: &MqttError) {
    let mut state = shared.state.lock();
    let queued = std::mem::take(&mut state.pending);
    let failed: Vec<Pending> = batch.into_iter().chain(queued).collect();
    if let Some(last) = failed.last() {
        state.settled_seq = last.seq;
    }
    for pending in failed.iter().rev() {
        if let Entry::Write {
            client_id, undo, ..
        } = &pending.entry
        {
            match undo {
                Some(previous) => {
                    state
                        .sessions
                        .insert(client_id.clone(), previous.as_ref().clone());
                }
                None => {
                    state.sessions.remove(client_id);
                }
            }
        }
    }
    drop(state);
    for pending in failed {
        if pending.done.send(Err(error.clone())).is_err() {
            debug!("Session write failed after its caller went away");
        }
    }
}

async fn compact_if_needed(writer: OwnedMutexGuard<LogWriter>) {
    if writer.health != Health::Sound || !writer.needs_compaction() {
        return;
    }
    let compacted = tokio::task::spawn_blocking(move || {
        let mut writer = writer;
        writer.compact()
    })
    .await;
    match compacted {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!("Session log compaction failed; will retry: {e}"),
        Err(e) => warn!("Session log compaction task failed: {e}"),
    }
}

fn encode(client_id: &str, session: Option<&ClientSession>) -> Result<Vec<u8>> {
    let op = if session.is_some() {
        Op::Put
    } else {
        Op::Remove
    };
    let body = serde_json::to_vec(&RecordRef {
        op,
        id: client_id,
        session,
    })
    .map_err(|e| MqttError::Io(format!("Failed to encode session {client_id}: {e}")))?;
    let mut line = Vec::with_capacity(CHECKSUM_LEN + body.len() + 2);
    line.extend_from_slice(format!("{:08x} ", crc32(&body)).as_bytes());
    line.extend_from_slice(&body);
    line.push(b'\n');
    Ok(line)
}

fn decode(line: &[u8]) -> Option<Replayed> {
    let (checksum, body) = line.split_at_checked(CHECKSUM_LEN + 1)?;
    let (hex, separator) = checksum.split_at(CHECKSUM_LEN);
    if separator != b" " {
        return None;
    }
    let expected = u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
    if crc32(body) != expected {
        return None;
    }
    let record = serde_json::from_slice::<Record>(body).ok()?;
    match (record.op, record.session) {
        (Op::Put, Some(session)) if session.client_id == record.id => {
            Some(Replayed::Put(record.id, Box::new(session)))
        }
        (Op::Remove, None) => Some(Replayed::Remove(record.id)),
        _ => None,
    }
}

type Loaded = HashMap<String, (ClientSession, Vec<u8>)>;

#[derive(Default)]
struct ReplaySummary {
    damaged: usize,
    torn_bytes: usize,
}

fn open_blocking(
    dir: &Path,
    import_legacy: bool,
) -> Result<(LogWriter, HashMap<String, ClientSession>)> {
    remove_temp_files(dir);
    let path = dir.join(LOG_FILE);
    let mut loaded = Loaded::new();
    let mut summary = ReplaySummary::default();
    match fs::read(&path) {
        Ok(bytes) => summary = replay(&bytes, &mut loaded),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            let aside = unique_aside(&path, "unreadable");
            error!(
                "Session log {} is unreadable ({e}); moving it to {} and starting without sessions",
                path.display(),
                aside.display()
            );
            fs::rename(&path, &aside).map_err(|e| {
                MqttError::Io(format!(
                    "Session log {} is unreadable and cannot be moved aside: {e}",
                    path.display()
                ))
            })?;
        }
    }
    if summary.damaged > 0 {
        error!(
            damaged = summary.damaged,
            "Session log {} has damaged records; they were skipped and every other record was replayed",
            path.display()
        );
    }
    if summary.torn_bytes > 0 {
        warn!(
            discarded = summary.torn_bytes,
            "Session log ends in an incomplete record from an unfinished write; discarding it"
        );
    }
    let legacy = if import_legacy {
        import_legacy_files(dir, &mut loaded)
    } else {
        warn_about_legacy_files(dir);
        Vec::new()
    };

    let durable: HashMap<String, Vec<u8>> = loaded
        .iter()
        .map(|(id, (_, line))| (id.clone(), line.clone()))
        .collect();
    let live_bytes = durable.values().map(|line| line.len() as u64).sum();
    let mut writer = LogWriter {
        dir: dir.to_path_buf(),
        path,
        file: None,
        len: 0,
        durable,
        live_bytes,
        health: if summary.damaged > 0 {
            Health::DamagedOriginal
        } else {
            Health::NeedsRepair
        },
        flushes: 0,
        #[cfg(test)]
        gate: None,
        #[cfg(test)]
        faults: Faults::default(),
    };
    if let Err(e) = writer.repair() {
        if import_legacy {
            return Err(MqttError::Io(format!(
                "Cannot migrate the sessions in {} to the session log: {e}",
                dir.display()
            )));
        }
        warn!(
            "Session log could not be rewritten at startup; serving the replayed sessions and refusing session writes until it can: {e}"
        );
    }
    for file in &legacy {
        if let Err(e) = fs::remove_file(file) {
            warn!(
                "Imported legacy session file {} could not be removed: {e}",
                file.display()
            );
        }
    }
    if !legacy.is_empty() {
        sync_directory(dir)?;
        info!(
            count = legacy.len(),
            "Migrated legacy session files into the session log"
        );
    }
    let sessions = loaded
        .into_iter()
        .map(|(id, (session, _))| (id, session))
        .collect();
    Ok((writer, sessions))
}

fn replay(bytes: &[u8], loaded: &mut Loaded) -> ReplaySummary {
    let mut summary = ReplaySummary::default();
    for segment in bytes.split_inclusive(|byte| *byte == b'\n') {
        let Some(line) = segment.strip_suffix(b"\n") else {
            summary.torn_bytes = segment.len();
            break;
        };
        match decode(line) {
            Some(Replayed::Put(id, session)) => {
                loaded.insert(id, (*session, segment.to_vec()));
            }
            Some(Replayed::Remove(id)) => {
                loaded.remove(&id);
            }
            None => summary.damaged += 1,
        }
    }
    summary
}

fn remove_temp_files(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for path in entries
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
    {
        let is_temp = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_temp_name);
        if is_temp && path.is_file() {
            match fs::remove_file(&path) {
                Ok(()) => info!("Removed leftover temporary file {}", path.display()),
                Err(e) => warn!(
                    "Could not remove leftover temporary file {}: {e}",
                    path.display()
                ),
            }
        }
    }
}

fn is_temp_name(name: &str) -> bool {
    let mut parts = name.rsplit('.');
    let counter = parts.next();
    let pid = parts.next();
    let marker = parts.next();
    let numeric = |part: Option<&str>| {
        part.is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    };
    marker == Some("tmp") && numeric(pid) && numeric(counter) && parts.next().is_some()
}

fn unique_aside(path: &Path, tag: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(LOG_FILE);
    let stamp = unix_millis_now();
    let mut attempt = 0u32;
    loop {
        let candidate = if attempt == 0 {
            path.with_file_name(format!("{name}.{tag}-{stamp}"))
        } else {
            path.with_file_name(format!("{name}.{tag}-{stamp}-{attempt}"))
        };
        if !candidate.exists() {
            return candidate;
        }
        attempt += 1;
    }
}

fn preserve_copy(dir: &Path, path: &Path) -> Result<PathBuf> {
    let aside = unique_aside(path, "corrupt");
    let copied = fs::copy(path, &aside)
        .and_then(|_| File::open(&aside))
        .and_then(|copy| copy.sync_all());
    if let Err(e) = copied {
        remove_quietly(&aside);
        return Err(MqttError::Io(format!(
            "Failed to preserve the damaged session log as {}: {e}",
            aside.display()
        )));
    }
    sync_directory(dir)?;
    Ok(aside)
}

fn legacy_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "json"))
        .collect()
}

fn warn_about_legacy_files(dir: &Path) {
    let leftover = legacy_files(dir).len();
    if leftover > 0 {
        warn!(
            leftover,
            dir = %dir.display(),
            "Ignoring per-session files left beside the session log"
        );
    }
}

fn quarantine_legacy(path: &Path, reason: &str) {
    let aside = unique_aside(path, "corrupt");
    match fs::rename(path, &aside) {
        Ok(()) => error!(
            "Legacy session file {} was not migrated ({reason}); kept as {}",
            path.display(),
            aside.display()
        ),
        Err(e) => error!(
            "Legacy session file {} was not migrated ({reason}) and could not be moved aside: {e}",
            path.display()
        ),
    }
}

fn import_legacy_files(dir: &Path, loaded: &mut Loaded) -> Vec<PathBuf> {
    let mut imported = Vec::new();
    for path in legacy_files(dir) {
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                quarantine_legacy(&path, &e.to_string());
                continue;
            }
        };
        let session = match serde_json::from_slice::<ClientSession>(&bytes) {
            Ok(session) => session,
            Err(e) => {
                quarantine_legacy(&path, &e.to_string());
                continue;
            }
        };
        match encode(&session.client_id, Some(&session)) {
            Ok(line) => {
                loaded.insert(session.client_id.clone(), (session, line));
                imported.push(path);
            }
            Err(e) => quarantine_legacy(&path, &e.to_string()),
        }
    }
    imported
}

fn write_snapshot<'a>(path: &Path, lines: impl Iterator<Item = &'a Vec<u8>>) -> Result<()> {
    let file = File::create(path)
        .map_err(|e| MqttError::Io(format!("Failed to create {}: {e}", path.display())))?;
    let mut out = BufWriter::new(file);
    for line in lines {
        out.write_all(line)
            .map_err(|e| MqttError::Io(format!("Failed to write {}: {e}", path.display())))?;
    }
    let file = out
        .into_inner()
        .map_err(|e| MqttError::Io(format!("Failed to write {}: {e}", path.display())))?;
    file.sync_all()
        .map_err(|e| MqttError::Io(format!("Failed to sync {}: {e}", path.display())))
}

fn open_append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| MqttError::Io(format!("Failed to open {}: {e}", path.display())))
}

fn remove_quietly(path: &Path) {
    if let Err(e) = fs::remove_file(path) {
        debug!("Could not remove {}: {e}", path.display());
    }
}

#[cfg(unix)]
pub(super) fn sync_directory(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|handle| handle.sync_all())
        .map_err(|e| MqttError::Io(format!("Failed to sync {}: {e}", dir.display())))
}

#[cfg(not(unix))]
pub(super) fn sync_directory(dir: &Path) -> Result<()> {
    fs::metadata(dir)
        .map(|_| ())
        .map_err(|e| MqttError::Io(format!("Failed to stat {}: {e}", dir.display())))
}

#[cfg(test)]
mod tests {
    use super::{SessionChange, SessionLog, LOG_FILE};
    use crate::broker::storage::ClientSession;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    async fn open(dir: &Path) -> Arc<SessionLog> {
        Arc::new(SessionLog::open(dir.to_path_buf(), false).await.unwrap())
    }

    async fn put(log: &SessionLog, session: ClientSession) -> crate::error::Result<()> {
        let client_id = session.client_id.clone();
        log.apply(&client_id, |_| {
            (Some(SessionChange::Put(Box::new(session))), ())
        })
        .await
    }

    fn spawn_put(
        log: &Arc<SessionLog>,
        session: ClientSession,
    ) -> tokio::task::JoinHandle<crate::error::Result<()>> {
        let log = Arc::clone(log);
        tokio::spawn(async move { put(&log, session).await })
    }

    fn on_disk(dir: &Path) -> String {
        std::fs::read_to_string(dir.join(LOG_FILE)).unwrap()
    }

    async fn wait_for_pending(log: &SessionLog, count: usize) {
        for _ in 0..500 {
            if log.shared.state.lock().pending.len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("writes never queued");
    }

    #[tokio::test]
    async fn write_is_visible_at_once_and_acknowledged_only_once_durable() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        let paused = Arc::clone(&log.shared.writer).lock_owned().await;
        let write = spawn_put(&log, ClientSession::new("a", true, Some(60)));
        wait_for_pending(&log, 1).await;
        assert!(log.get("a").is_some(), "a later reader must see the write");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !write.is_finished(),
            "the write was acknowledged before it was durable"
        );
        assert!(!on_disk(dir.path()).contains("\"id\":\"a\""));
        drop(paused);
        write.await.unwrap().unwrap();
        assert!(on_disk(dir.path()).contains("\"id\":\"a\""));
    }

    #[tokio::test]
    async fn removal_is_acknowledged_only_once_durable() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();
        let paused = Arc::clone(&log.shared.writer).lock_owned().await;
        let removing = Arc::clone(&log);
        let removal = tokio::spawn(async move {
            removing
                .apply("a", |current| (current.map(|_| SessionChange::Remove), ()))
                .await
        });
        wait_for_pending(&log, 1).await;
        assert!(log.get("a").is_none());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !removal.is_finished(),
            "the removal was acknowledged before it was durable"
        );
        drop(paused);
        removal.await.unwrap().unwrap();
        drop(log);
        assert!(open(dir.path()).await.get("a").is_none());
    }

    #[tokio::test]
    async fn unchanged_outcome_waits_for_the_writes_it_observed() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        let paused = Arc::clone(&log.shared.writer).lock_owned().await;
        let write = spawn_put(&log, ClientSession::new("a", true, Some(60)));
        wait_for_pending(&log, 1).await;
        let reading = Arc::clone(&log);
        let observed = tokio::spawn(async move {
            reading
                .apply("a", |current| (None, current.is_some()))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !observed.is_finished(),
            "an outcome based on an unflushed write was acknowledged before that write"
        );
        drop(paused);
        write.await.unwrap().unwrap();
        assert!(observed.await.unwrap().unwrap());
        let idle = log.apply("b", |current| (None, current.is_some())).await;
        assert!(!idle.unwrap());
    }

    #[tokio::test]
    async fn concurrent_writes_share_one_flush() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        let paused = Arc::clone(&log.shared.writer).lock_owned().await;
        let before = paused.flushes;
        let writes: Vec<_> = (0..200)
            .map(|i| spawn_put(&log, ClientSession::new(format!("c{i}"), true, Some(60))))
            .collect();
        wait_for_pending(&log, 200).await;
        drop(paused);
        for write in writes {
            write.await.unwrap().unwrap();
        }
        assert_eq!(log.shared.writer.lock().await.flushes, before + 1);
        drop(log);
        let reopened = open(dir.path()).await;
        assert_eq!(reopened.client_ids().len(), 200);
    }

    #[tokio::test]
    async fn failed_flush_fails_every_pending_write_and_restores_what_was_durable() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();

        let mut paused = Arc::clone(&log.shared.writer).lock_owned().await;
        paused.file = None;
        let changed = spawn_put(&log, ClientSession::new("a", true, Some(99)));
        let added = spawn_put(&log, ClientSession::new("b", true, Some(60)));
        wait_for_pending(&log, 2).await;
        assert_eq!(log.get("a").unwrap().expiry_interval, Some(99));
        drop(paused);
        assert!(changed.await.unwrap().is_err());
        assert!(added.await.unwrap().is_err());
        assert_eq!(log.get("a").unwrap().expiry_interval, Some(60));
        assert!(log.get("b").is_none());

        put(&log, ClientSession::new("c", true, Some(60)))
            .await
            .expect("the next write repairs the log");
        drop(log);
        let reopened = open(dir.path()).await;
        assert_eq!(reopened.get("a").unwrap().expiry_interval, Some(60));
        assert!(reopened.get("b").is_none());
        assert!(reopened.get("c").is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_flush_also_fails_writes_queued_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("k", true, Some(1)))
            .await
            .unwrap();
        let gate = Arc::new(std::sync::Barrier::new(2));
        {
            let mut writer = log.shared.writer.lock().await;
            writer.file = None;
            writer.gate = Some(Arc::clone(&gate));
        }
        let first = spawn_put(&log, ClientSession::new("k", true, Some(2)));
        for _ in 0..500 {
            if log.get("k").unwrap().expiry_interval == Some(2)
                && log.shared.state.lock().pending.is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let building = Arc::clone(&log);
        let second = tokio::spawn(async move {
            building
                .apply("k", |current| {
                    let mut next = current.unwrap().clone();
                    next.persistent = false;
                    (Some(SessionChange::Put(Box::new(next))), ())
                })
                .await
        });
        wait_for_pending(&log, 1).await;
        tokio::task::spawn_blocking(move || {
            gate.wait();
        })
        .await
        .unwrap();
        assert!(first.await.unwrap().is_err());
        assert!(
            second.await.unwrap().is_err(),
            "a write built on a failed write must fail with it"
        );
        let restored = log.get("k").unwrap();
        assert_eq!(restored.expiry_interval, Some(1));
        assert!(restored.persistent);
        put(&log, ClientSession::new("other", true, Some(1)))
            .await
            .unwrap();
        drop(log);
        let reopened = open(dir.path()).await.get("k").unwrap();
        assert_eq!(reopened.expiry_interval, Some(1));
        assert!(reopened.persistent);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn earlier_write_never_lands_over_a_later_one_for_the_same_client_id() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        let gate = Arc::new(std::sync::Barrier::new(2));
        log.shared.writer.lock().await.gate = Some(Arc::clone(&gate));
        let older = spawn_put(&log, ClientSession::new("k", true, Some(1)));
        for _ in 0..500 {
            if log.get("k").is_some() && log.shared.state.lock().pending.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let newer = spawn_put(&log, ClientSession::new("k", true, Some(2)));
        wait_for_pending(&log, 1).await;
        assert!(!older.is_finished() && !newer.is_finished());
        tokio::task::spawn_blocking(move || {
            gate.wait();
        })
        .await
        .unwrap();
        older.await.unwrap().unwrap();
        newer.await.unwrap().unwrap();
        assert_eq!(log.get("k").unwrap().expiry_interval, Some(2));

        let paused = Arc::clone(&log.shared.writer).lock_owned().await;
        let third = spawn_put(&log, ClientSession::new("k", true, Some(3)));
        wait_for_pending(&log, 1).await;
        let fourth = spawn_put(&log, ClientSession::new("k", true, Some(4)));
        wait_for_pending(&log, 2).await;
        drop(paused);
        third.await.unwrap().unwrap();
        fourth.await.unwrap().unwrap();
        drop(log);
        assert_eq!(
            open(dir.path()).await.get("k").unwrap().expiry_interval,
            Some(4),
            "the durable record went back to an older write"
        );
    }

    #[tokio::test]
    async fn torn_tail_is_discarded_and_later_writes_survive() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();
        drop(log);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(LOG_FILE))
            .unwrap();
        std::io::Write::write_all(&mut file, b"{\"id\":\"x\",\"sess").unwrap();
        drop(file);

        let log = open(dir.path()).await;
        assert!(log.get("a").is_some());
        assert!(log.get("x").is_none());
        put(&log, ClientSession::new("b", true, Some(60)))
            .await
            .unwrap();
        drop(log);
        let reopened = open(dir.path()).await;
        assert!(reopened.get("a").is_some());
        assert!(reopened.get("b").is_some());
    }

    #[tokio::test]
    async fn unreadable_line_between_records_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let first = super::encode("a", Some(&ClientSession::new("a", true, Some(60)))).unwrap();
        let last = super::encode("b", Some(&ClientSession::new("b", true, Some(60)))).unwrap();
        let mut bytes = first;
        bytes.extend_from_slice(b"\0\0\0garbage\n");
        bytes.extend_from_slice(&last);
        std::fs::write(dir.path().join(LOG_FILE), bytes).unwrap();
        let log = open(dir.path()).await;
        assert!(log.get("a").is_some());
        assert!(
            log.get("b").is_some(),
            "a record after a damaged one stands alone and must be replayed"
        );
    }

    #[test]
    fn checksum_is_crc32() {
        assert_eq!(super::crc32(b"123456789"), 0xCBF4_3926);
    }

    #[tokio::test]
    async fn removal_is_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();
        log.apply("a", |current| (current.map(|_| SessionChange::Remove), ()))
            .await
            .unwrap();
        assert!(log.get("a").is_none());
        drop(log);
        assert!(open(dir.path()).await.get("a").is_none());
    }

    #[tokio::test]
    async fn compaction_bounds_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        for round in 0..300u32 {
            let mut session = ClientSession::new("big", true, Some(round));
            session.user_id = Some("u".repeat(10_000));
            put(&log, session).await.unwrap();
        }
        let len = std::fs::metadata(dir.path().join(LOG_FILE)).unwrap().len();
        assert!(
            len <= super::COMPACT_FLOOR + 20_000,
            "log grew to {len} bytes"
        );
        drop(log);
        assert_eq!(
            open(dir.path()).await.get("big").unwrap().expiry_interval,
            Some(299)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_log_is_moved_aside_instead_of_blocking_startup() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();
        drop(log);
        let path = dir.path().join(LOG_FILE);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            return;
        }
        let log = open(dir.path()).await;
        assert!(log.get("a").is_none());
        assert_eq!(siblings(dir.path(), "sessions.log.unreadable-").len(), 1);
        put(&log, ClientSession::new("b", true, Some(60)))
            .await
            .unwrap();
        drop(log);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        drop(open(dir.path()).await);
        assert_eq!(
            siblings(dir.path(), "sessions.log.unreadable-").len(),
            2,
            "a second unreadable log overwrote the first one moved aside"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn writes_are_refused_while_the_log_cannot_be_repaired() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::write(dir.path().join("probe"), b"").is_ok() {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        {
            let mut writer = log.shared.writer.lock().await;
            writer.faults.sync = true;
            writer.faults.truncate = true;
        }
        let failed = put(&log, ClientSession::new("a", true, Some(99))).await;
        let refused = put(&log, ClientSession::new("b", true, Some(60))).await;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(failed.is_err());
        assert!(
            refused.is_err(),
            "a write was appended after a failed write that could not be removed"
        );
        assert_eq!(log.get("a").unwrap().expiry_interval, Some(60));
        put(&log, ClientSession::new("c", true, Some(60)))
            .await
            .expect("the next write repairs the log");
        drop(log);
        let reopened = open(dir.path()).await;
        assert_eq!(reopened.get("a").unwrap().expiry_interval, Some(60));
        assert!(reopened.get("b").is_none());
        assert!(reopened.get("c").is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_file_that_cannot_be_read_is_moved_aside() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let unreadable = dir.path().join("locked.json");
        std::fs::write(
            &unreadable,
            serde_json::to_vec(&ClientSession::new("locked", true, Some(60))).unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&unreadable).is_ok() {
            return;
        }
        let garbled = dir.path().join("garbled.json");
        std::fs::write(&garbled, b"{not json").unwrap();
        let log = SessionLog::open(dir.path().to_path_buf(), true)
            .await
            .unwrap();
        assert!(log.get("locked").is_none());
        assert!(
            !unreadable.exists(),
            "an unreadable legacy file was left in place"
        );
        assert!(!garbled.exists());
        assert_eq!(siblings(dir.path(), "locked.json.corrupt-").len(), 1);
        assert_eq!(siblings(dir.path(), "garbled.json.corrupt-").len(), 1);
    }

    fn siblings(dir: &Path, prefix: &str) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix))
            })
            .collect()
    }

    fn line_starts(bytes: &[u8]) -> Vec<usize> {
        std::iter::once(0)
            .chain(
                bytes
                    .iter()
                    .enumerate()
                    .filter(|(_, byte)| **byte == b'\n')
                    .map(|(index, _)| index + 1),
            )
            .filter(|start| *start < bytes.len())
            .collect()
    }

    #[tokio::test]
    async fn write_whose_fsync_failed_is_not_resurrected_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();
        log.shared.writer.lock().await.faults.sync = true;
        assert!(put(&log, ClientSession::new("a", true, Some(99)))
            .await
            .is_err());
        assert_eq!(log.get("a").unwrap().expiry_interval, Some(60));
        drop(log);
        assert_eq!(
            open(dir.path()).await.get("a").unwrap().expiry_interval,
            Some(60),
            "a write reported as failed came back after a restart"
        );
    }

    #[tokio::test]
    async fn removal_whose_fsync_failed_is_not_applied_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();
        log.shared.writer.lock().await.faults.sync = true;
        let removed = log
            .apply("a", |current| (current.map(|_| SessionChange::Remove), ()))
            .await;
        assert!(removed.is_err());
        assert!(log.get("a").is_some());
        drop(log);
        assert!(
            open(dir.path()).await.get("a").is_some(),
            "a removal reported as failed was applied after a restart"
        );
    }

    #[tokio::test]
    async fn failed_write_is_removed_from_the_log_before_it_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();
        let durable = std::fs::read(dir.path().join(LOG_FILE)).unwrap();
        log.shared.writer.lock().await.faults.sync = true;
        assert!(put(&log, ClientSession::new("b", true, Some(60)))
            .await
            .is_err());
        assert_eq!(
            std::fs::read(dir.path().join(LOG_FILE)).unwrap(),
            durable,
            "the failed write is still in the log"
        );
    }

    #[tokio::test]
    async fn damaged_record_does_not_lose_the_records_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        for i in 0..100 {
            put(&log, ClientSession::new(format!("c{i}"), true, Some(60)))
                .await
                .unwrap();
        }
        drop(log);
        let path = dir.path().join(LOG_FILE);
        let mut bytes = std::fs::read(&path).unwrap();
        let damaged = line_starts(&bytes)[1];
        bytes[damaged] = b'X';
        std::fs::write(&path, &bytes).unwrap();

        let log = open(dir.path()).await;
        assert_eq!(
            log.client_ids().len(),
            99,
            "records after a damaged one were discarded"
        );
        let preserved = siblings(dir.path(), "sessions.log.corrupt-");
        assert_eq!(preserved.len(), 1, "the damaged log was not preserved");
        assert_eq!(std::fs::read(&preserved[0]).unwrap(), bytes);
        drop(log);
        assert_eq!(open(dir.path()).await.client_ids().len(), 99);
    }

    #[tokio::test]
    async fn damaged_session_field_is_not_read_as_a_removal() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();
        put(&log, ClientSession::new("a", true, Some(99)))
            .await
            .unwrap();
        drop(log);
        let path = dir.path().join(LOG_FILE);
        let mut bytes = std::fs::read(&path).unwrap();
        let key = b"\"session\"";
        let at = bytes
            .windows(key.len())
            .rposition(|window| window == key)
            .unwrap();
        bytes[at + 3] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();

        let log = open(dir.path()).await;
        assert!(
            log.get("a").is_some(),
            "a damaged update was replayed as a removal"
        );
    }

    #[tokio::test]
    async fn torn_tail_alone_is_not_treated_as_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();
        drop(log);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(LOG_FILE))
            .unwrap();
        std::io::Write::write_all(&mut file, b"0123").unwrap();
        drop(file);
        let log = open(dir.path()).await;
        assert!(log.get("a").is_some());
        assert!(siblings(dir.path(), "sessions.log.corrupt-").is_empty());
    }

    #[tokio::test]
    async fn removed_session_stays_removed_across_compaction_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();
        put(&log, ClientSession::new("b", true, Some(60)))
            .await
            .unwrap();
        log.apply("a", |current| (current.map(|_| SessionChange::Remove), ()))
            .await
            .unwrap();
        log.shared.writer.lock().await.compact().unwrap();
        drop(log);
        let reopened = open(dir.path()).await;
        assert!(
            reopened.get("a").is_none(),
            "a removed session came back from the compacted log"
        );
        assert!(reopened.get("b").is_some());
    }

    #[tokio::test]
    async fn leftover_temp_files_are_removed_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let stale_log = dir.path().join(format!("{LOG_FILE}.tmp.1.2"));
        let stale_v1 = dir.path().join("client.tmp.3.4");
        let session_named_like_temp = dir.path().join("a.tmp.3.4.json");
        std::fs::write(&stale_log, b"partial").unwrap();
        std::fs::write(&stale_v1, b"partial").unwrap();
        std::fs::write(&session_named_like_temp, b"{}").unwrap();
        let log = open(dir.path()).await;
        assert!(!stale_log.exists());
        assert!(!stale_v1.exists());
        assert!(
            session_named_like_temp.exists(),
            "a session file whose ClientID looks like a temp name was deleted"
        );
        drop(log);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_serves_the_replayed_sessions_when_compaction_fails() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path()).await;
        put(&log, ClientSession::new("a", true, Some(60)))
            .await
            .unwrap();
        drop(log);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::write(dir.path().join("probe"), b"").is_ok() {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let reopened = SessionLog::open(dir.path().to_path_buf(), false).await;
        let refused = match &reopened {
            Ok(log) => Some(put(log, ClientSession::new("b", true, Some(60))).await),
            Err(_) => None,
        };
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let log = reopened.expect("startup must not fail when compaction fails");
        assert!(log.get("a").is_some());
        assert!(
            refused.is_some_and(|result| result.is_err()),
            "a write was accepted before the log was repaired"
        );
        put(&log, ClientSession::new("c", true, Some(60)))
            .await
            .expect("the next write repairs the log");
        drop(log);
        let reopened = open(dir.path()).await;
        assert!(reopened.get("a").is_some());
        assert!(reopened.get("b").is_none());
        assert!(reopened.get("c").is_some());
    }
}
