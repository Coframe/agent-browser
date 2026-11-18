//! The platform layer for agent-browser on wasm32-wasip1.
//!
//! WASI covers the clock, environment, files and stdio. It has no event
//! loop, sockets, HTTP or processes, so the JS host supplies those through
//! the `agent_browser_host` import module. The shim crates next to this one
//! (`tokio`, `tokio-tungstenite`, `reqwest`) are built on it.
//!
//! The module is synchronous: [`block_on`] polls its future and the spawned
//! tasks until none can make progress, then calls the `event_wait` import,
//! which the host suspends until something happened (a timer fired, a
//! socket received data, a request finished). The host therefore needs
//! JavaScript Promise Integration, or a module transformed with Asyncify.
//!
//! An event is `[kind: u8][id: u32 le][payload]`. See [`dispatch`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_channel::{mpsc, oneshot};
use futures_executor::{LocalPool, LocalSpawner};
use futures_util::task::{ArcWake, LocalSpawnExt};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

mod sys {
    #[link(wasm_import_module = "agent_browser_host")]
    extern "C" {
        pub fn event_wait() -> usize;
        pub fn event_read(buffer: *mut u8);
        pub fn timer_set(id: u32, ms: f64);
        pub fn timer_clear(id: u32);
        pub fn ws_open(id: u32, url: *const u8, url_len: usize, headers: *const u8, headers_len: usize);
        pub fn ws_send(id: u32, text: *const u8, text_len: usize) -> i32;
        pub fn ws_close(id: u32);
        pub fn fetch_start(id: u32, request: *const u8, request_len: usize, body: *const u8, body_len: usize);
        pub fn fetch_abort(id: u32);
        pub fn tcp_listen(port: u32) -> i32;
        pub fn tcp_unlisten(port: u32);
        pub fn tcp_connect(port: u32) -> i32;
        pub fn conn_write(fd: i32, data: *const u8, data_len: usize) -> i32;
        pub fn conn_close(fd: i32);
        pub fn file_lock(fd: i32) -> i32;
        pub fn process_id() -> u32;
        pub fn process_alive(pid: u32) -> i32;
        pub fn spawn(request: *const u8, request_len: usize) -> u32;
    }
}

const EVENT_TIMER: u8 = 1;
const EVENT_WEBSOCKET: u8 = 2;
const EVENT_FETCH: u8 = 3;
const EVENT_ACCEPT: u8 = 4;
const EVENT_CONN: u8 = 5;

type FetchResult = Result<FetchResponse, FetchError>;

thread_local! {
    static POOL: RefCell<LocalPool> = RefCell::new(LocalPool::new());
    static SPAWNER: LocalSpawner = POOL.with(|pool| pool.borrow().spawner());
    static NEXT_ID: RefCell<u32> = const { RefCell::new(0) };
    static TIMERS: RefCell<HashMap<u32, oneshot::Sender<()>>> = RefCell::new(HashMap::new());
    static SOCKETS: RefCell<HashMap<u32, mpsc::UnboundedSender<WsEvent>>> = RefCell::new(HashMap::new());
    static FETCHES: RefCell<HashMap<u32, oneshot::Sender<FetchResult>>> = RefCell::new(HashMap::new());
    static LISTENERS: RefCell<HashMap<u32, mpsc::UnboundedSender<i32>>> = RefCell::new(HashMap::new());
    static CONNS: RefCell<HashMap<i32, mpsc::UnboundedSender<Vec<u8>>>> = RefCell::new(HashMap::new());
}

fn next_id() -> u32 {
    NEXT_ID.with(|next| {
        let mut next = next.borrow_mut();
        *next = next.wrapping_add(1).max(1);
        *next
    })
}

/// Queues `future` on the executor. It runs inside [`block_on`].
pub fn spawn_local(future: impl Future<Output = ()> + 'static) {
    SPAWNER.with(|spawner| {
        let _ = spawner.spawn_local(future);
    });
}

struct RootWaker(AtomicBool);

impl ArcWake for RootWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.0.store(true, Ordering::SeqCst);
    }
}

/// Runs `future` to completion, along with every spawned task.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let root = Arc::new(RootWaker(AtomicBool::new(true)));
    let waker = futures_util::task::waker(root.clone());
    let mut cx = Context::from_waker(&waker);
    loop {
        if root.0.swap(false, Ordering::SeqCst) {
            if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
                return output;
            }
        }
        // A nested `block_on` finds the pool borrowed by the outer one and
        // makes progress on host events alone.
        POOL.with(|pool| {
            if let Ok(mut pool) = pool.try_borrow_mut() {
                pool.run_until_stalled();
            }
        });
        if !root.0.load(Ordering::SeqCst) {
            dispatch();
        }
    }
}

/// Waits for one host event and hands it to whoever is waiting for it.
fn dispatch() {
    let len = unsafe { sys::event_wait() };
    let mut event = vec![0u8; len];
    unsafe { sys::event_read(event.as_mut_ptr()) };
    if event.len() < 5 {
        return;
    }
    let id = u32::from_le_bytes([event[1], event[2], event[3], event[4]]);
    let payload = &event[5..];
    match event[0] {
        EVENT_TIMER => {
            if let Some(tx) = TIMERS.with(|timers| timers.borrow_mut().remove(&id)) {
                let _ = tx.send(());
            }
        }
        // payload: [0 open | 1 message | 2 close | 3 error][utf-8 text]
        EVENT_WEBSOCKET => {
            let Some((&kind, text)) = payload.split_first() else {
                return;
            };
            let text = String::from_utf8_lossy(text).into_owned();
            let event = match kind {
                0 => WsEvent::Open,
                1 => WsEvent::Message(text),
                2 => WsEvent::Close((!text.is_empty()).then_some(text)),
                _ => WsEvent::Error(text),
            };
            let last = matches!(event, WsEvent::Close(_) | WsEvent::Error(_));
            SOCKETS.with(|sockets| {
                let mut sockets = sockets.borrow_mut();
                if let Some(tx) = sockets.get(&id) {
                    let _ = tx.unbounded_send(event);
                }
                if last {
                    sockets.remove(&id);
                }
            });
        }
        // payload: [status: u32 le][head_len: u32 le][head json][body]
        // status 0: head is a `FetchError`. Otherwise head is `{ url, headers }`.
        EVENT_FETCH => {
            let result = parse_fetch(payload);
            if let Some(tx) = FETCHES.with(|fetches| fetches.borrow_mut().remove(&id)) {
                let _ = tx.send(result);
            }
        }
        // id: the listening port. payload: [fd: u32 le] of the accepted connection.
        EVENT_ACCEPT => {
            if let Some(fd) = payload.get(..4) {
                let fd = i32::from_le_bytes([fd[0], fd[1], fd[2], fd[3]]);
                LISTENERS.with(|listeners| {
                    if let Some(tx) = listeners.borrow().get(&id) {
                        let _ = tx.unbounded_send(fd);
                    }
                });
            }
        }
        // id: the connection fd. payload: received bytes; empty at end of stream.
        EVENT_CONN => CONNS.with(|conns| {
            let mut conns = conns.borrow_mut();
            let fd = id as i32;
            if payload.is_empty() {
                conns.remove(&fd);
            } else if let Some(tx) = conns.get(&fd) {
                let _ = tx.unbounded_send(payload.to_vec());
            }
        }),
        _ => {}
    }
}

/// Completes after `ms` milliseconds. Dropping it cancels the host timer.
#[derive(Debug)]
pub struct Timer {
    id: u32,
    fired: oneshot::Receiver<()>,
}

pub fn timer(ms: f64) -> Timer {
    let id = next_id();
    let (tx, fired) = oneshot::channel();
    TIMERS.with(|timers| timers.borrow_mut().insert(id, tx));
    unsafe { sys::timer_set(id, ms.max(0.0)) };
    Timer { id, fired }
}

impl Future for Timer {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.fired).poll(cx).map(|_| ())
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let pending = TIMERS
            .try_with(|timers| timers.borrow_mut().remove(&self.id).is_some())
            .unwrap_or(false);
        if pending {
            unsafe { sys::timer_clear(self.id) };
        }
    }
}

#[derive(Debug)]
pub enum WsEvent {
    Open,
    Message(String),
    Close(Option<String>),
    Error(String),
}

/// A WebSocket the host owns. Dropping it closes the socket.
#[derive(Debug)]
pub struct WebSocket {
    id: u32,
}

/// Asks the host to open a WebSocket. The first event is [`WsEvent::Open`]
/// or [`WsEvent::Error`].
pub fn ws_open(url: &str, headers: &[(String, String)]) -> (WebSocket, mpsc::UnboundedReceiver<WsEvent>) {
    let id = next_id();
    let (tx, events) = mpsc::unbounded();
    SOCKETS.with(|sockets| sockets.borrow_mut().insert(id, tx));
    let headers = serde_json::to_vec(headers).unwrap_or_else(|_| b"[]".to_vec());
    unsafe { sys::ws_open(id, url.as_ptr(), url.len(), headers.as_ptr(), headers.len()) };
    (WebSocket { id }, events)
}

impl WebSocket {
    pub fn send(&self, text: &str) -> Result<(), String> {
        match unsafe { sys::ws_send(self.id, text.as_ptr(), text.len()) } {
            0 => Ok(()),
            _ => Err("WebSocket is closed".to_string()),
        }
    }
}

impl Drop for WebSocket {
    fn drop(&mut self) {
        let _ = SOCKETS.try_with(|sockets| sockets.borrow_mut().remove(&self.id));
        unsafe { sys::ws_close(self.id) };
    }
}

#[derive(Debug, Default, Serialize)]
pub struct FetchRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    #[serde(skip)]
    pub body: Option<Vec<u8>>,
    /// Milliseconds before the host aborts the request.
    pub timeout_ms: Option<f64>,
}

#[derive(Debug, Default)]
pub struct FetchResponse {
    pub status: u16,
    /// The URL after redirects.
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Default, Deserialize)]
pub struct FetchError {
    pub message: String,
    #[serde(default)]
    pub timeout: bool,
}

impl FetchError {
    fn new(message: impl Into<String>) -> Self {
        FetchError {
            message: message.into(),
            timeout: false,
        }
    }
}

#[derive(Deserialize)]
struct FetchHead {
    url: String,
    headers: Vec<(String, String)>,
}

fn parse_fetch(payload: &[u8]) -> FetchResult {
    let malformed = || FetchError::new("malformed response from host");
    let status = payload.get(..4).ok_or_else(malformed)?;
    let status = u32::from_le_bytes([status[0], status[1], status[2], status[3]]);
    let head_len = payload.get(4..8).ok_or_else(malformed)?;
    let head_len = u32::from_le_bytes([head_len[0], head_len[1], head_len[2], head_len[3]]) as usize;
    let head = payload.get(8..8 + head_len).ok_or_else(malformed)?;
    let body = &payload[8 + head_len..];
    if status == 0 {
        return Err(serde_json::from_slice::<FetchError>(head)
            .unwrap_or_else(|_| FetchError::new(String::from_utf8_lossy(head))));
    }
    let head: FetchHead = serde_json::from_slice(head).map_err(|_| malformed())?;
    Ok(FetchResponse {
        status: status as u16,
        url: head.url,
        headers: head.headers,
        body: body.to_vec(),
    })
}

/// Sends an HTTP request through the host and buffers the whole response.
pub async fn fetch(request: FetchRequest) -> FetchResult {
    struct Abort(u32);
    impl Drop for Abort {
        fn drop(&mut self) {
            let pending = FETCHES
                .try_with(|fetches| fetches.borrow_mut().remove(&self.0).is_some())
                .unwrap_or(false);
            if pending {
                unsafe { sys::fetch_abort(self.0) };
            }
        }
    }

    let id = next_id();
    let (tx, done) = oneshot::channel();
    FETCHES.with(|fetches| fetches.borrow_mut().insert(id, tx));
    let _abort = Abort(id);
    let head = serde_json::to_vec(&request).map_err(|e| FetchError::new(e.to_string()))?;
    let body = request.body.as_deref().unwrap_or_default();
    unsafe { sys::fetch_start(id, head.as_ptr(), head.len(), body.as_ptr(), body.len()) };
    done.await
        .unwrap_or_else(|_| Err(FetchError::new("request was dropped by the host")))
}

/// A loopback listener. Instances of this module reach each other through
/// the host, which stands in for `127.0.0.1`.
#[derive(Debug)]
pub struct TcpListener {
    port: u16,
    accepted: mpsc::UnboundedReceiver<i32>,
}

/// Listens on `port`, or on a free port when `port` is 0.
pub fn tcp_listen(port: u16) -> std::io::Result<TcpListener> {
    let bound = unsafe { sys::tcp_listen(port as u32) };
    if bound <= 0 {
        return Err(std::io::ErrorKind::AddrInUse.into());
    }
    let (tx, accepted) = mpsc::unbounded();
    LISTENERS.with(|listeners| listeners.borrow_mut().insert(bound as u32, tx));
    Ok(TcpListener {
        port: bound as u16,
        accepted,
    })
}

impl TcpListener {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn poll_accept(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<Conn>> {
        match self.accepted.poll_next_unpin(cx) {
            Poll::Ready(Some(fd)) => Poll::Ready(Ok(Conn::adopt(fd))),
            Poll::Ready(None) => Poll::Ready(Err(std::io::ErrorKind::NotConnected.into())),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        let _ = LISTENERS.try_with(|listeners| listeners.borrow_mut().remove(&(self.port as u32)));
        unsafe { sys::tcp_unlisten(self.port as u32) };
    }
}

/// The accepting end of a loopback connection. Received bytes arrive as
/// host events. Dropping it closes the connection.
#[derive(Debug)]
pub struct Conn {
    fd: i32,
    received: mpsc::UnboundedReceiver<Vec<u8>>,
}

impl Conn {
    fn adopt(fd: i32) -> Self {
        let (tx, received) = mpsc::unbounded();
        CONNS.with(|conns| conns.borrow_mut().insert(fd, tx));
        Conn { fd, received }
    }

    /// The next received bytes, or `None` at end of stream.
    pub fn poll_read(&mut self, cx: &mut Context<'_>) -> Poll<Option<Vec<u8>>> {
        self.received.poll_next_unpin(cx)
    }

    pub fn write(&self, data: &[u8]) -> std::io::Result<usize> {
        match unsafe { sys::conn_write(self.fd, data.as_ptr(), data.len()) } {
            0 => Ok(data.len()),
            _ => Err(std::io::ErrorKind::BrokenPipe.into()),
        }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        let _ = CONNS.try_with(|conns| conns.borrow_mut().remove(&self.fd));
        unsafe { sys::conn_close(self.fd) };
    }
}

/// Connects to a listener that another instance opened on `port`. The
/// stream is a WASI file descriptor, so it blocks like a native socket.
pub fn tcp_connect(port: u16) -> std::io::Result<std::net::TcpStream> {
    use std::os::fd::FromRawFd;
    match unsafe { sys::tcp_connect(port as u32) } {
        fd if fd >= 0 => Ok(unsafe { std::net::TcpStream::from_raw_fd(fd) }),
        _ => Err(std::io::ErrorKind::ConnectionRefused.into()),
    }
}

/// Runs first in `main`. wasi-libc starts in `/`, so the working directory
/// is taken from `PWD`.
pub fn init() {
    if let Some(dir) = std::env::var_os("PWD").filter(|dir| !dir.is_empty()) {
        let _ = std::env::set_current_dir(dir);
    }
}

/// The directory for temporary files. `std::env::temp_dir` traps on WASI.
pub fn temp_dir() -> std::path::PathBuf {
    std::env::var_os("TMPDIR")
        .filter(|dir| !dir.is_empty())
        .map(Into::into)
        .unwrap_or_else(|| "/tmp".into())
}

/// The home directory, from `HOME`. `std::env::home_dir` is `None` on WASI.
pub fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .filter(|dir| !dir.is_empty())
        .map(Into::into)
}

/// The path this module was started as, from `argv[0]`.
pub fn current_exe() -> std::path::PathBuf {
    std::env::args_os()
        .next()
        .map(Into::into)
        .unwrap_or_else(|| "agent-browser".into())
}

/// The id the host gave this instance. `std::process::id` traps on WASI.
pub fn process_id() -> u32 {
    unsafe { sys::process_id() }
}

pub fn process_alive(pid: u32) -> bool {
    unsafe { sys::process_alive(pid) != 0 }
}

/// Starts another instance of this module with the arguments and
/// environment of `command`, and returns its process id.
pub fn spawn(command: &std::process::Command) -> std::io::Result<u32> {
    let args: Vec<String> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let env: Vec<(String, String)> = command
        .get_envs()
        .filter_map(|(key, value)| {
            Some((
                key.to_string_lossy().into_owned(),
                value?.to_string_lossy().into_owned(),
            ))
        })
        .collect();
    let request = serde_json::to_vec(&(args, env)).map_err(std::io::Error::other)?;
    match unsafe { sys::spawn(request.as_ptr(), request.len()) } {
        0 => Err(std::io::Error::other("the host could not start a process")),
        pid => Ok(pid),
    }
}

/// Takes an exclusive lock on `file`, waiting for other instances to
/// release theirs. The host releases it when the file is closed. WASI has
/// no file locks, so `File::lock` fails there.
pub fn lock_file(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    match unsafe { sys::file_lock(file.as_raw_fd()) } {
        0 => Ok(()),
        _ => Err(std::io::Error::other("the host could not lock the file")),
    }
}
