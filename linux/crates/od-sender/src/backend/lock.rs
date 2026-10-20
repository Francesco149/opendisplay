//! One sender per virtual output. A second sender for the same receiver would
//! adopt and reconfigure an output the first is still capturing, which takes
//! Hyprland down (see [`super::VirtualDisplay`]). The lock is an `flock` on
//! `$XDG_RUNTIME_DIR/opendisplay/<name>.lock`, released when the file closes,
//! including on a crash.

use std::fs::File;
use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Debug)]
pub struct OutputLock {
    _file: File,
}

pub fn dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("opendisplay")
}

/// Take the lock for output `name`, or fail with "already driving" when a
/// running sender holds it.
pub fn lock_output(name: &str) -> Result<OutputLock> {
    match try_lock(name)? {
        Some(l) => Ok(l),
        None => anyhow::bail!(
            "another od-sender is already driving {name} ({})",
            dir().join(format!("{name}.lock")).display()
        ),
    }
}

/// `Ok(None)` when another process holds the lock.
pub fn try_lock(name: &str) -> Result<Option<OutputLock>> {
    let dir = dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{name}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    use std::os::fd::AsRawFd;
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    // SAFETY: flock on a descriptor we own for the duration of the call.
    if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
        return Ok(Some(OutputLock { _file: file }));
    }
    let e = std::io::Error::last_os_error();
    if e.kind() == std::io::ErrorKind::WouldBlock {
        return Ok(None);
    }
    Err(e).with_context(|| format!("locking {}", path.display()))
}

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::{lock_output, try_lock};

    #[test]
    fn second_sender_for_the_same_output_is_refused() {
        let name = format!("od-test-{}", std::process::id());
        let first = lock_output(&name).expect("first lock");
        let err = lock_output(&name).expect_err("second lock must fail");
        assert!(err.to_string().contains("already driving"), "{err:#}");
        assert!(try_lock(&name).unwrap().is_none());
        drop(first);
        drop(lock_output(&name).expect("lock is free again once the holder closes"));
        let _ = std::fs::remove_file(super::dir().join(format!("{name}.lock")));
    }
}
