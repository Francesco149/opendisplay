//! Wayland-protocol pieces shared by every backend that is a Wayland
//! compositor speaking the common protocols (SENDER-PLAN §3.1).

pub mod capture;
pub mod cursor;
pub mod display;
pub mod input;
pub mod registry;
pub mod screencopy;
pub mod shm_pool;

use std::time::Duration;

use anyhow::{Context, Result};
use wayland_client::Connection;

/// `blocking_dispatch` with an upper bound, so a capture of a static output
/// (which never produces a frame) still notices the stop flag.
pub fn dispatch_with_timeout<S>(
    conn: &Connection,
    queue: &mut wayland_client::EventQueue<S>,
    st: &mut S,
    timeout: Duration,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    queue.dispatch_pending(st)?;
    conn.flush()?;
    let Some(guard) = conn.prepare_read() else {
        queue.dispatch_pending(st)?;
        return Ok(());
    };
    let fd = guard.connection_fd().as_raw_fd();
    let mut pfd = libc_pollfd(fd);
    // SAFETY: pollfd points at one valid struct for the duration of the call.
    let r = unsafe { libc_poll(&mut pfd, 1, timeout.as_millis() as i32) };
    if r > 0 {
        // A dead connection (e.g. after a protocol error) must end the
        // caller's loop instead of spinning on it.
        match guard.read() {
            Ok(_) => {}
            Err(wayland_client::backend::WaylandError::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e).context("reading from the compositor"),
        }
        queue.dispatch_pending(st)?;
    }
    Ok(())
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

fn libc_pollfd(fd: i32) -> PollFd {
    PollFd {
        fd,
        events: 0x001, /* POLLIN */
        revents: 0,
    }
}

unsafe extern "C" {
    #[link_name = "poll"]
    fn libc_poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
}
