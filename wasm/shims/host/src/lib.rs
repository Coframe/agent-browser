//! The platform layer for agent-browser on wasm32-wasip1.
//!
//! WASI covers the clock, environment, files and stdio. It has no event
//! loop, sockets or HTTP, so the JS host supplies those through the
//! `agent_browser_host` import module, and reports results by calling the
//! `ab_*` exports below. The shim crates next to this one (`tokio`,
//! `tokio-tungstenite`, `reqwest`) are built on it.
//!
//! Contract with the host:
//!
//! - Wasm runs only while the host is inside an export. Every `ab_*` event
//!   export runs the executor until no task can make progress.
//! - The host must not call an export from inside an import. It queues the
//!   event and delivers it from its own event loop.
//! - Buffers passed to an import are borrowed for that call. Buffers passed
//!   to an export are allocated with [`ab_alloc`] and owned by the callee.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_channel::{mpsc, oneshot};
use futures_executor::{LocalPool, LocalSpawner};
use futures_util::task::LocalSpawnExt;
use serde::{Deserialize, Serialize};

mod sys {
    #[link(wasm_import_module = "agent_browser_host")]
    extern "C" {
        pub fn timer_set(id: u32, ms: f64);
        pub fn timer_clear(id: u32);
        pub fn ws_open(id: u32, url: *const u8, url_len: usize, headers: *const u8, headers_len: usize);
        pub fn ws_send(id: u32, text: *const u8, text_len: usize) -> i32;
        pub fn ws_close(id: u32);
        pub fn fetch_start(id: u32, request: *const u8, request_len: usize, body: *const u8, body_len: usize);
        pub fn fetch_abort(id: u32);
    }
}

thread_local! {
    static POOL: RefCell<LocalPool> = RefCell::new(LocalPool::new());
    static SPAWNER: LocalSpawner = POOL.with(|pool| pool.borrow().spawner());
    static NEXT_ID: RefCell<u32> = const { RefCell::new(0) };
    static TIMERS: RefCell<HashMap<u32, oneshot::Sender<()>>> = RefCell::new(HashMap::new());
    static SOCKETS: RefCell<HashMap<u32, mpsc::UnboundedSender<WsEvent>>> = RefCell::new(HashMap::new());
    static FETCHES: RefCell<HashMap<u32, oneshot::Sender<Result<FetchResponse, FetchError>>>> =
        RefCell::new(HashMap::new());
}

fn next_id() -> u32 {
    NEXT_ID.with(|next| {
        let mut next = next.borrow_mut();
        *next = next.wrapping_add(1).max(1);
        *next
    })
}

/// Queues `future` on the executor. It first runs on the next [`run`].
pub fn spawn_local(future: impl Future<Output = ()> + 'static) {
    SPAWNER.with(|spawner| {
        let _ = spawner.spawn_local(future);
    });
}

/// Runs every task until none can make progress without a host event.
pub fn run() {
    POOL.with(|pool| {
        // A host that breaks the no-reentry rule finds the pool borrowed;
        // the outer call drains the tasks it woke.
        if let Ok(mut pool) = pool.try_borrow_mut() {
            pool.run_until_stalled();
        }
    });
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

#[derive(Deserialize)]
struct FetchMeta {
    url: String,
    headers: Vec<(String, String)>,
}

/// Sends an HTTP request through the host and buffers the whole response.
pub async fn fetch(request: FetchRequest) -> Result<FetchResponse, FetchError> {
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
    let head = serde_json::to_vec(&request).map_err(|e| FetchError {
        message: e.to_string(),
        timeout: false,
    })?;
    let body = request.body.as_deref().unwrap_or_default();
    unsafe { sys::fetch_start(id, head.as_ptr(), head.len(), body.as_ptr(), body.len()) };
    done.await.unwrap_or_else(|_| {
        Err(FetchError {
            message: "request was dropped by the host".to_string(),
            timeout: false,
        })
    })
}

/// Takes ownership of a buffer the host allocated with [`ab_alloc`].
unsafe fn take(ptr: *mut u8, len: usize) -> Vec<u8> {
    if ptr.is_null() || len == 0 {
        return Vec::new();
    }
    Vec::from_raw_parts(ptr, len, len)
}

#[no_mangle]
pub extern "C" fn ab_alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr
}

#[no_mangle]
pub extern "C" fn ab_poll() {
    run();
}

#[no_mangle]
pub extern "C" fn ab_timer_fired(id: u32) {
    if let Some(tx) = TIMERS.with(|timers| timers.borrow_mut().remove(&id)) {
        let _ = tx.send(());
    }
    run();
}

/// `kind`: 0 open, 1 message, 2 close, 3 error. The buffer holds the
/// message text, the close reason or the error message.
///
/// # Safety
/// `ptr` and `len` must describe a buffer from [`ab_alloc`], or be null/0.
#[no_mangle]
pub unsafe extern "C" fn ab_ws_event(id: u32, kind: u32, ptr: *mut u8, len: usize) {
    let text = String::from_utf8_lossy(&take(ptr, len)).into_owned();
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
    run();
}

/// `status` 0 means the request failed and `meta` is a JSON [`FetchError`].
/// Otherwise `meta` is JSON `{ url, headers }` and `body` the response body.
///
/// # Safety
/// Both buffers must come from [`ab_alloc`], or be null/0.
#[no_mangle]
pub unsafe extern "C" fn ab_fetch_done(
    id: u32,
    status: u32,
    meta: *mut u8,
    meta_len: usize,
    body: *mut u8,
    body_len: usize,
) {
    let meta = take(meta, meta_len);
    let body = take(body, body_len);
    let result = if status == 0 {
        Err(serde_json::from_slice::<FetchError>(&meta).unwrap_or_else(|_| FetchError {
            message: String::from_utf8_lossy(&meta).into_owned(),
            timeout: false,
        }))
    } else {
        match serde_json::from_slice::<FetchMeta>(&meta) {
            Ok(meta) => Ok(FetchResponse {
                status: status as u16,
                url: meta.url,
                headers: meta.headers,
                body,
            }),
            Err(e) => Err(FetchError {
                message: format!("malformed response from host: {e}"),
                timeout: false,
            }),
        }
    };
    if let Some(tx) = FETCHES.with(|fetches| fetches.borrow_mut().remove(&id)) {
        let _ = tx.send(result);
    }
    run();
}
