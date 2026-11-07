//! The anchor: one permanent, silent client per session.
//!
//! The runner keeps the master end of a pty in a `HashMap<id, Anchor>`.
//! A drain task reads the master and discards bytes; it NEVER writes.
//! On a runner restart the old anchor dies of SIGHUP (its master closed)
//! and the new runner spawns a fresh one; the session survives.
//!
//! `ignore-size` plus `window-size manual` never resizes the window. The
//! responder lands in T9; the anchor is silent here.

use std::fs::File;
use std::io::Read;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

use tokio::process::Command;

use super::RunnerError;

/// One permanent silent client attached to a holder.
pub struct Anchor {
    master: File,
    cell_w: u16,
    cell_h: u16,
    /// The anchor client's pid (a child of the runner).
    pid: u32,
    /// The tmux client process (killed on drop; SIGHUP would do too).
    _child: tokio::process::Child,
}

impl Anchor {
    /// Spawn `<tmux-bin> -S <sock> attach-session -t kb -f ignore-size`
    /// on a fresh pty of the given grid, with the pixel winsize derived
    /// from the cell size.
    pub fn spawn(
        tmux_bin: &str,
        sock: &std::path::Path,
        cols: u16,
        rows: u16,
        cell_w: u16,
        cell_h: u16,
    ) -> Result<Anchor, RunnerError> {
        let (master, slave) = openpty_pair(cols, rows, cell_w, cell_h)?;
        let slave_fd = slave.as_raw_fd();
        let mut cmd = Command::new(tmux_bin);
        cmd.arg("-S")
            .arg(sock)
            .args(["attach-session", "-t", "kb", "-f", "ignore-size"]);
        cmd.env_clear();
        cmd.env("TERM", "xterm-256color");
        cmd.env("LANG", "C.UTF-8");
        cmd.env("HOME", std::env::var("HOME").unwrap_or_default());
        cmd.stdin(
            slave
                .try_clone()
                .map_err(|e| RunnerError::Internal(e.to_string()))?,
        )
        .stdout(
            slave
                .try_clone()
                .map_err(|e| RunnerError::Internal(e.to_string()))?,
        )
        .stderr(
            slave
                .try_clone()
                .map_err(|e| RunnerError::Internal(e.to_string()))?,
        );
        unsafe {
            cmd.pre_exec(|| {
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
            .map_err(|e| RunnerError::TmuxFailed(format!("spawn anchor: {e}")))?;
        let pid = child.id().unwrap_or(0);
        // The drain task reads the master and discards; it never writes.
        let drain = master
            .try_clone()
            .map_err(|e| RunnerError::Internal(e.to_string()))?;
        tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 4096];
            let mut drain = drain;
            loop {
                match drain.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        drop(slave);
        Ok(Anchor {
            master,
            cell_w,
            cell_h,
            pid,
            _child: child,
        })
    }

    /// The anchor client's pid.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Current pty size: (cols, rows, pixel_w, pixel_h).
    pub fn winsize(&self) -> (u16, u16, u16, u16) {
        winsize_of(&self.master)
    }

    /// Resize the pty (cells and pixel fields).
    pub fn set_winsize(&self, cols: u16, rows: u16) {
        let (cell_w, cell_h) = (self.cell_w, self.cell_h);
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: cols.saturating_mul(cell_w),
            ws_ypixel: rows.saturating_mul(cell_h),
        };
        unsafe {
            libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws);
        }
    }
}

impl Drop for Anchor {
    fn drop(&mut self) {
        // Closing the master SIGHUPs the client; killing the child handle
        // too keeps a dropped runner from leaking a client process.
        if self.pid != 0 {
            unsafe {
                libc::kill(self.pid as i32, libc::SIGTERM);
            }
        }
    }
}

/// Open a pty pair sized to (cols x rows) with pixel fields from the cell size.
fn openpty_pair(
    cols: u16,
    rows: u16,
    cell_w: u16,
    cell_h: u16,
) -> Result<(File, OwnedFd), RunnerError> {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: cols.saturating_mul(cell_w),
        ws_ypixel: rows.saturating_mul(cell_h),
    };
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let rc = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), &ws) };
    if rc != 0 || master < 0 || slave < 0 {
        return Err(RunnerError::Internal(format!(
            "openpty failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    Ok((master, slave))
}

fn winsize_of(file: &File) -> (u16, u16, u16, u16) {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        libc::ioctl(file.as_raw_fd(), libc::TIOCGWINSZ, &mut ws);
    }
    (ws.ws_col, ws.ws_row, ws.ws_xpixel, ws.ws_ypixel)
}
