//! A minimal pty helper for the runner's hidden tmux clients.
//!
//! [`open_pty`] builds a master/slave pair with `posix_openpt` +
//! `grantpt` + `unlockpt` + `ptsname_r` + `open` (not `libc::openpty`, so
//! the winsize is fully under our control). [`spawn_on_pty`] starts a
//! command with stdin/stdout/stderr as dups of the slave, made a session
//! leader with a controlling tty (`setsid` + `TIOCSCTTY`): dropping the
//! master hangs the child up with SIGHUP, which is what makes a runner
//! crash take its client ptys (and anchors) with it. All `unsafe` lives
//! in this file.

use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Stdio as StdStdio;

use tokio::process::{Command, Stdio};

use crate::runner::RunnerError;

/// A spawned client process; reaped by a background task, never a zombie.
pub struct PtyChild {
    /// The child's pid.
    pub pid: u32,
}

/// Open a pty pair sized to `cols` x `rows` with pixel winsize fields.
///
/// The master is `O_NONBLOCK | O_CLOEXEC`.
pub fn open_pty(
    cols: u16,
    rows: u16,
    xpix: u16,
    ypix: u16,
) -> Result<(OwnedFd, OwnedFd), RunnerError> {
    let open_err = |msg: &str| {
        RunnerError::Internal(format!(
            "open_pty {msg}: {}",
            std::io::Error::last_os_error()
        ))
    };
    unsafe {
        let master_raw = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if master_raw < 0 {
            return Err(open_err("posix_openpt"));
        }
        let master = OwnedFd::from(unsafe { std::fs::File::from_raw_fd(master_raw) });
        if libc::grantpt(master.as_raw_fd()) != 0 {
            return Err(open_err("grantpt"));
        }
        if libc::unlockpt(master.as_raw_fd()) != 0 {
            return Err(open_err("unlockpt"));
        }
        let mut name = [0u8; 256];
        let rc = libc::ptsname_r(
            master.as_raw_fd(),
            name.as_mut_ptr() as *mut libc::c_char,
            name.len(),
        );
        if rc != 0 {
            return Err(RunnerError::Internal(format!(
                "ptsname_r: {}",
                std::io::Error::from_raw_os_error(rc)
            )));
        }
        let slave_name = std::ffi::CStr::from_bytes_until_nul(&name)
            .map_err(|e| RunnerError::Internal(format!("ptsname: {e}")))?;
        let cpath = std::ffi::CString::new(slave_name.to_bytes())
            .map_err(|e| RunnerError::Internal(format!("slave path: {e}")))?;
        let slave_raw = libc::open(
            cpath.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        );
        if slave_raw < 0 {
            return Err(open_err("open slave"));
        }
        let slave = OwnedFd::from(unsafe { std::fs::File::from_raw_fd(slave_raw) });
        set_winsize(&master, cols, rows, xpix, ypix);
        // Master non-blocking for AsyncFd; the slave stays blocking so the
        // child sees a normal tty.
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Ok((master, slave))
    }
}

/// Read the current winsize of `fd`: (cols, rows, xpix, ypix).
pub fn winsize_of<Fd: AsRawFd>(fd: &Fd) -> (u16, u16, u16, u16) {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        libc::ioctl(fd.as_raw_fd(), libc::TIOCGWINSZ, &mut ws);
    }
    (ws.ws_col, ws.ws_row, ws.ws_xpixel, ws.ws_ypixel)
}

/// Write the winsize (cells and pixel fields) to `fd`.
pub fn set_winsize<Fd: AsRawFd>(fd: &Fd, cols: u16, rows: u16, xpix: u16, ypix: u16) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: xpix,
        ws_ypixel: ypix,
    };
    unsafe {
        libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ, &ws);
    }
}

/// Spawn `cmd` (already env-configured by the caller) on the pty slave.
///
/// stdin/stdout/stderr are dups of the slave; `pre_exec` makes the child
/// a session leader with the slave as its controlling tty. The caller
/// closes the slave in the parent after this returns. The child is
/// reaped by a task (`wait`), never left a zombie, and `kill_on_drop`
/// is NOT used: dropping the master must hang the child up (SIGHUP),
/// which the controlling-tty setup provides.
pub fn spawn_on_pty(mut cmd: Command, slave: &OwnedFd) -> Result<PtyChild, RunnerError> {
    let slave_fd = slave.as_raw_fd();
    let dup = || -> Result<OwnedFd, RunnerError> {
        slave
            .try_clone()
            .map_err(|e| RunnerError::Internal(format!("dup pty slave: {e}")))
    };
    cmd.stdin(StdStdio::from(dup()?))
        .stdout(StdStdio::from(dup()?))
        .stderr(StdStdio::from(dup()?));
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| RunnerError::TmuxFailed(format!("spawn client: {e}")))?;
    let pid = child.id().unwrap_or(0);
    tokio::task::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(PtyChild { pid })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_pty_carries_the_requested_winsize_and_pixels() {
        let (master, _slave) = open_pty(100, 30, 900, 540).unwrap();
        assert_eq!(winsize_of(&master), (100, 30, 900, 540));
        set_winsize(&master, 80, 24, 720, 432);
        assert_eq!(winsize_of(&master), (80, 24, 720, 432));
        // The master is non-blocking (flags carry O_NONBLOCK).
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        assert!(flags & libc::O_NONBLOCK != 0, "master must be O_NONBLOCK");
    }
}
