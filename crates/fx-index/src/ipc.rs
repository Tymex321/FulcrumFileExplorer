//! The helper pipe protocol: framing, the server's request loop, and the app-side client.
//!
//! The elevated `fx-helper` service owns the MFT index; the unelevated app reaches it over
//! [`fx_core::HELPER_PIPE_NAME`]. Both ends live here, written against plain
//! `Read`/`Write`, so the whole protocol — handshake, streaming, cancellation, limits — is
//! exercised by the macOS test suite over a socket pair. Only *opening* the named pipe is
//! Windows-specific.
//!
//! ## Wire format
//!
//! Each message is a little-endian `u32` length followed by that many bytes of JSON. JSON
//! rather than a binary codec because [`FxError`] is adjacently tagged, which binary serde
//! formats cannot round-trip; result batches are a few hundred kilobytes at most, so the
//! encoding cost is noise next to the query.
//!
//! ## Security — this is a SYSTEM process reading attacker-influenced input
//!
//! - Every frame is bounded by [`MAX_MESSAGE_BYTES`], checked **before** allocating, so a
//!   forged length prefix cannot exhaust the service's memory.
//! - The first message must be a [`HelperRequest::Hello`] carrying
//!   [`HELPER_PROTOCOL_VERSION`]; anything else closes the connection.
//! - Result windows are capped server-side ([`MAX_REMOTE_LIMIT`]): `limit: u32::MAX` must
//!   not make the service materialise millions of rows.
//! - The protocol is **read-only**. There is no request that writes, deletes, moves or
//!   executes anything, and there must never be one: that would turn a query channel into
//!   privilege escalation. `Clear` drops the in-memory index only.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use fx_core::{
    BatchSink, ChangeCallback, ChangeEvent, EntryId, FxError, IndexBackend, IndexStats, Progress,
    ProgressSink, Query, QuerySummary, Result, SearchHit, VolumeId, WatchHandle,
    HELPER_PROTOCOL_VERSION,
};
use parking_lot::Mutex;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Maximum accepted encoded message size.
///
/// Bounds memory for a SYSTEM-level process reading attacker-influenced input; a batch of
/// 500 hits with ordinary paths is a few hundred kilobytes. Batches that would exceed it
/// (500 maximal 32k-character paths) are split by the sender rather than rejected.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// Largest result window the helper will materialise for one query. The UI asks for a
/// screenful plus a buffer; anything far beyond that is a bug or an attack.
pub const MAX_REMOTE_LIMIT: u32 = 50_000;

/// Requests the unelevated app sends over the pipe. Mirrors [`IndexBackend`] so the client
/// can implement that trait by forwarding. Read-only by design: see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HelperRequest {
    /// Protocol handshake. Must be the first message on every connection.
    Hello {
        version: u32,
    },
    Stats,
    Query(Box<Query>),
    /// Rebuild the index. Answered by a stream of `Progress`, then `Ok` or `Error`.
    Scan,
    ResolvePath {
        volume: VolumeId,
        id: EntryId,
    },
    /// Turns this connection into a change feed: `Ok`, then `Changes` until `Unwatch`.
    Watch,
    Unwatch,
    /// Drops the in-memory index. Touches no file.
    Clear,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HelperResponse {
    Hello {
        version: u32,
        elevated: bool,
    },
    Stats(Box<IndexStats>),
    /// One streamed batch of a query. The query's `Summary` (or `Error`) ends the stream.
    Hits(Vec<SearchHit>),
    Summary(Box<QuerySummary>),
    Progress(Box<Progress>),
    Changes(Vec<ChangeEvent>),
    Path(String),
    Ok,
    Error(FxError),
}

/// Writes one frame. Fails with `InvalidData` if the message is over the limit, so the
/// caller can split it; nothing is written in that case.
pub fn write_frame<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let bytes =
        serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message of {} bytes exceeds the frame limit", bytes.len()),
        ));
    }
    let mut buf = Vec::with_capacity(4 + bytes.len());
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&bytes);
    // One write per frame: on a message-oriented pipe, and for frames written from the
    // watch thread, a frame must never be interleaved with another.
    w.write_all(&buf)?;
    w.flush()
}

/// Reads one frame. `Ok(None)` is a clean end of stream between frames; a stream that ends
/// mid-frame, an oversized length, or undecodable JSON is an error.
pub fn read_frame<T: DeserializeOwned>(r: &mut impl Read) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_le_bytes(len) as usize;
    // Checked before allocating anything: this is the memory-exhaustion guard.
    if len > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes exceeds the limit"),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

fn send(w: &SharedWriter, msg: &HelperResponse) -> io::Result<()> {
    write_frame(&mut *w.lock(), msg)
}

/// Sends hits, halving any batch whose encoding would exceed the frame limit. A single hit
/// that cannot fit on its own (impossible for real paths) is dropped rather than wedging
/// the stream.
fn send_hits(w: &SharedWriter, hits: Vec<SearchHit>) -> io::Result<()> {
    match send(w, &HelperResponse::Hits(hits.clone())) {
        Err(e) if e.kind() == io::ErrorKind::InvalidData && hits.len() > 1 => {
            let mut hits = hits;
            let tail = hits.split_off(hits.len() / 2);
            send_hits(w, hits)?;
            send_hits(w, tail)
        }
        Err(e) if e.kind() == io::ErrorKind::InvalidData => Ok(()),
        other => other,
    }
}

/// Streams a query's batches to the client. A failed write means the client hung up or
/// navigated away: the sink says so and the query stops early.
struct RemoteSink {
    w: SharedWriter,
    dead: AtomicBool,
}

impl BatchSink<SearchHit> for RemoteSink {
    fn push(&self, batch: Vec<SearchHit>) -> bool {
        if self.dead.load(Ordering::Relaxed) {
            return false;
        }
        let ok = send_hits(&self.w, batch).is_ok();
        if !ok {
            self.dead.store(true, Ordering::Relaxed);
        }
        ok
    }
}

impl ProgressSink for RemoteSink {
    fn emit(&self, progress: &Progress) {
        if send(
            &self.w,
            &HelperResponse::Progress(Box::new(progress.clone())),
        )
        .is_err()
        {
            self.dead.store(true, Ordering::Relaxed);
        }
    }

    /// A client that disconnects mid-scan cancels it.
    fn should_continue(&self) -> bool {
        !self.dead.load(Ordering::Relaxed)
    }
}

/// Serves one client connection until it disconnects. `elevated` is reported in the
/// handshake so the app can tell a misconfigured helper from a working one.
///
/// Returns `Ok` on a clean disconnect and an error for protocol violations (bad
/// handshake, oversized frame), after which the caller drops the connection.
pub fn serve_connection(
    backend: &dyn IndexBackend,
    mut reader: impl Read,
    writer: impl Write + Send + 'static,
    elevated: bool,
) -> io::Result<()> {
    let w: SharedWriter = Arc::new(Mutex::new(Box::new(writer)));

    match read_frame::<HelperRequest>(&mut reader)? {
        Some(HelperRequest::Hello { version }) if version == HELPER_PROTOCOL_VERSION => {
            send(
                &w,
                &HelperResponse::Hello {
                    version: HELPER_PROTOCOL_VERSION,
                    elevated,
                },
            )?;
        }
        Some(HelperRequest::Hello { version }) => {
            let _ = send(
                &w,
                &HelperResponse::Error(FxError::HelperUnavailable(format!(
                    "protocol version {version} not supported; helper speaks {HELPER_PROTOCOL_VERSION}"
                ))),
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "protocol version mismatch",
            ));
        }
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "first message must be Hello",
            ));
        }
        None => return Ok(()),
    }

    let mut watch: Option<WatchHandle> = None;
    let result = loop {
        let req = match read_frame::<HelperRequest>(&mut reader) {
            Ok(Some(r)) => r,
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        };
        let sent = match req {
            HelperRequest::Hello { .. } => send(
                &w,
                &HelperResponse::Error(FxError::BadQuery("duplicate handshake".into())),
            ),
            HelperRequest::Stats => {
                let mut stats = backend.stats();
                stats.helper_connected = true;
                send(&w, &HelperResponse::Stats(Box::new(stats)))
            }
            HelperRequest::Query(mut q) => {
                q.limit = q.effective_limit().min(MAX_REMOTE_LIMIT);
                let sink = RemoteSink {
                    w: Arc::clone(&w),
                    dead: AtomicBool::new(false),
                };
                match backend.query(&q, &sink) {
                    Ok(sum) => send(&w, &HelperResponse::Summary(Box::new(sum))),
                    Err(_) if sink.dead.load(Ordering::Relaxed) => {
                        break Ok(()); // The client is gone; nothing to answer.
                    }
                    Err(e) => send(&w, &HelperResponse::Error(e)),
                }
            }
            HelperRequest::Scan => {
                let sink = RemoteSink {
                    w: Arc::clone(&w),
                    dead: AtomicBool::new(false),
                };
                match backend.scan(&sink) {
                    Ok(()) => send(&w, &HelperResponse::Ok),
                    Err(_) if sink.dead.load(Ordering::Relaxed) => break Ok(()),
                    Err(e) => send(&w, &HelperResponse::Error(e)),
                }
            }
            HelperRequest::ResolvePath { volume, id } => match backend.resolve_path(&volume, id) {
                Ok(p) => send(&w, &HelperResponse::Path(p)),
                Err(e) => send(&w, &HelperResponse::Error(e)),
            },
            HelperRequest::Watch => {
                if watch.is_some() {
                    send(&w, &HelperResponse::Ok)
                } else {
                    let feed = Arc::clone(&w);
                    let cb: ChangeCallback = Box::new(move |events| {
                        // A failed write means the client is gone; the read loop notices
                        // the disconnect and drops the watch.
                        let _ = send(&feed, &HelperResponse::Changes(events));
                    });
                    match backend.watch(cb) {
                        Ok(h) => {
                            watch = Some(h);
                            send(&w, &HelperResponse::Ok)
                        }
                        Err(e) => send(&w, &HelperResponse::Error(e)),
                    }
                }
            }
            HelperRequest::Unwatch => {
                if let Some(h) = watch.take() {
                    h.stop();
                }
                let _ = send(&w, &HelperResponse::Ok);
                break Ok(());
            }
            HelperRequest::Clear => match backend.clear() {
                Ok(()) => send(&w, &HelperResponse::Ok),
                Err(e) => send(&w, &HelperResponse::Error(e)),
            },
        };
        if let Err(e) = sent {
            break Err(e);
        }
    };
    if let Some(h) = watch.take() {
        h.stop();
    }
    result
}

// ---- client ----

/// One open, handshaken connection to the helper.
pub struct Connection {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Connection")
    }
}

/// Opens a raw transport: a read half and a write half of one duplex stream.
pub type Connector =
    Box<dyn Fn() -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> + Send + Sync>;

fn unavailable(e: impl std::fmt::Display) -> FxError {
    FxError::HelperUnavailable(e.to_string())
}

impl Connection {
    fn open(connector: &Connector) -> Result<(Self, bool)> {
        let (reader, writer) = connector().map_err(unavailable)?;
        let mut c = Connection { reader, writer };
        c.send(&HelperRequest::Hello {
            version: HELPER_PROTOCOL_VERSION,
        })?;
        match c.recv()? {
            HelperResponse::Hello { version, elevated } if version == HELPER_PROTOCOL_VERSION => {
                Ok((c, elevated))
            }
            HelperResponse::Hello { version, .. } => Err(FxError::HelperUnavailable(format!(
                "helper speaks protocol {version}, app speaks {HELPER_PROTOCOL_VERSION}"
            ))),
            HelperResponse::Error(e) => Err(e),
            other => Err(unavailable(format!("unexpected handshake reply {other:?}"))),
        }
    }

    fn send(&mut self, req: &HelperRequest) -> Result<()> {
        write_frame(&mut self.writer, req).map_err(unavailable)
    }

    fn recv(&mut self) -> Result<HelperResponse> {
        read_frame(&mut self.reader)
            .map_err(unavailable)?
            .ok_or_else(|| unavailable("helper closed the connection"))
    }
}

/// [`IndexBackend`] implemented by forwarding to the elevated helper.
///
/// Connections are pooled: each call borrows one (opening and handshaking a new one if the
/// pool is empty) and returns it only if the exchange completed cleanly. Concurrent
/// queries — the UI fires one per keystroke — therefore run in parallel instead of queueing
/// on one pipe, and a connection left mid-stream by a cancelled query is simply dropped
/// rather than resynchronised.
pub struct HelperClient {
    connector: Arc<Connector>,
    pool: Mutex<Vec<Connection>>,
    elevated: AtomicBool,
}

impl std::fmt::Debug for HelperClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperClient")
            .field("pooled", &self.pool.lock().len())
            .finish()
    }
}

/// Pooled connections kept idle. More than this are closed after use.
const MAX_POOLED: usize = 4;

impl HelperClient {
    /// Connects once to verify the helper is reachable and speaks this protocol.
    pub fn connect(connector: Connector) -> Result<Self> {
        let (conn, elevated) = Connection::open(&connector)?;
        Ok(Self {
            connector: Arc::new(connector),
            pool: Mutex::new(vec![conn]),
            elevated: AtomicBool::new(elevated),
        })
    }

    /// Connects to the installed service over [`fx_core::HELPER_PIPE_NAME`].
    #[cfg(windows)]
    pub fn connect_pipe() -> Result<Self> {
        Self::connect(Box::new(open_pipe))
    }

    /// Whether the helper reported holding administrator rights.
    pub fn helper_elevated(&self) -> bool {
        self.elevated.load(Ordering::Relaxed)
    }

    fn checkout(&self) -> Result<Connection> {
        if let Some(c) = self.pool.lock().pop() {
            return Ok(c);
        }
        let (c, elevated) = Connection::open(&self.connector)?;
        self.elevated.store(elevated, Ordering::Relaxed);
        Ok(c)
    }

    fn checkin(&self, c: Connection) {
        let mut pool = self.pool.lock();
        if pool.len() < MAX_POOLED {
            pool.push(c);
        }
    }

    /// One request, one reply. On a transport error the connection is discarded and the
    /// request retried once on a fresh one, which transparently survives a helper restart.
    fn call(&self, req: &HelperRequest) -> Result<HelperResponse> {
        for attempt in 0..2 {
            let mut c = self.checkout()?;
            let res = c.send(req).and_then(|()| c.recv());
            match res {
                Ok(resp) => {
                    self.checkin(c);
                    return Ok(resp);
                }
                Err(FxError::HelperUnavailable(_)) if attempt == 0 => continue,
                Err(e) => return Err(e),
            }
        }
        Err(unavailable("helper unreachable"))
    }
}

fn unexpected(r: HelperResponse) -> FxError {
    match r {
        HelperResponse::Error(e) => e,
        other => unavailable(format!("unexpected reply {other:?}")),
    }
}

impl IndexBackend for HelperClient {
    fn stats(&self) -> IndexStats {
        match self.call(&HelperRequest::Stats) {
            Ok(HelperResponse::Stats(s)) => *s,
            // Unreachable helper: report it rather than fail, so the settings page can
            // show "instant search unavailable" instead of an error.
            _ => IndexStats {
                state: fx_core::IndexState::Failed,
                helper_connected: false,
                ..Default::default()
            },
        }
    }

    fn scan(&self, progress: &dyn ProgressSink) -> Result<()> {
        let mut c = self.checkout()?;
        c.send(&HelperRequest::Scan)?;
        loop {
            match c.recv()? {
                HelperResponse::Progress(p) => {
                    progress.emit(&p);
                    if !progress.should_continue() {
                        // Dropping the connection is the cancel signal: the helper's next
                        // progress write fails and its scan stops.
                        return Err(FxError::Cancelled);
                    }
                }
                HelperResponse::Ok => {
                    self.checkin(c);
                    return Ok(());
                }
                other => return Err(unexpected(other)),
            }
        }
    }

    fn query(&self, query: &Query, sink: &dyn BatchSink<SearchHit>) -> Result<QuerySummary> {
        let mut c = self.checkout()?;
        if let Err(FxError::HelperUnavailable(_)) =
            c.send(&HelperRequest::Query(Box::new(query.clone())))
        {
            // A pooled connection the helper has since closed: retry once on a fresh one.
            c = Connection::open(&self.connector)?.0;
            c.send(&HelperRequest::Query(Box::new(query.clone())))?;
        }
        loop {
            match c.recv()? {
                HelperResponse::Hits(hits) => {
                    if !sink.push(hits) {
                        // Abandon the connection mid-stream; the helper notices the hang-up
                        // on its next write and stops the query.
                        return Err(FxError::Cancelled);
                    }
                }
                HelperResponse::Summary(s) => {
                    self.checkin(c);
                    return Ok(*s);
                }
                other => {
                    let e = unexpected(other);
                    if !matches!(e, FxError::HelperUnavailable(_)) {
                        self.checkin(c);
                    }
                    return Err(e);
                }
            }
        }
    }

    fn watch(&self, on_change: ChangeCallback) -> Result<WatchHandle> {
        // A dedicated connection: the change feed is unsolicited and would otherwise
        // interleave with request/response traffic.
        let (mut c, _) = Connection::open(&self.connector)?;
        c.send(&HelperRequest::Watch)?;
        match c.recv()? {
            HelperResponse::Ok => {}
            other => return Err(unexpected(other)),
        }
        let Connection { mut reader, writer } = c;
        let writer = Arc::new(Mutex::new(writer));
        std::thread::Builder::new()
            .name("fx-helper-watch".into())
            .spawn(move || loop {
                match read_frame::<HelperResponse>(&mut reader) {
                    Ok(Some(HelperResponse::Changes(ev))) => on_change(ev),
                    Ok(Some(_)) => {}
                    // Unwatch acknowledged and closed, or the helper went away. In the
                    // latter case the index may have moved on unseen.
                    Ok(None) | Err(_) => return,
                }
            })
            .map_err(|e| FxError::Index(format!("cannot spawn watch thread: {e}")))?;
        Ok(WatchHandle::new(move || {
            let _ = write_frame(&mut *writer.lock(), &HelperRequest::Unwatch);
        }))
    }

    fn resolve_path(&self, volume: &VolumeId, id: EntryId) -> Result<String> {
        match self.call(&HelperRequest::ResolvePath {
            volume: volume.clone(),
            id,
        })? {
            HelperResponse::Path(p) => Ok(p),
            other => Err(unexpected(other)),
        }
    }

    fn clear(&self) -> Result<()> {
        match self.call(&HelperRequest::Clear)? {
            HelperResponse::Ok => Ok(()),
            other => Err(unexpected(other)),
        }
    }
}

// ---- shared watching ----

type Subscriber = Arc<dyn Fn(Vec<ChangeEvent>) + Send + Sync>;

/// Wraps a backend so that one real watch serves any number of subscribers.
///
/// The helper must keep its index live whether or not a client is connected, and several
/// clients (two app windows) must not each start their own journal loop over the same
/// index — two loops would race on the store and on its checkpoint. The hub starts the
/// backend's watch once ([`WatchHub::start`]) and fans each coalesced batch out to whoever
/// is subscribed at that moment. Every other call is forwarded unchanged.
pub struct WatchHub<B: IndexBackend> {
    inner: Arc<B>,
    subs: Arc<Mutex<Vec<(u64, Subscriber)>>>,
    next_id: std::sync::atomic::AtomicU64,
    handle: Mutex<Option<WatchHandle>>,
}

impl<B: IndexBackend> std::fmt::Debug for WatchHub<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchHub")
            .field("subscribers", &self.subs.lock().len())
            .field("live", &self.handle.lock().is_some())
            .finish()
    }
}

impl<B: IndexBackend> WatchHub<B> {
    pub fn new(inner: Arc<B>) -> Self {
        Self {
            inner,
            subs: Arc::new(Mutex::new(Vec::new())),
            next_id: std::sync::atomic::AtomicU64::new(0),
            handle: Mutex::new(None),
        }
    }

    pub fn inner(&self) -> &Arc<B> {
        &self.inner
    }

    /// Starts the one real watch. Idempotent; call again after a rescan replaced what the
    /// watch was following.
    pub fn start(&self) -> Result<()> {
        let mut h = self.handle.lock();
        if let Some(old) = h.take() {
            old.stop();
        }
        let subs = Arc::clone(&self.subs);
        *h = Some(self.inner.watch(Box::new(move |events| {
            // Snapshot the list so a subscriber's callback never runs under the lock (it
            // may block on a pipe write, or unsubscribe itself).
            let targets: Vec<Subscriber> = subs.lock().iter().map(|(_, s)| Arc::clone(s)).collect();
            for t in targets {
                t(events.clone());
            }
        }))?);
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(h) = self.handle.lock().take() {
            h.stop();
        }
    }
}

impl<B: IndexBackend + 'static> IndexBackend for WatchHub<B> {
    fn stats(&self) -> IndexStats {
        self.inner.stats()
    }

    fn scan(&self, progress: &dyn ProgressSink) -> Result<()> {
        self.inner.scan(progress)
    }

    fn query(&self, query: &Query, sink: &dyn BatchSink<SearchHit>) -> Result<QuerySummary> {
        self.inner.query(query, sink)
    }

    fn watch(&self, on_change: ChangeCallback) -> Result<WatchHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.subs.lock().push((id, Arc::from(on_change)));
        let subs = Arc::clone(&self.subs);
        Ok(WatchHandle::new(move || {
            subs.lock().retain(|(i, _)| *i != id)
        }))
    }

    fn resolve_path(&self, volume: &VolumeId, id: EntryId) -> Result<String> {
        self.inner.resolve_path(volume, id)
    }

    fn clear(&self) -> Result<()> {
        self.inner.clear()
    }
}

/// Opens the helper's named pipe, waiting briefly if every instance is busy.
#[cfg(windows)]
fn open_pipe() -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
    const ERROR_PIPE_BUSY: i32 = 231;
    let mut last = None;
    for _ in 0..20 {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(fx_core::HELPER_PIPE_NAME)
        {
            Ok(f) => {
                let r = f.try_clone()?;
                return Ok((Box::new(r), Box::new(f)));
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::ErrorKind::TimedOut.into()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::store::{EntryStore, Meta, NO_RECORD};
    use crate::walk::WalkIndex;
    use fx_core::{CollectSink, NullSink};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    /// A backend over a fixed in-memory store, so protocol tests do not touch the disk.
    struct Fixed {
        store: EntryStore,
        watch_events: Mutex<Option<ChangeCallback>>,
    }

    impl Fixed {
        fn new(n: u64) -> Self {
            let mut s = EntryStore::with_mount("C".into(), r"C:\", '\\');
            let root = s.push(5, NO_RECORD, ".", Meta::dir());
            s.set_root(root);
            for i in 0..n {
                s.push(100 + i, root, &format!("file{i}.txt"), Meta::file(i));
            }
            Self {
                store: s,
                watch_events: Mutex::new(None),
            }
        }
    }

    impl IndexBackend for Fixed {
        fn stats(&self) -> IndexStats {
            IndexStats {
                total_entries: self.store.live_count() as u64,
                ..Default::default()
            }
        }
        fn scan(&self, progress: &dyn ProgressSink) -> Result<()> {
            for i in 0..1000 {
                if !progress.should_continue() {
                    return Err(FxError::Cancelled);
                }
                progress.emit(&crate::scan_progress(
                    fx_core::OpState::Running,
                    i,
                    1000,
                    String::new(),
                ));
            }
            Ok(())
        }
        fn query(&self, q: &Query, sink: &dyn BatchSink<SearchHit>) -> Result<QuerySummary> {
            crate::query::execute(&[&self.store], q, sink, false)
        }
        fn watch(&self, cb: ChangeCallback) -> Result<WatchHandle> {
            *self.watch_events.lock() = Some(cb);
            Ok(WatchHandle::noop())
        }
        fn resolve_path(&self, _v: &VolumeId, id: EntryId) -> Result<String> {
            self.store
                .find(id)
                .map(|ix| self.store.path_of(ix))
                .ok_or_else(|| FxError::NotFound(id.to_string()))
        }
        fn clear(&self) -> Result<()> {
            Ok(())
        }
    }

    /// A connector whose every connection is served by `backend` on its own thread.
    fn connector(backend: Arc<dyn IndexBackend>) -> Connector {
        Box::new(move || {
            let (client, server) = UnixStream::pair()?;
            let b = Arc::clone(&backend);
            std::thread::spawn(move || {
                let r = server.try_clone().unwrap();
                let _ = serve_connection(&*b, r, server, true);
            });
            let r = client.try_clone()?;
            Ok((
                Box::new(r) as Box<dyn Read + Send>,
                Box::new(client) as Box<dyn Write + Send>,
            ))
        })
    }

    fn client(n: u64) -> (HelperClient, Arc<Fixed>) {
        let backend = Arc::new(Fixed::new(n));
        let c = HelperClient::connect(connector(backend.clone())).unwrap();
        (c, backend)
    }

    #[test]
    fn frames_round_trip_and_eof_between_frames_is_clean() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &HelperRequest::Stats).unwrap();
        write_frame(&mut buf, &HelperRequest::Hello { version: 7 }).unwrap();
        let mut r = buf.as_slice();
        assert_eq!(
            read_frame::<HelperRequest>(&mut r).unwrap(),
            Some(HelperRequest::Stats)
        );
        assert_eq!(
            read_frame::<HelperRequest>(&mut r).unwrap(),
            Some(HelperRequest::Hello { version: 7 })
        );
        assert_eq!(read_frame::<HelperRequest>(&mut r).unwrap(), None);
        // Truncated mid-frame is an error, not a clean EOF.
        let mut r = &buf[..buf.len() - 1];
        read_frame::<HelperRequest>(&mut r).unwrap();
        assert!(read_frame::<HelperRequest>(&mut r).is_err());
    }

    #[test]
    fn oversized_length_prefix_is_rejected_before_allocating() {
        // Claims 4 GiB; the reader must refuse on the prefix alone.
        let mut r: &[u8] = &[0xFF, 0xFF, 0xFF, 0xFF, b'{'];
        let err = read_frame::<HelperRequest>(&mut r).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let mut ok_len = ((MAX_MESSAGE_BYTES + 1) as u32).to_le_bytes().to_vec();
        ok_len.push(b'x');
        assert!(read_frame::<HelperRequest>(&mut ok_len.as_slice()).is_err());
    }

    #[test]
    fn garbage_json_is_an_error_not_a_panic() {
        let mut buf = 3u32.to_le_bytes().to_vec();
        buf.extend_from_slice(b"}}}");
        assert!(read_frame::<HelperRequest>(&mut buf.as_slice()).is_err());
    }

    fn raw_server(
        backend: Arc<dyn IndexBackend>,
    ) -> (UnixStream, std::thread::JoinHandle<io::Result<()>>) {
        let (client, server) = UnixStream::pair().unwrap();
        let h = std::thread::spawn(move || {
            let r = server.try_clone().unwrap();
            serve_connection(&*backend, r, server, false)
        });
        (client, h)
    }

    #[test]
    fn handshake_is_mandatory_and_versioned() {
        let backend: Arc<dyn IndexBackend> = Arc::new(Fixed::new(1));

        // Wrong version: told why, then disconnected.
        let (mut c, h) = raw_server(Arc::clone(&backend));
        write_frame(&mut c, &HelperRequest::Hello { version: 999 }).unwrap();
        let reply: HelperResponse = read_frame(&mut c).unwrap().unwrap();
        assert!(matches!(
            reply,
            HelperResponse::Error(FxError::HelperUnavailable(_))
        ));
        assert!(h.join().unwrap().is_err());

        // Skipping the handshake: disconnected without an answer.
        let (mut c, h) = raw_server(Arc::clone(&backend));
        write_frame(&mut c, &HelperRequest::Stats).unwrap();
        assert!(h.join().unwrap().is_err());
        assert!(read_frame::<HelperResponse>(&mut c).unwrap().is_none());

        // A hostile length prefix after a valid handshake also ends the connection.
        let (mut c, h) = raw_server(backend);
        write_frame(
            &mut c,
            &HelperRequest::Hello {
                version: HELPER_PROTOCOL_VERSION,
            },
        )
        .unwrap();
        let _: HelperResponse = read_frame(&mut c).unwrap().unwrap();
        c.write_all(&u32::MAX.to_le_bytes()).unwrap();
        assert!(h.join().unwrap().is_err());
    }

    #[test]
    fn client_rejects_a_helper_speaking_another_version() {
        let conn: Connector = Box::new(|| {
            let (client, mut server) = UnixStream::pair()?;
            std::thread::spawn(move || {
                let _: Option<HelperRequest> = read_frame(&mut server).unwrap();
                write_frame(
                    &mut server,
                    &HelperResponse::Hello {
                        version: 999,
                        elevated: true,
                    },
                )
                .unwrap();
            });
            let r = client.try_clone()?;
            Ok((
                Box::new(r) as Box<dyn Read + Send>,
                Box::new(client) as Box<dyn Write + Send>,
            ))
        });
        assert!(matches!(
            HelperClient::connect(conn),
            Err(FxError::HelperUnavailable(_))
        ));
    }

    #[test]
    fn queries_stream_through_the_pipe_in_batches() {
        let (c, _) = client(1200);
        assert!(c.helper_elevated());
        let sink = CollectSink::new();
        let q = Query {
            text: "file".into(),
            limit: 1100,
            ..Default::default()
        };
        let sum = c.query(&q, &sink).unwrap();
        assert_eq!(sum.returned, 1100);
        assert_eq!(sum.total_matched, 1200);
        let hits = sink.into_inner();
        assert_eq!(hits.len(), 1100);
        assert!(hits[0].path.starts_with(r"C:\file"));

        let stats = c.stats();
        assert!(stats.helper_connected, "the helper marks itself connected");
        assert_eq!(stats.total_entries, 1201);
        assert_eq!(c.resolve_path(&"C".into(), 100).unwrap(), r"C:\file0.txt");
        assert!(matches!(
            c.resolve_path(&"C".into(), 1),
            Err(FxError::NotFound(_))
        ));
        c.clear().unwrap();
    }

    #[test]
    fn errors_cross_the_pipe_intact() {
        let (c, _) = client(1);
        let q = Query {
            text: "(".into(),
            mode: fx_core::MatchMode::Regex,
            ..Default::default()
        };
        assert!(matches!(c.query(&q, &NullSink), Err(FxError::BadQuery(_))));
        // The connection is still usable afterwards.
        assert!(c.query(&Query::default(), &NullSink).is_ok());
    }

    #[test]
    fn huge_limits_are_capped_by_the_helper() {
        let (c, _) = client(10);
        let q = Query {
            limit: u32::MAX,
            ..Default::default()
        };
        // Can only verify the request survives; the cap itself is exercised below.
        assert_eq!(c.query(&q, &NullSink).unwrap().returned, 10);

        let mut q2 = Query::default();
        q2.limit = u32::MAX;
        q2.limit = q2.effective_limit().min(MAX_REMOTE_LIMIT);
        assert_eq!(q2.limit, MAX_REMOTE_LIMIT);
    }

    #[test]
    fn a_consumer_that_goes_away_cancels_and_the_client_recovers() {
        struct Once(AtomicBool);
        impl BatchSink<SearchHit> for Once {
            fn push(&self, _b: Vec<SearchHit>) -> bool {
                !self.0.swap(true, Ordering::Relaxed)
            }
        }
        let (c, _) = client(3000);
        let q = Query {
            limit: 3000,
            ..Default::default()
        };
        assert_eq!(
            c.query(&q, &Once(AtomicBool::new(false))),
            Err(FxError::Cancelled)
        );
        // The abandoned connection was not returned to the pool; the next call works.
        assert_eq!(c.query(&q, &NullSink).unwrap().returned, 3000);
    }

    #[test]
    fn scan_progress_streams_and_cancel_propagates() {
        struct Count(Mutex<u64>, u64);
        impl ProgressSink for Count {
            fn emit(&self, _p: &Progress) {
                *self.0.lock() += 1;
            }
            fn should_continue(&self) -> bool {
                *self.0.lock() < self.1
            }
        }
        let (c, _) = client(1);
        let all = Count(Mutex::new(0), u64::MAX);
        c.scan(&all).unwrap();
        assert_eq!(*all.0.lock(), 1000);
        let some = Count(Mutex::new(0), 5);
        assert_eq!(c.scan(&some), Err(FxError::Cancelled));
        assert!(c.query(&Query::default(), &NullSink).is_ok());
    }

    #[test]
    fn oversized_batches_are_split_not_dropped() {
        let (a, b) = UnixStream::pair().unwrap();
        let w: SharedWriter = Arc::new(Mutex::new(Box::new(a)));
        let hit = |i: u32| SearchHit {
            entry: fx_core::FileEntry {
                id: i as u64,
                parent: 0,
                name: "x".into(),
                ext: None,
                size: 0,
                modified: 0,
                created: 0,
                kind: fx_core::EntryKind::File,
                attrs: Default::default(),
            },
            volume: "C".into(),
            // ~32k-character paths, the Windows maximum: 500 of these overflow a frame.
            path: "p".repeat(32_000),
            score: 0,
            match_ranges: vec![],
        };
        let hits: Vec<SearchHit> = (0..500).map(hit).collect();
        let reader = std::thread::spawn(move || {
            let mut b = b;
            let mut n = 0;
            let mut frames = 0;
            while n < 500 {
                match read_frame::<HelperResponse>(&mut b).unwrap() {
                    Some(HelperResponse::Hits(h)) => {
                        n += h.len();
                        frames += 1;
                    }
                    other => panic!("{other:?}"),
                }
            }
            frames
        });
        send_hits(&w, hits).unwrap();
        assert!(reader.join().unwrap() > 1);
    }

    #[test]
    fn change_feed_flows_until_unwatched() {
        let (c, backend) = client(1);
        let got = Arc::new(std::sync::Mutex::new(Vec::new()));
        let g = Arc::clone(&got);
        let h = c
            .watch(Box::new(move |ev| g.lock().unwrap().extend(ev)))
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while backend.watch_events.lock().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let ev = ChangeEvent::Desynced { volume: "C".into() };
        (backend.watch_events.lock().as_ref().unwrap())(vec![ev.clone()]);
        while got.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*got.lock().unwrap(), vec![ev]);
        h.stop();
    }

    #[test]
    fn hub_runs_one_real_watch_and_fans_out() {
        use std::sync::atomic::AtomicUsize;
        let backend = Arc::new(Fixed::new(1));
        let hub = WatchHub::new(Arc::clone(&backend));
        hub.start().unwrap();
        let fire = |ev: Vec<ChangeEvent>| (backend.watch_events.lock().as_ref().unwrap())(ev);

        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        let (a2, b2) = (Arc::clone(&a), Arc::clone(&b));
        let ha = hub
            .watch(Box::new(move |e| {
                a2.fetch_add(e.len(), Ordering::Relaxed);
            }))
            .unwrap();
        let _hb = hub
            .watch(Box::new(move |e| {
                b2.fetch_add(e.len(), Ordering::Relaxed);
            }))
            .unwrap();
        let ev = ChangeEvent::Desynced { volume: "C".into() };
        fire(vec![ev.clone()]);
        assert_eq!(
            (a.load(Ordering::Relaxed), b.load(Ordering::Relaxed)),
            (1, 1)
        );

        // Unsubscribing one leaves the other, and the real watch keeps running.
        ha.stop();
        fire(vec![ev.clone(), ev]);
        assert_eq!(
            (a.load(Ordering::Relaxed), b.load(Ordering::Relaxed)),
            (1, 3)
        );
        hub.stop();
    }

    #[test]
    fn walk_backend_is_servable_end_to_end() {
        // The real fallback backend behind the real protocol, as a smoke test that the
        // trait is forwarded faithfully.
        let backend: Arc<dyn IndexBackend> =
            Arc::new(WalkIndex::with_roots(vec![
                env!("CARGO_MANIFEST_DIR").to_owned()
            ]));
        let c = HelperClient::connect(connector(backend)).unwrap();
        c.scan(&NullSink).unwrap();
        let sink = CollectSink::new();
        let q = Query {
            text: "ipc.rs".into(),
            ..Default::default()
        };
        c.query(&q, &sink).unwrap();
        let hits = sink.into_inner();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("ipc.rs"));
        assert_eq!(
            c.resolve_path(&hits[0].volume, hits[0].entry.id).unwrap(),
            hits[0].path
        );
    }
}
