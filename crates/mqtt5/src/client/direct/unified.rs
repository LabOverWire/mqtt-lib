//! Unified reader and writer types for all transport types

use crate::error::{MqttError, Result};
use crate::packet::Packet;
use crate::transport::packet_io::{encode_packet_to_buffer, read_packet_from_stream};
use crate::transport::tls::{TlsReadHalf, TlsWriteHalf};
use bytes::BytesMut;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::watch;

#[cfg(feature = "transport-websocket")]
use crate::transport::websocket::{WebSocketReadHandle, WebSocketWriteHandle};
#[cfg(feature = "transport-quic")]
use quinn::{Connection, RecvStream, SendStream};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const RETAINED_WRITE_CAPACITY: usize = 64 * 1024;

#[cfg(feature = "transport-websocket")]
const WEBSOCKET_MESSAGE_LIMIT: usize = 16 * 1024;

const PENDING_LIMIT: usize = 1024 * 1024;

#[cfg(feature = "transport-quic")]
const QUIC_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

const CLOSE_GRACE: Duration = Duration::from_secs(1);

enum UnifiedReaderInner {
    Tcp(OwnedReadHalf),
    Tls(TlsReadHalf),
    #[cfg(feature = "transport-websocket")]
    WebSocket(WebSocketReadHandle),
    #[cfg(feature = "transport-quic")]
    Quic(RecvStream),
}

pub struct UnifiedReader {
    inner: UnifiedReaderInner,
    protocol_version: u8,
    read_buffer: BytesMut,
    max_packet_size: usize,
}

impl UnifiedReader {
    pub fn tcp(reader: OwnedReadHalf, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::Tcp(reader),
            protocol_version,
            read_buffer: BytesMut::new(),
            max_packet_size: usize::MAX,
        }
    }

    pub fn tls(reader: TlsReadHalf, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::Tls(reader),
            protocol_version,
            read_buffer: BytesMut::new(),
            max_packet_size: usize::MAX,
        }
    }

    #[cfg(feature = "transport-websocket")]
    pub fn websocket(reader: WebSocketReadHandle, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::WebSocket(reader),
            protocol_version,
            read_buffer: BytesMut::new(),
            max_packet_size: usize::MAX,
        }
    }

    #[cfg(feature = "transport-quic")]
    pub fn quic(reader: RecvStream, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::Quic(reader),
            protocol_version,
            read_buffer: BytesMut::new(),
            max_packet_size: usize::MAX,
        }
    }

    #[must_use]
    pub fn with_maximum_packet_size(mut self, maximum_packet_size: Option<u32>) -> Self {
        self.max_packet_size = maximum_packet_size
            .and_then(|size| usize::try_from(size).ok())
            .unwrap_or(usize::MAX);
        self
    }

    pub async fn read_packet(&mut self) -> Result<Packet> {
        match &mut self.inner {
            UnifiedReaderInner::Tcp(reader) => {
                read_packet_from_stream(
                    reader,
                    self.protocol_version,
                    &mut self.read_buffer,
                    self.max_packet_size,
                )
                .await
            }
            UnifiedReaderInner::Tls(reader) => {
                read_packet_from_stream(
                    reader,
                    self.protocol_version,
                    &mut self.read_buffer,
                    self.max_packet_size,
                )
                .await
            }
            #[cfg(feature = "transport-websocket")]
            UnifiedReaderInner::WebSocket(reader) => {
                reader
                    .read_packet_limited(self.protocol_version, self.max_packet_size)
                    .await
            }
            #[cfg(feature = "transport-quic")]
            UnifiedReaderInner::Quic(reader) => {
                read_packet_from_stream(
                    reader,
                    self.protocol_version,
                    &mut self.read_buffer,
                    self.max_packet_size,
                )
                .await
            }
        }
    }
}

enum WriterStream {
    Tcp(OwnedWriteHalf),
    Tls(TlsWriteHalf),
    #[cfg(feature = "transport-websocket")]
    WebSocket(WebSocketWriteHandle),
    #[cfg(feature = "transport-quic")]
    Quic(SendStream),
    #[cfg(feature = "transport-quic")]
    QuicControl(SendStream, Arc<Connection>),
    #[cfg(feature = "transport-quic")]
    QuicUnopened(Arc<Connection>),
    Closed,
}

struct Pending {
    bytes: BytesMut,
    reserved: usize,
    committed: u64,
    closed: bool,
    flushing: bool,
}

impl Pending {
    fn load(&self) -> usize {
        self.bytes.len() + self.reserved
    }

    fn is_full(&self) -> bool {
        self.load() >= PENDING_LIMIT
    }
}

#[derive(Clone, Copy, Default)]
struct Progress {
    flushed: u64,
    closed: bool,
    write_failed: bool,
    handoffs: u64,
}

const FLUSHER_ROUNDS_PAST_OWN_END: u32 = 16;

pub(super) struct Transport {
    stream: WriterStream,
    inflight: BytesMut,
    sink_pending: u64,
    oversized: bool,
    #[cfg(feature = "transport-quic")]
    prefix: BytesMut,
}

pub struct UnifiedWriter {
    pending: Mutex<Pending>,
    transport: tokio::sync::Mutex<Transport>,
    taken: AtomicU64,
    progress: watch::Sender<Progress>,
    room: tokio::sync::Notify,
    flush_wanted: AtomicU64,
    flush_queued: AtomicBool,
    background_flusher: OnceLock<tokio::sync::mpsc::UnboundedSender<Arc<UnifiedWriter>>>,
    #[cfg(feature = "transport-quic")]
    drained: Option<Arc<tokio::sync::Notify>>,
    #[cfg(feature = "transport-quic")]
    slot: Mutex<Option<tokio::sync::OwnedSemaphorePermit>>,
    #[cfg(test)]
    flusher_claims: AtomicU64,
    #[cfg(test)]
    flushers_active: AtomicU64,
    #[cfg(test)]
    flushers_peak: AtomicU64,
    #[cfg(test)]
    background_flushers_started: AtomicU64,
}

impl UnifiedWriter {
    fn new(stream: WriterStream) -> Self {
        Self {
            pending: Mutex::new(Pending {
                bytes: BytesMut::new(),
                reserved: 0,
                committed: 0,
                closed: false,
                flushing: false,
            }),
            transport: tokio::sync::Mutex::new(Transport {
                stream,
                inflight: BytesMut::new(),
                sink_pending: 0,
                oversized: false,
                #[cfg(feature = "transport-quic")]
                prefix: BytesMut::new(),
            }),
            taken: AtomicU64::new(0),
            progress: watch::channel(Progress::default()).0,
            room: tokio::sync::Notify::new(),
            flush_wanted: AtomicU64::new(0),
            flush_queued: AtomicBool::new(false),
            background_flusher: OnceLock::new(),
            #[cfg(feature = "transport-quic")]
            drained: None,
            #[cfg(feature = "transport-quic")]
            slot: Mutex::new(None),
            #[cfg(test)]
            flusher_claims: AtomicU64::new(0),
            #[cfg(test)]
            flushers_active: AtomicU64::new(0),
            #[cfg(test)]
            flushers_peak: AtomicU64::new(0),
            #[cfg(test)]
            background_flushers_started: AtomicU64::new(0),
        }
    }

    pub fn tcp(writer: OwnedWriteHalf) -> Self {
        Self::new(WriterStream::Tcp(writer))
    }

    pub fn tls(writer: TlsWriteHalf) -> Self {
        Self::new(WriterStream::Tls(writer))
    }

    #[cfg(feature = "transport-websocket")]
    pub fn websocket(writer: WebSocketWriteHandle) -> Self {
        Self::new(WriterStream::WebSocket(writer))
    }

    #[cfg(feature = "transport-quic")]
    pub fn quic(writer: SendStream) -> Self {
        Self::new(WriterStream::Quic(writer))
    }

    #[cfg(feature = "transport-quic")]
    pub fn quic_control(writer: SendStream, connection: Arc<Connection>) -> Self {
        Self::new(WriterStream::QuicControl(writer, connection))
    }

    #[cfg(feature = "transport-quic")]
    pub fn quic_unopened(
        connection: Arc<Connection>,
        drained: Arc<tokio::sync::Notify>,
        header: Option<BytesMut>,
    ) -> Self {
        let mut writer = Self::new(WriterStream::QuicUnopened(connection));
        writer.drained = Some(drained);
        if let Some(header) = header {
            writer.transport.get_mut().prefix = header;
        }
        writer
    }

    #[cfg(feature = "transport-quic")]
    pub fn hold_slot(&self, permit: tokio::sync::OwnedSemaphorePermit) {
        *self.slot.lock() = Some(permit);
    }

    pub async fn reserve_room(self: &Arc<Self>, size: usize) -> RoomReservation {
        loop {
            if let Some(reservation) = self.try_reserve_room(size) {
                return reservation;
            }
            let room = self.room.notified();
            tokio::pin!(room);
            room.as_mut().enable();
            if let Some(reservation) = self.try_reserve_room(size) {
                return reservation;
            }
            room.await;
        }
    }

    fn try_reserve_room(self: &Arc<Self>, size: usize) -> Option<RoomReservation> {
        let mut pending = self.pending.lock();
        if !pending.closed && pending.is_full() {
            return None;
        }
        pending.reserved += size;
        Some(RoomReservation {
            writer: Arc::clone(self),
            size,
            held: true,
        })
    }

    pub fn commit(&self, packet: &Packet) -> Result<u64> {
        self.commit_then(packet, || ())
    }

    pub fn commit_then(&self, packet: &Packet, committed: impl FnOnce()) -> Result<u64> {
        self.commit_span(packet, committed).map(|(_, end)| end)
    }

    pub fn commit_span(&self, packet: &Packet, committed: impl FnOnce()) -> Result<(u64, u64)> {
        let mut pending = self.pending.lock();
        Self::commit_locked(&mut pending, packet, committed)
    }

    fn commit_locked(
        pending: &mut Pending,
        packet: &Packet,
        committed: impl FnOnce(),
    ) -> Result<(u64, u64)> {
        if pending.closed {
            return Err(MqttError::NotConnected);
        }
        let start = pending.bytes.len();
        if let Err(e) = encode_packet_to_buffer(packet, &mut pending.bytes) {
            pending.bytes.truncate(start);
            return Err(e);
        }
        let begins_at = pending.committed;
        pending.committed += (pending.bytes.len() - start) as u64;
        committed();
        Ok((begins_at, pending.committed))
    }

    pub fn reached_transport(&self, offset: u64) -> bool {
        self.taken.load(Ordering::Acquire) > offset
    }

    #[cfg(all(test, feature = "transport-quic"))]
    pub fn commit_bytes(&self, bytes: &[u8]) -> Result<u64> {
        let mut pending = self.pending.lock();
        if pending.closed {
            return Err(MqttError::NotConnected);
        }
        pending.bytes.extend_from_slice(bytes);
        pending.committed += bytes.len() as u64;
        Ok(pending.committed)
    }

    pub async fn send(self: &Arc<Self>, packet: Packet) -> Result<()> {
        let end = self.commit(&packet)?;
        Flush::new(self, end).wait().await
    }

    pub fn send_detached(self: &Arc<Self>, packet: &Packet) -> Result<()> {
        let end = self.commit(packet)?;
        drop(Flush::new(self, end));
        Ok(())
    }

    fn flush_in_background(self: &Arc<Self>, end: u64) {
        self.flush_wanted.fetch_max(end, Ordering::SeqCst);
        if self.flush_queued.swap(true, Ordering::SeqCst) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.flush_queued.store(false, Ordering::Release);
            tracing::debug!("Committed packet left unflushed: no runtime");
            return;
        };
        let requests = self.background_flusher.get_or_init(|| {
            let (requests, requested) = tokio::sync::mpsc::unbounded_channel();
            #[cfg(test)]
            self.background_flushers_started
                .fetch_add(1, Ordering::SeqCst);
            runtime.spawn(Self::flush_requested_bytes(requested));
            requests
        });
        if requests.send(Arc::clone(self)).is_err() {
            self.flush_queued.store(false, Ordering::Release);
            tracing::debug!("Committed packet left unflushed: background flusher gone");
        }
    }

    async fn flush_requested_bytes(
        mut requested: tokio::sync::mpsc::UnboundedReceiver<Arc<UnifiedWriter>>,
    ) {
        while let Some(writer) = requested.recv().await {
            writer.flush_queued.store(false, Ordering::SeqCst);
            let wanted = writer.flush_wanted.load(Ordering::SeqCst);
            if let Err(e) = writer.flush_through(wanted).await {
                tracing::debug!("Committed packet not flushed: {e}");
            }
        }
    }

    pub async fn flush_through(&self, end: u64) -> Result<()> {
        let mut progress = None;
        loop {
            let handoffs = match self.claim_or_wait(end) {
                FlushTurn::Done(result) => return result,
                FlushTurn::Flush => return self.flush_as_flusher(end).await,
                FlushTurn::Wait(handoffs) => handoffs,
            };
            let progress = progress.get_or_insert_with(|| self.progress.subscribe());
            let changed = progress
                .wait_for(|p| p.flushed >= end || p.handoffs != handoffs)
                .await
                .is_ok();
            if !changed {
                return Err(MqttError::NotConnected);
            }
        }
    }

    fn claim_or_wait(&self, end: u64) -> FlushTurn {
        let mut pending = self.pending.lock();
        let seen = *self.progress.borrow();
        if seen.flushed >= end {
            FlushTurn::Done(Ok(()))
        } else if pending.flushing {
            FlushTurn::Wait(seen.handoffs)
        } else if seen.closed {
            FlushTurn::Done(Err(MqttError::NotConnected))
        } else {
            pending.flushing = true;
            FlushTurn::Flush
        }
    }

    async fn flush_as_flusher(&self, end: u64) -> Result<()> {
        let mut role = FlusherRole::claim(self);
        let mut progress = self.progress.subscribe();
        let mut transport = tokio::select! {
            biased;
            transport = self.transport.lock() => transport,
            closed = async {
                progress
                    .wait_for(|p| p.flushed >= end || p.closed)
                    .await
                    .map_or(true, |p| p.flushed < end)
            } => {
                role.release();
                self.notify_drained();
                return if closed {
                    Err(MqttError::NotConnected)
                } else {
                    Ok(())
                };
            }
        };
        let mut rounds_past_end = 0;
        let (result, drained) = loop {
            let flushed = self.record_flushed(&transport);
            if transport.unsent() == 0 {
                let mut pending = self.pending.lock();
                let done = flushed >= end
                    && (pending.bytes.is_empty() || rounds_past_end >= FLUSHER_ROUNDS_PAST_OWN_END);
                if done || pending.bytes.is_empty() {
                    let stranded = !pending.bytes.is_empty();
                    pending.flushing = false;
                    role.released(stranded);
                    break (
                        if done {
                            Ok(())
                        } else {
                            Err(MqttError::NotConnected)
                        },
                        !stranded,
                    );
                }
                self.take_pending(&mut transport, &mut pending);
                if flushed >= end {
                    rounds_past_end += 1;
                }
            }
            if let Err(e) = self.write_step(&mut transport).await {
                role.release();
                break (if self.is_flushed(end) { Ok(()) } else { Err(e) }, true);
            }
        };
        drop(transport);
        if drained || result.is_err() {
            self.notify_drained();
        }
        result
    }

    fn record_flushed(&self, transport: &Transport) -> u64 {
        let flushed = self.taken.load(Ordering::Acquire) - transport.unsent();
        self.advance_flushed(flushed);
        flushed
    }

    fn advance_flushed(&self, flushed: u64) {
        self.progress.send_if_modified(|progress| {
            let advanced = progress.flushed < flushed;
            if advanced {
                progress.flushed = flushed;
            }
            advanced
        });
    }

    fn take_pending(&self, transport: &mut Transport, pending: &mut Pending) {
        let full = pending.is_full();
        std::mem::swap(&mut pending.bytes, &mut transport.inflight);
        if full && !pending.is_full() {
            self.room.notify_waiters();
        }
        self.taken
            .fetch_add(transport.inflight.len() as u64, Ordering::AcqRel);
        transport.oversized = transport.inflight.capacity() > RETAINED_WRITE_CAPACITY;
    }

    async fn write_step(&self, transport: &mut Transport) -> Result<()> {
        let mut progress = self.progress.subscribe();
        let (written, transport_failed) = tokio::select! {
            biased;
            written = transport.write_inflight() => {
                let failed = written.is_err();
                (written, failed)
            }
            _ = progress.wait_for(|progress| progress.closed) => (Err(MqttError::NotConnected), false),
        };
        if let Err(e) = written {
            if transport_failed {
                self.progress
                    .send_modify(|progress| progress.write_failed = true);
            }
            #[cfg(feature = "transport-quic")]
            if let WriterStream::QuicControl(_, connection) = &transport.stream {
                connection.close(
                    quinn::VarInt::from_u32(mqtt5_protocol::QuicConnectionCode::Unspecified.code()),
                    b"write not completed",
                );
            }
            let unsent = transport.unsent();
            let written = self.taken.fetch_sub(unsent, Ordering::AcqRel) - unsent;
            self.advance_flushed(written);
            self.discard();
            transport.sink_pending = 0;
            #[cfg(feature = "transport-quic")]
            transport.end_quic_stream();
            transport.inflight = BytesMut::new();
            transport.stream = WriterStream::Closed;
            return Err(e);
        }
        if transport.oversized {
            transport.inflight = BytesMut::new();
            transport.oversized = false;
        }
        Ok(())
    }

    pub async fn write_failure(&self) -> MqttError {
        let mut progress = self.progress.subscribe();
        if progress
            .wait_for(|progress| progress.write_failed)
            .await
            .is_ok()
        {
            MqttError::ConnectionError("writing to the connection failed".to_string())
        } else {
            MqttError::NotConnected
        }
    }

    #[cfg(feature = "transport-quic")]
    pub fn has_write_failed(&self) -> bool {
        self.progress.borrow().write_failed
    }

    fn notify_drained(&self) {
        #[cfg(feature = "transport-quic")]
        if let Some(drained) = &self.drained {
            drained.notify_waiters();
        }
    }

    fn is_flushed(&self, end: u64) -> bool {
        self.progress.borrow().flushed >= end
    }

    async fn flush_locked(&self, transport: &mut Transport, end: u64) -> Result<()> {
        loop {
            if self.record_flushed(transport) >= end {
                return Ok(());
            }
            if transport.unsent() == 0 {
                let mut pending = self.pending.lock();
                self.take_pending(transport, &mut pending);
                drop(pending);
                if transport.inflight.is_empty() {
                    return Err(MqttError::NotConnected);
                }
            }
            self.write_step(transport).await?;
        }
    }

    #[cfg(feature = "transport-quic")]
    fn committed(&self) -> u64 {
        self.pending.lock().committed
    }

    pub fn discard(&self) {
        let mut pending = self.pending.lock();
        pending.closed = true;
        pending.bytes = BytesMut::new();
        self.progress.send_modify(|progress| progress.closed = true);
        drop(pending);
        self.room.notify_waiters();
        self.notify_drained();
    }

    pub async fn close(&self, final_packet: Option<Packet>) -> Result<()> {
        let closing = self.close_after(final_packet);
        tokio::pin!(closing);
        tokio::select! {
            closed = &mut closing => closed,
            () = tokio::time::sleep(CLOSE_GRACE) => {
                self.discard();
                tokio::time::timeout(CLOSE_GRACE, closing)
                    .await
                    .unwrap_or(Err(MqttError::Timeout))
            }
        }
    }

    async fn close_after(&self, final_packet: Option<Packet>) -> Result<()> {
        let mut transport = self.transport.lock().await;
        let failed = final_packet.as_ref().is_some_and(is_error_disconnect);
        let written = match final_packet.map(|packet| self.commit(&packet)) {
            Some(Ok(end)) => self.flush_locked(&mut transport, end).await,
            Some(Err(e)) => Err(e),
            None => Ok(()),
        };
        self.discard();
        let shutdown = transport.shutdown(failed).await;
        written.and(shutdown)
    }

    #[cfg(feature = "transport-quic")]
    pub async fn finish(&self) -> Result<()> {
        let mut transport = self.transport.lock().await;
        let flushed = self.flush_locked(&mut transport, self.committed()).await;
        self.discard();
        let shutdown = transport.shutdown(flushed.is_err()).await;
        drop(self.slot.lock().take());
        flushed.and(shutdown)
    }

    #[cfg(feature = "transport-quic")]
    pub async fn finish_or_reset(&self) -> Result<()> {
        if let Ok(finished) = tokio::time::timeout(CLOSE_GRACE, self.finish()).await {
            finished
        } else {
            self.abandon();
            Err(MqttError::Timeout)
        }
    }

    #[cfg(feature = "transport-quic")]
    pub fn try_finish_idle(&self) -> bool {
        let mut pending = self.pending.lock();
        if !pending.bytes.is_empty() || pending.flushing {
            return false;
        }
        let Ok(mut transport) = self.transport.try_lock() else {
            return false;
        };
        if transport.unsent() != 0 {
            return false;
        }
        pending.closed = true;
        self.progress.send_modify(|progress| progress.closed = true);
        transport.end_quic_stream();
        true
    }

    #[cfg(feature = "transport-quic")]
    pub fn abandon(&self) {
        self.discard();
        if let Ok(mut transport) = self.transport.try_lock() {
            transport.end_quic_stream();
        }
    }

    #[cfg(test)]
    pub(super) async fn hold_transport(&self) -> tokio::sync::MutexGuard<'_, Transport> {
        self.transport.lock().await
    }
}

impl Transport {
    fn unsent(&self) -> u64 {
        self.inflight.len() as u64 + self.sink_pending
    }

    async fn write_inflight(&mut self) -> Result<()> {
        #[cfg(feature = "transport-quic")]
        if let WriterStream::QuicUnopened(connection) = &self.stream {
            let writer = connection.open_uni().await.map_err(|e| {
                MqttError::ConnectionError(format!("Failed to open QUIC stream: {e}"))
            })?;
            self.stream = WriterStream::Quic(writer);
        }
        let inflight = &mut self.inflight;
        match &mut self.stream {
            WriterStream::Tcp(writer) => writer.write_all_buf(inflight).await?,
            WriterStream::Tls(writer) => {
                while !inflight.is_empty() {
                    let written = writer.write_buf(inflight).await?;
                    if written == 0 {
                        return Err(MqttError::from(std::io::Error::from(
                            std::io::ErrorKind::WriteZero,
                        )));
                    }
                    self.sink_pending += written as u64;
                    writer.flush().await?;
                    self.sink_pending = 0;
                }
                writer.flush().await?;
                self.sink_pending = 0;
            }
            #[cfg(feature = "transport-websocket")]
            WriterStream::WebSocket(writer) => {
                while !inflight.is_empty() {
                    writer.ready().await?;
                    let message = inflight.split_to(inflight.len().min(WEBSOCKET_MESSAGE_LIMIT));
                    self.sink_pending += message.len() as u64;
                    writer.start(message.freeze())?;
                    writer.flush().await?;
                    self.sink_pending = 0;
                }
                writer.flush().await?;
                self.sink_pending = 0;
            }
            #[cfg(feature = "transport-quic")]
            WriterStream::Quic(writer) | WriterStream::QuicControl(writer, _) => {
                write_quic(writer, &mut self.prefix).await?;
                write_quic(writer, inflight).await?;
            }
            #[cfg(feature = "transport-quic")]
            WriterStream::QuicUnopened(_) | WriterStream::Closed => {
                return Err(MqttError::NotConnected)
            }
            #[cfg(not(feature = "transport-quic"))]
            WriterStream::Closed => return Err(MqttError::NotConnected),
        }
        Ok(())
    }

    #[cfg(feature = "transport-quic")]
    fn end_quic_stream(&mut self) {
        let unsent = !self.inflight.is_empty();
        if let WriterStream::Quic(writer) = &mut self.stream {
            let ended = if unsent {
                writer
                    .reset(quinn::VarInt::from_u32(
                        mqtt5_protocol::QuicStreamCode::IncompletePacket.code(),
                    ))
                    .map_err(|e| MqttError::ConnectionError(format!("QUIC stream reset: {e}")))
            } else {
                finish_quic_stream(writer)
            };
            if let Err(e) = ended {
                tracing::debug!("QUIC data stream not ended: {e}");
            }
        }
        self.stream = WriterStream::Closed;
        self.inflight = BytesMut::new();
    }

    async fn shutdown(&mut self, failed: bool) -> Result<()> {
        tracing::debug!(failed, "Shutting down network connection");
        let ended = match &mut self.stream {
            WriterStream::Tcp(writer) => writer.shutdown().await.map_err(MqttError::from),
            WriterStream::Tls(writer) => writer.shutdown().await.map_err(MqttError::from),
            #[cfg(feature = "transport-websocket")]
            WriterStream::WebSocket(writer) => writer.close().await,
            #[cfg(feature = "transport-quic")]
            WriterStream::Quic(writer) => finish_quic_stream(writer),
            #[cfg(feature = "transport-quic")]
            WriterStream::QuicControl(writer, connection) => {
                let finished = finish_quic_stream(writer);
                if finished.is_ok()
                    && tokio::time::timeout(QUIC_DRAIN_TIMEOUT, writer.stopped())
                        .await
                        .is_err()
                {
                    tracing::debug!("QUIC control stream not acknowledged before close");
                }
                let code = if failed {
                    mqtt5_protocol::QuicConnectionCode::Unspecified
                } else {
                    mqtt5_protocol::QuicConnectionCode::NoError
                };
                connection.close(quinn::VarInt::from_u32(code.code()), b"disconnect");
                finished
            }
            #[cfg(feature = "transport-quic")]
            WriterStream::QuicUnopened(_) => Ok(()),
            WriterStream::Closed => Ok(()),
        };
        self.stream = WriterStream::Closed;
        ended
    }
}

#[cfg(feature = "transport-quic")]
async fn write_quic(writer: &mut SendStream, inflight: &mut BytesMut) -> Result<()> {
    writer
        .write_all_buf(inflight)
        .await
        .map_err(|e| MqttError::ConnectionError(format!("QUIC write error: {e}")))
}

enum FlushTurn {
    Done(Result<()>),
    Flush,
    Wait(u64),
}

pub struct RoomReservation {
    writer: Arc<UnifiedWriter>,
    size: usize,
    held: bool,
}

impl RoomReservation {
    pub fn writer(&self) -> &Arc<UnifiedWriter> {
        &self.writer
    }

    pub fn commit_then(mut self, packet: &Packet, committed: impl FnOnce()) -> Result<u64> {
        let mut pending = self.writer.pending.lock();
        let full = pending.is_full();
        pending.reserved -= self.size;
        self.held = false;
        let result = UnifiedWriter::commit_locked(&mut pending, packet, committed);
        let freed = full && !pending.is_full();
        drop(pending);
        if freed {
            self.writer.room.notify_waiters();
        }
        result.map(|(_, end)| end)
    }
}

impl Drop for RoomReservation {
    fn drop(&mut self) {
        if self.held {
            let mut pending = self.writer.pending.lock();
            let full = pending.is_full();
            pending.reserved -= self.size;
            let freed = full && !pending.is_full();
            drop(pending);
            if freed {
                self.writer.room.notify_waiters();
            }
        }
    }
}

struct FlusherRole<'a> {
    writer: &'a UnifiedWriter,
    held: bool,
}

impl<'a> FlusherRole<'a> {
    fn claim(writer: &'a UnifiedWriter) -> Self {
        #[cfg(test)]
        {
            writer.flusher_claims.fetch_add(1, Ordering::SeqCst);
            let active = writer.flushers_active.fetch_add(1, Ordering::SeqCst) + 1;
            writer.flushers_peak.fetch_max(active, Ordering::SeqCst);
        }
        Self { writer, held: true }
    }

    fn released(&mut self, stranded: bool) {
        self.held = false;
        #[cfg(test)]
        self.writer.flushers_active.fetch_sub(1, Ordering::SeqCst);
        if stranded || self.writer.progress.borrow().closed {
            self.writer
                .progress
                .send_modify(|progress| progress.handoffs += 1);
        }
    }

    fn release(&mut self) {
        if self.held {
            let mut pending = self.writer.pending.lock();
            pending.flushing = false;
            self.released(true);
        }
    }
}

impl Drop for FlusherRole<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

pub struct Flush {
    writer: Arc<UnifiedWriter>,
    end: u64,
    finished: bool,
}

impl Flush {
    pub fn new(writer: &Arc<UnifiedWriter>, end: u64) -> Self {
        Self {
            writer: Arc::clone(writer),
            end,
            finished: false,
        }
    }

    pub async fn wait(mut self) -> Result<()> {
        let flushed = self.writer.flush_through(self.end).await;
        self.finished = true;
        flushed
    }
}

impl Drop for Flush {
    fn drop(&mut self) {
        if self.finished || self.writer.is_flushed(self.end) {
            return;
        }
        self.writer.flush_in_background(self.end);
    }
}

fn is_error_disconnect(packet: &Packet) -> bool {
    matches!(packet, Packet::Disconnect(disconnect) if disconnect.reason_code.is_error())
}

#[cfg(feature = "transport-quic")]
fn finish_quic_stream(writer: &mut SendStream) -> Result<()> {
    writer
        .finish()
        .map_err(|e| MqttError::ConnectionError(format!("QUIC stream finish: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::{pin, Pin};
    use std::task::{Context, Poll, Waker};
    use tokio::net::{TcpListener, TcpStream};

    async fn loopback_writer() -> (Arc<UnifiedWriter>, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (client, accepted) = tokio::join!(TcpStream::connect(addr), listener.accept());
        let (_read_half, write_half) = client.unwrap().into_split();
        (
            Arc::new(UnifiedWriter::tcp(write_half)),
            accepted.unwrap().0,
        )
    }

    fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    #[tokio::test]
    async fn flushing_bytes_another_caller_already_wrote_does_not_take_the_transport() {
        let (writer, peer) = loopback_writer().await;
        let first = writer.commit(&Packet::PingReq).unwrap();
        let second = writer.commit(&Packet::PingReq).unwrap();
        writer.flush_through(second).await.unwrap();
        let held = writer.transport.lock().await;
        let flush = pin!(writer.flush_through(first));

        assert!(matches!(poll_once(flush), Poll::Ready(Ok(()))));
        drop(held);
        drop(peer);
    }

    #[tokio::test]
    async fn waiting_caller_returns_when_another_flusher_writes_its_bytes() {
        let (writer, peer) = loopback_writer().await;
        let end = writer.commit(&Packet::PingReq).unwrap();
        let mut held = writer.transport.lock().await;
        let mut flush = pin!(writer.flush_through(end));
        assert!(poll_once(flush.as_mut()).is_pending());
        writer.flush_locked(&mut held, end).await.unwrap();

        assert!(matches!(poll_once(flush.as_mut()), Poll::Ready(Ok(()))));
        drop(held);
        drop(peer);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn detached_packets_are_flushed_by_one_background_flusher() {
        use tokio::io::AsyncReadExt;
        let (writer, mut peer) = loopback_writer().await;
        let packets = 200;
        for _ in 0..packets {
            writer.send_detached(&Packet::PingReq).unwrap();
        }
        let mut received = vec![0u8; packets * 2];
        tokio::time::timeout(Duration::from_secs(5), peer.read_exact(&mut received))
            .await
            .unwrap()
            .unwrap();

        assert!(received.chunks(2).all(|ping| ping == [0xC0, 0x00]));
        assert_eq!(
            writer.background_flushers_started.load(Ordering::SeqCst),
            1,
            "every detached packet must be flushed by the writer's one background flusher"
        );
    }

    #[tokio::test]
    async fn background_flusher_does_not_keep_its_writer_alive() {
        let (writer, peer) = loopback_writer().await;
        writer.send_detached(&Packet::PingReq).unwrap();
        let flushed = Arc::downgrade(&writer);
        drop(writer);
        tokio::time::timeout(Duration::from_secs(5), async {
            while flushed.strong_count() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(peer);
    }

    fn large_publish() -> Packet {
        Packet::Publish(crate::packet::publish::PublishPacket::new(
            "t/large",
            vec![7u8; 8 << 20],
            crate::QoS::AtMostOnce,
        ))
    }

    async fn drain(mut peer: TcpStream) {
        use tokio::io::AsyncReadExt;
        let mut chunk = vec![0u8; 1 << 20];
        while matches!(peer.read(&mut chunk).await, Ok(read) if read > 0) {}
    }

    async fn until_bytes_are_taken(writer: &UnifiedWriter) {
        while writer.taken.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    }

    fn claims(writer: &UnifiedWriter) -> (u64, u64) {
        (
            writer.flusher_claims.load(Ordering::SeqCst),
            writer.flushers_peak.load(Ordering::SeqCst),
        )
    }

    #[tokio::test]
    async fn commit_made_while_a_flusher_writes_is_flushed_by_that_flusher() {
        let (writer, peer) = loopback_writer().await;
        let first = writer.commit(&large_publish()).unwrap();
        let flushing = Arc::clone(&writer);
        let flusher = tokio::spawn(async move { flushing.flush_through(first).await });
        until_bytes_are_taken(&writer).await;
        let second = writer.commit(&Packet::PingReq).unwrap();
        let mut waiter = pin!(writer.flush_through(second));
        assert!(poll_once(waiter.as_mut()).is_pending());
        let draining = tokio::spawn(drain(peer));

        assert!(matches!(flusher.await, Ok(Ok(()))));
        assert!(writer.is_flushed(second));
        assert!(matches!(poll_once(waiter.as_mut()), Poll::Ready(Ok(()))));
        assert_eq!(claims(&writer), (1, 1));
        draining.abort();
    }

    #[tokio::test]
    async fn cancelled_flusher_hands_the_remaining_bytes_to_exactly_one_successor() {
        let (writer, peer) = loopback_writer().await;
        let first = writer.commit(&large_publish()).unwrap();
        let mut flusher = Box::pin(writer.flush_through(first));
        while writer.taken.load(Ordering::SeqCst) == 0 {
            assert!(poll_once(flusher.as_mut()).is_pending());
        }
        let second = writer.commit(&Packet::PingReq).unwrap();
        let mut waiters = [
            Box::pin(writer.flush_through(second)),
            Box::pin(writer.flush_through(second)),
        ];
        for waiter in &mut waiters {
            assert!(poll_once(waiter.as_mut()).is_pending());
        }
        drop(flusher);
        let draining = tokio::spawn(drain(peer));
        let [one, two] = waiters;
        let (one, two) = tokio::join!(one, two);

        assert!(one.is_ok() && two.is_ok(), "{one:?} {two:?}");
        assert!(writer.is_flushed(second));
        assert_eq!(claims(&writer), (2, 1));
        draining.abort();
    }

    #[tokio::test]
    async fn bytes_of_a_failed_write_are_never_reported_as_flushed() {
        let (writer, peer) = loopback_writer().await;
        drop(peer);
        let failed_end = loop {
            let end = writer.commit(&large_publish()).unwrap();
            if writer.flush_through(end).await.is_err() {
                break end;
            }
            tokio::task::yield_now().await;
        };
        let finished = writer.finish().await;

        assert!(
            writer.flush_through(failed_end).await.is_err(),
            "a flush whose write failed reported Ok after finish returned {finished:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn waiter_deferring_to_a_flusher_stuck_on_a_stalled_link_returns_once_the_writer_is_discarded(
    ) {
        let (writer, peer) = loopback_writer().await;
        let flusher_end = writer.commit(&large_publish()).unwrap();
        let flushing = Arc::clone(&writer);
        let flusher = tokio::spawn(async move { flushing.flush_through(flusher_end).await });
        until_bytes_are_taken(&writer).await;
        let waiter_end = writer.commit(&room_packet()).unwrap();
        let waiting = Arc::clone(&writer);
        let waiter = tokio::spawn(async move { waiting.flush_through(waiter_end).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let both_stuck = !flusher.is_finished() && !waiter.is_finished();

        writer.discard();
        let waiter_outcome = tokio::time::timeout(Duration::from_secs(2), waiter).await;
        let flusher_outcome = tokio::time::timeout(Duration::from_secs(2), flusher).await;
        drop(peer);

        assert!(
            both_stuck,
            "the flusher and the waiter must be stuck on the stalled link"
        );
        assert!(
            matches!(waiter_outcome, Ok(Ok(Err(_)))),
            "the waiter deferring to the stuck flusher returned {waiter_outcome:?} after the writer was discarded"
        );
        assert!(
            matches!(flusher_outcome, Ok(Ok(Err(_)))),
            "{flusher_outcome:?}"
        );
    }

    #[tokio::test]
    async fn caller_waiting_for_room_is_woken_when_a_flusher_takes_the_buffer() {
        let (writer, peer) = loopback_writer().await;
        let draining = tokio::spawn(drain(peer));
        let mut held = writer.transport.lock().await;
        let mut end = 0;
        while let Some(room) = writer.try_reserve_room(ROOM_PACKET) {
            end = room.commit_then(&room_packet(), || ()).unwrap();
        }
        let mut waiting = pin!(writer.reserve_room(ROOM_PACKET));
        assert!(poll_once(waiting.as_mut()).is_pending());
        writer.flush_locked(&mut held, end).await.unwrap();

        assert!(poll_once(waiting.as_mut()).is_ready());
        drop(held);
        draining.abort();
    }

    const ROOM_PACKET: usize = 64 * 1024;

    fn room_packet() -> Packet {
        Packet::Publish(crate::packet::publish::PublishPacket::new(
            "t/room",
            vec![3u8; ROOM_PACKET],
            crate::QoS::AtMostOnce,
        ))
    }

    #[tokio::test]
    async fn uncommitted_reservations_hold_room_until_they_commit_or_are_dropped() {
        let (writer, peer) = loopback_writer().await;
        let mut admitted = Vec::new();
        while let Some(room) = writer.try_reserve_room(ROOM_PACKET) {
            admitted.push(room);
        }
        assert_eq!(admitted.len(), PENDING_LIMIT / ROOM_PACKET);
        let mut waiting = pin!(writer.reserve_room(ROOM_PACKET));
        assert!(poll_once(waiting.as_mut()).is_pending());
        admitted.pop();

        assert!(poll_once(waiting.as_mut()).is_ready());
        drop(peer);
    }

    fn small_publish(topic: &str) -> (Packet, usize) {
        let publish = crate::packet::publish::PublishPacket::new(
            topic,
            vec![1u8; 10],
            crate::QoS::AtMostOnce,
        );
        let estimate = publish.body_encoded_size() + 5;
        (Packet::Publish(publish), estimate)
    }

    async fn writer_filled_by_a_reservation(
        estimate: usize,
    ) -> (Arc<UnifiedWriter>, TcpStream, RoomReservation) {
        let (writer, peer) = loopback_writer().await;
        writer
            .commit_bytes(&vec![0u8; PENDING_LIMIT - estimate])
            .unwrap();
        let reservation = writer.try_reserve_room(estimate).unwrap();
        (writer, peer, reservation)
    }

    #[tokio::test]
    async fn caller_waiting_for_room_is_woken_when_a_commit_brings_the_buffer_under_the_limit() {
        let (packet, estimate) = small_publish("t");
        let (writer, peer, reservation) = writer_filled_by_a_reservation(estimate).await;
        let mut waiting = pin!(writer.reserve_room(1));
        assert!(poll_once(waiting.as_mut()).is_pending());

        reservation.commit_then(&packet, || ()).unwrap();

        let load = {
            let pending = writer.pending.lock();
            pending.bytes.len() + pending.reserved
        };
        assert!(load < PENDING_LIMIT, "{load} bytes still pending");
        assert!(
            poll_once(waiting.as_mut()).is_ready(),
            "{load} of {PENDING_LIMIT} bytes pending after the commit, but the caller waiting for room was not woken"
        );
        drop(peer);
    }

    #[tokio::test]
    async fn caller_waiting_for_room_is_woken_when_a_commit_fails_to_encode() {
        let (packet, estimate) = small_publish("t\0");
        let (writer, peer, reservation) = writer_filled_by_a_reservation(estimate).await;
        let mut waiting = pin!(writer.reserve_room(1));
        assert!(poll_once(waiting.as_mut()).is_pending());

        assert!(reservation.commit_then(&packet, || ()).is_err());

        assert!(
            poll_once(waiting.as_mut()).is_ready(),
            "the failed commit released its room, but the caller waiting for room was not woken"
        );
        drop(peer);
    }

    #[tokio::test]
    async fn caller_waiting_for_room_is_not_woken_while_the_buffer_stays_full() {
        let (writer, peer) = loopback_writer().await;
        let mut admitted = Vec::new();
        while let Some(room) = writer.try_reserve_room(ROOM_PACKET) {
            admitted.push(room);
        }
        writer.commit_bytes(&vec![0u8; ROOM_PACKET]).unwrap();
        let mut waiting = pin!(writer.reserve_room(ROOM_PACKET));
        assert!(poll_once(waiting.as_mut()).is_pending());
        admitted.pop();

        assert!(poll_once(waiting.as_mut()).is_pending());
        admitted.pop();
        assert!(poll_once(waiting.as_mut()).is_ready());
        drop(peer);
    }

    async fn tls_writer_pair() -> (
        Arc<UnifiedWriter>,
        crate::transport::tls::TlsReadHalf,
        tokio_rustls::server::TlsStream<TcpStream>,
    ) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
        let key =
            rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![der.clone()], key)
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(der).unwrap();
        let client_config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server_socket = tokio::net::TcpSocket::new_v4().unwrap();
        server_socket.set_recv_buffer_size(4096).unwrap();
        server_socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = server_socket.local_addr().unwrap();
        let listener = server_socket.listen(1).unwrap();
        let client_socket = tokio::net::TcpSocket::new_v4().unwrap();
        client_socket.set_send_buffer_size(4096).unwrap();
        let (client, accepted) = tokio::join!(client_socket.connect(addr), listener.accept());
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let (client_tls, server_tls) = tokio::join!(
            connector.connect(name, client.unwrap()),
            acceptor.accept(accepted.unwrap().0)
        );
        let (read_half, write_half) = tokio::io::split(client_tls.unwrap());
        (
            Arc::new(UnifiedWriter::tls(write_half)),
            read_half,
            server_tls.unwrap(),
        )
    }

    fn kib_publish() -> Packet {
        Packet::Publish(crate::packet::publish::PublishPacket::new(
            "t/k",
            vec![5u8; 1024],
            crate::QoS::AtMostOnce,
        ))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tls_flush_waiter_is_not_told_ok_after_the_flusher_is_cancelled_mid_tls_flush() {
        use tokio::io::AsyncReadExt;
        let (writer, _read_half, mut server) = tls_writer_pair().await;
        loop {
            let end = writer.commit(&kib_publish()).unwrap();
            if tokio::time::timeout(Duration::from_millis(100), writer.flush_through(end))
                .await
                .is_err()
            {
                break;
            }
        }
        writer.commit(&kib_publish()).unwrap();
        let waiter_end = writer.commit(&kib_publish()).unwrap();
        let mut flusher = Box::pin(writer.flush_through(waiter_end - 1));
        for _ in 0..20 {
            let _ = poll_once(flusher.as_mut());
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let taken_by_flusher = writer.taken.load(Ordering::SeqCst) >= waiter_end;
        let mut waiter = Box::pin(writer.flush_through(waiter_end));
        let _ = poll_once(waiter.as_mut());
        drop(flusher);

        let reading = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let mut received = 0u64;
            let mut chunk = vec![0u8; 1 << 16];
            while let Ok(Ok(read)) =
                tokio::time::timeout(Duration::from_secs(1), server.read(&mut chunk)).await
            {
                if read == 0 {
                    break;
                }
                received += read as u64;
            }
            received
        });
        let outcome = tokio::time::timeout(Duration::from_secs(10), waiter).await;
        let received = reading.await.unwrap();

        assert!(matches!(outcome, Ok(Ok(()))), "{outcome:?}");
        assert!(
            received >= waiter_end,
            "the flush returned Ok, yet the server received only {received} of the {waiter_end} bytes it covers and no more arrived (cancelled flusher had taken the waiter's bytes: {taken_by_flusher})"
        );
    }
}
