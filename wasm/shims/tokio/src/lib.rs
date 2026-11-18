//! The subset of the tokio API that agent-browser uses, for wasm32-wasip1.
//!
//! The wasm crate depends on this package under the name `tokio`, so upstream
//! source files compile unchanged. Channels, `select!` and the io traits are
//! tokio's own. Timers and tasks run on `agent-browser-host`, which the JS
//! host drives. Sockets, child processes and runtimes do not exist here:
//! their constructors return `io::ErrorKind::Unsupported`.

pub use real_tokio::{io, join, pin, select, sync, try_join};

pub mod net;
pub mod process;
pub mod runtime;
pub mod signal;
pub mod task;
pub mod time;

pub use task::spawn;

pub(crate) fn unsupported(what: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("{what} is not available on this platform"),
    )
}
