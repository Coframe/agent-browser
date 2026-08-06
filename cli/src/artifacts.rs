//! Pluggable artifact persistence. Commands that produce files (screenshots,
//! PDFs, HARs, diffs) write through [`write`] so library hosts without a real
//! filesystem (e.g. wasm) can install their own writer via
//! [`set_artifact_writer`]. When no writer is installed, [`write`] falls back
//! to `std::fs::write`, preserving native CLI behavior.

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::sync::OnceLock;

    pub type ArtifactWriter = Box<dyn Fn(&str, &[u8]) -> Result<(), String> + Send + Sync>;

    static WRITER: OnceLock<ArtifactWriter> = OnceLock::new();

    /// Install a process-global artifact writer. Returns an error if a writer
    /// was already installed.
    pub fn set_artifact_writer(writer: ArtifactWriter) -> Result<(), String> {
        WRITER
            .set(writer)
            .map_err(|_| "artifact writer already installed".to_string())
    }

    pub fn write(path: &str, bytes: &[u8]) -> Result<(), String> {
        if let Some(writer) = WRITER.get() {
            return writer(path, bytes);
        }
        std::fs::write(path, bytes).map_err(|e| format!("Failed to write {}: {}", path, e))
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::cell::RefCell;

    pub type ArtifactWriter = Box<dyn Fn(&str, &[u8]) -> Result<(), String>>;

    thread_local! {
        static WRITER: RefCell<Option<ArtifactWriter>> = const { RefCell::new(None) };
    }

    /// Install a process-global artifact writer. Returns an error if a writer
    /// was already installed.
    pub fn set_artifact_writer(writer: ArtifactWriter) -> Result<(), String> {
        WRITER.with(|w| {
            let mut slot = w.borrow_mut();
            if slot.is_some() {
                return Err("artifact writer already installed".to_string());
            }
            *slot = Some(writer);
            Ok(())
        })
    }

    pub fn write(path: &str, bytes: &[u8]) -> Result<(), String> {
        WRITER.with(|w| match w.borrow().as_ref() {
            Some(writer) => writer(path, bytes),
            None => Err(format!(
                "cannot write {}: no artifact writer installed on this platform",
                path
            )),
        })
    }
}

pub use imp::{set_artifact_writer, write, ArtifactWriter};
