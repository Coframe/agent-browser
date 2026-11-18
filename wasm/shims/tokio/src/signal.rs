//! Process signals do not exist on this platform.

use std::io;

pub async fn ctrl_c() -> io::Result<()> {
    std::future::pending().await
}
