//! Read access to a session's working area: the `fs_ls` and `fs_get` ops
//! (FXLS).
//!
//! Two roots are readable: `cwd`, the working directory the session was
//! LAUNCHED in (the `@kb_cwd` the runner recorded, never the live cwd of
//! the pane process, which the agent can change), and `up`, the session's
//! upload area ([`upload::up_dir`](crate::runner::upload::up_dir)). The
//! client never names a path to the kernel directly: it names a root and a
//! path under it.
//!
//! **Containment is the kernel's.** The root is opened once as a directory
//! descriptor and the requested path is resolved RELATIVE to it with
//! `openat2(2)` and `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS |
//! RESOLVE_NO_SYMLINKS`, so a `..` that climbs out of the root, an absolute
//! symlink, a `/proc/self/...` magic link and a symlink loop all fail in
//! the kernel. No symlink is followed at all, inside the root either. There
//! is no per-component fallback and no string-prefix check standing in for
//! the kernel: where `openat2` is unavailable (an old kernel, a seccomp
//! profile that denies it, a non-Linux host) both ops fail closed with an
//! `internal` refusal. An absolute client path is only ever turned into a
//! relative one by a component-wise `Path::strip_prefix` against the root
//! it was sent with, and then goes through the same `openat2` call.
//!
//! **Regular files and directories only.** After the open, `fstat` must say
//! `S_IFREG` (a read) or `S_IFDIR` (a listing); anything else is
//! `not-a-file`. The open is `O_NONBLOCK` so a FIFO can never block the
//! runner before that check. A listing omits (and counts in `skipped`) every
//! entry that is not a regular file or directory, judged with an `lstat`
//! (`AT_SYMLINK_NOFOLLOW`) relative to the listed directory descriptor.
//!
//! **Bounded.** A listing returns at most [`LS_MAX_ENTRIES`] entries,
//! directories first and then files, each group in bytewise name order, and
//! says `truncated: true` when more existed (it keeps the smallest N while
//! scanning, so the answer is the same however the directory is ordered on
//! disk). A read serves one chunk of at most
//! [`CHUNK_MAX_BYTES`](crate::runner::upload::CHUNK_MAX_BYTES) and refuses
//! a file over [`FILE_MAX_BYTES`](crate::runner::upload::FILE_MAX_BYTES)
//! (`too-large`) before any body byte, whatever the offset.

use std::path::{Component, Path, PathBuf};

use crate::runner::upload::{CHUNK_MAX_BYTES, FILE_MAX_BYTES};
use crate::runner::RunnerError;
use crate::wire::{FsEntry, FsGetData, FsLsData};

/// Most entries one listing returns.
pub const LS_MAX_ENTRIES: usize = 500;

/// Most directory entries one listing will scan before it gives up and
/// reports `truncated` (a runaway directory must not pin a worker).
pub const LS_SCAN_CAP: usize = 100_000;

/// Longest client `path` accepted, in bytes (the Linux `PATH_MAX`).
pub const PATH_MAX_BYTES: usize = 4096;

/// A readable root of a session's working area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    /// The launch-recorded working directory.
    Cwd,
    /// The session's upload area.
    Up,
}

impl Root {
    /// Parse the wire spelling (`cwd` or `up`).
    pub fn parse(s: &str) -> Result<Root, RunnerError> {
        match s {
            "cwd" => Ok(Root::Cwd),
            "up" => Ok(Root::Up),
            _ => Err(RunnerError::BadRequest(
                "root must be `cwd` or `up`".to_string(),
            )),
        }
    }

    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Root::Cwd => "cwd",
            Root::Up => "up",
        }
    }
}

/// Turn a client path into the relative path handed to the kernel, plus the
/// lexically cleaned form used for display. Pure; no file system access.
///
/// * a path over [`PATH_MAX_BYTES`] or with a NUL is `bad-request`;
/// * an absolute path must lie under `root` (component-wise
///   `strip_prefix`), else `outside-root`;
/// * the empty path and `.` name the root itself.
///
/// A `..` is NOT judged here: the kernel decides (`RESOLVE_BENEATH`), so a
/// `a/../b` that stays inside works and one that climbs out is refused
/// there, not by string inspection.
pub fn split_path(root: &Path, path: &str) -> Result<(PathBuf, String), RunnerError> {
    if path.len() > PATH_MAX_BYTES {
        return Err(RunnerError::BadRequest(format!(
            "path is longer than {PATH_MAX_BYTES} bytes"
        )));
    }
    if path.contains('\0') {
        return Err(RunnerError::BadRequest(
            "path contains a NUL byte".to_string(),
        ));
    }
    let given = Path::new(path);
    let rel: PathBuf = if given.is_absolute() {
        given
            .strip_prefix(root)
            .map_err(|_| RunnerError::OutsideRoot("path is outside the root".to_string()))?
            .to_path_buf()
    } else {
        given.to_path_buf()
    };
    let mut clean: Vec<String> = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(n) => clean.push(n.to_string_lossy().into_owned()),
            Component::ParentDir => {
                clean.pop();
            }
            _ => {}
        }
    }
    let rel = if rel.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        rel
    };
    Ok((rel, clean.join("/")))
}

/// List the directory `path` under `root`. `root_name` is only echoed.
/// Blocking: run it off the async workers.
pub fn fs_ls(root: &Path, root_name: &str, path: &str) -> Result<FsLsData, RunnerError> {
    let (rel, dir) = split_path(root, path)?;
    sys::ls(root, &rel).map(|(entries, truncated, skipped)| FsLsData {
        root: root_name.to_string(),
        dir,
        entries,
        truncated,
        skipped,
    })
}

/// Read up to `len` bytes of the file `path` under `root`, from `offset`.
/// Returns the header (`len` is the byte count actually returned) and the
/// bytes. Blocking: run it off the async workers.
pub fn fs_get(
    root: &Path,
    path: &str,
    offset: u64,
    len: u64,
) -> Result<(FsGetData, Vec<u8>), RunnerError> {
    if len > CHUNK_MAX_BYTES {
        return Err(RunnerError::TooLarge(format!(
            "chunk over {CHUNK_MAX_BYTES} bytes"
        )));
    }
    let (rel, clean) = split_path(root, path)?;
    let name = clean.rsplit('/').next().unwrap_or_default().to_string();
    let (size, mtime_unix, bytes) = sys::get(root, &rel, offset, len, FILE_MAX_BYTES)?;
    Ok((
        FsGetData {
            size,
            len: bytes.len() as u64,
            mtime_unix,
            name,
        },
        bytes,
    ))
}

#[cfg(target_os = "linux")]
mod sys {
    use std::collections::BTreeMap;
    use std::ffi::{CStr, CString, OsStr};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileExt;
    use std::path::Path;

    use super::{FsEntry, RunnerError, LS_MAX_ENTRIES, LS_SCAN_CAP};

    /// `openat2(2)`'s syscall number: the same on every architecture
    /// (the unified numbering above 423).
    const SYS_OPENAT2: libc::c_long = 437;
    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;
    const RESOLVE_BENEATH: u64 = 0x08;

    /// `struct open_how` of `openat2(2)`.
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    fn errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    /// Map the errno of an `openat2` (or the root `open`) to a typed refusal.
    fn map_open_errno(e: i32) -> RunnerError {
        match e {
            libc::ENOENT | libc::ENOTDIR => {
                RunnerError::NotFound("no such file or directory".to_string())
            }
            // RESOLVE_BENEATH: the path climbs out of the root.
            libc::EXDEV => RunnerError::OutsideRoot("path escapes the root".to_string()),
            // RESOLVE_NO_SYMLINKS / NO_MAGICLINKS / O_NOFOLLOW, and a loop.
            libc::ELOOP => RunnerError::NotAFile("symbolic links are not followed".to_string()),
            // Opening a socket (or a fifo with no reader) for reading.
            libc::ENXIO => RunnerError::NotAFile("not a regular file".to_string()),
            libc::ENAMETOOLONG => RunnerError::BadRequest("path too long".to_string()),
            libc::EACCES => RunnerError::Io("permission denied".to_string()),
            libc::ENOSYS | libc::EPERM | libc::EINVAL | libc::E2BIG => RunnerError::Internal(
                "openat2 is unavailable here; file access is refused".to_string(),
            ),
            other => RunnerError::Io(format!("open failed (errno {other})")),
        }
    }

    /// Open the root directory (a trusted path: the recorded launch cwd or
    /// the runner's own session dir) as a path descriptor.
    fn open_root(root: &Path) -> Result<OwnedFd, RunnerError> {
        let c = CString::new(root.as_os_str().as_bytes())
            .map_err(|_| RunnerError::BadRequest("root contains a NUL byte".to_string()))?;
        // SAFETY: `c` is a valid NUL-terminated string for the call.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(map_open_errno(errno()));
        }
        // SAFETY: `open` returned a fresh descriptor that nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Resolve `rel` beneath `dirfd` in the kernel and open it read-only.
    fn open_beneath(dirfd: RawFd, rel: &Path) -> Result<OwnedFd, RunnerError> {
        let c = CString::new(OsStr::as_bytes(rel.as_os_str()))
            .map_err(|_| RunnerError::BadRequest("path contains a NUL byte".to_string()))?;
        let how = OpenHow {
            flags: (libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK) as u64,
            mode: 0,
            resolve: RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
        };
        // SAFETY: `c` and `how` outlive the call; the size is `how`'s.
        let fd = unsafe {
            libc::syscall(
                SYS_OPENAT2,
                dirfd,
                c.as_ptr(),
                &how as *const OpenHow,
                std::mem::size_of::<OpenHow>(),
            )
        };
        if fd < 0 {
            return Err(map_open_errno(errno()));
        }
        // SAFETY: the syscall returned a fresh descriptor nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
    }

    fn fstat(fd: RawFd) -> Result<libc::stat, RunnerError> {
        // SAFETY: a zeroed `stat` is a valid out-parameter.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is open and `st` is writable.
        if unsafe { libc::fstat(fd, &mut st) } != 0 {
            return Err(RunnerError::Io(format!("stat failed (errno {})", errno())));
        }
        Ok(st)
    }

    fn kind(st: &libc::stat) -> u32 {
        st.st_mode & libc::S_IFMT
    }

    /// Owns a `DIR*` and closes it (and its descriptor) on drop.
    struct Dir(*mut libc::DIR);

    impl Drop for Dir {
        fn drop(&mut self) {
            // SAFETY: the pointer came from a successful `fdopendir`.
            unsafe { libc::closedir(self.0) };
        }
    }

    pub fn ls(root: &Path, rel: &Path) -> Result<(Vec<FsEntry>, bool, u32), RunnerError> {
        let rootfd = open_root(root)?;
        let fd = open_beneath(rootfd.as_raw_fd(), rel)?;
        let st = fstat(fd.as_raw_fd())?;
        if kind(&st) != libc::S_IFDIR {
            return Err(RunnerError::NotAFile("not a directory".to_string()));
        }
        // `fdopendir` takes ownership of the descriptor it is given; the
        // original stays ours for the per-entry `fstatat`.
        // SAFETY: `fd` is open.
        let dup = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if dup < 0 {
            return Err(RunnerError::Io(format!("dup failed (errno {})", errno())));
        }
        // SAFETY: `dup` is a fresh descriptor; on failure it stays ours.
        let dirp = unsafe { libc::fdopendir(dup) };
        if dirp.is_null() {
            let e = errno();
            // SAFETY: `fdopendir` failed, so `dup` was not consumed.
            unsafe { libc::close(dup) };
            return Err(map_open_errno(e));
        }
        let dir = Dir(dirp);
        // Smallest-N retention: key = (kind rank, name bytes).
        let mut keep: BTreeMap<(u8, Vec<u8>), FsEntry> = BTreeMap::new();
        let mut truncated = false;
        let mut skipped: u32 = 0;
        let mut scanned = 0usize;
        loop {
            // SAFETY: the errno location is thread-local and always valid.
            unsafe { *libc::__errno_location() = 0 };
            // SAFETY: `dir.0` is a live `DIR*`.
            let ent = unsafe { libc::readdir(dir.0) };
            if ent.is_null() {
                let e = errno();
                if e != 0 {
                    return Err(RunnerError::Io(format!("readdir failed (errno {e})")));
                }
                break;
            }
            scanned += 1;
            if scanned > LS_SCAN_CAP {
                truncated = true;
                break;
            }
            // SAFETY: `d_name` is a NUL-terminated name inside the entry.
            let cname = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
            let raw = cname.to_bytes();
            if raw == b"." || raw == b".." {
                continue;
            }
            let name = match std::str::from_utf8(raw) {
                Ok(n) if !n.chars().any(char::is_control) => n.to_string(),
                _ => {
                    skipped += 1;
                    continue;
                }
            };
            // SAFETY: a zeroed `stat` is a valid out-parameter.
            let mut est: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: `fd` is the open directory, `cname` a plain name
            // (readdir never yields a slash) and `est` is writable.
            let rc = unsafe {
                libc::fstatat(
                    fd.as_raw_fd(),
                    cname.as_ptr(),
                    &mut est,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc != 0 {
                skipped += 1;
                continue;
            }
            let (rank, kind_name, bytes) = match kind(&est) {
                libc::S_IFDIR => (0u8, "dir", 0u64),
                libc::S_IFREG => (1u8, "file", est.st_size.max(0) as u64),
                _ => {
                    skipped += 1;
                    continue;
                }
            };
            let hidden = name.starts_with('.');
            keep.insert(
                (rank, raw.to_vec()),
                FsEntry {
                    name,
                    kind: kind_name.to_string(),
                    bytes,
                    mtime_unix: est.st_mtime,
                    hidden,
                },
            );
            if keep.len() > LS_MAX_ENTRIES {
                keep.pop_last();
                truncated = true;
            }
        }
        Ok((keep.into_values().collect(), truncated, skipped))
    }

    pub fn get(
        root: &Path,
        rel: &Path,
        offset: u64,
        len: u64,
        file_cap: u64,
    ) -> Result<(u64, i64, Vec<u8>), RunnerError> {
        let rootfd = open_root(root)?;
        let fd = open_beneath(rootfd.as_raw_fd(), rel)?;
        let st = fstat(fd.as_raw_fd())?;
        if kind(&st) != libc::S_IFREG {
            return Err(RunnerError::NotAFile("not a regular file".to_string()));
        }
        let size = st.st_size.max(0) as u64;
        if size > file_cap {
            return Err(RunnerError::TooLarge(format!(
                "file is over the {file_cap} byte download cap"
            )));
        }
        if offset > size {
            return Err(RunnerError::BadRequest(
                "offset is past the end of the file".to_string(),
            ));
        }
        let want = len.min(size - offset) as usize;
        let file = std::fs::File::from(fd);
        let mut buf = vec![0u8; want];
        let mut got = 0usize;
        while got < want {
            match file.read_at(&mut buf[got..], offset + got as u64) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(RunnerError::Io(format!("read failed: {e}"))),
            }
        }
        // A file that shrank under the read serves what existed.
        buf.truncate(got);
        Ok((size, st.st_mtime, buf))
    }
}

#[cfg(not(target_os = "linux"))]
mod sys {
    use std::path::Path;

    use super::{FsEntry, RunnerError};

    fn unsupported() -> RunnerError {
        RunnerError::Internal(
            "openat2 is unavailable on this platform; file access is refused".to_string(),
        )
    }

    pub fn ls(_root: &Path, _rel: &Path) -> Result<(Vec<FsEntry>, bool, u32), RunnerError> {
        Err(unsupported())
    }

    pub fn get(
        _root: &Path,
        _rel: &Path,
        _offset: u64,
        _len: u64,
        _file_cap: u64,
    ) -> Result<(u64, i64, Vec<u8>), RunnerError> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_path_handles_relative_absolute_and_garbage() {
        let root = Path::new("/w/proj");
        let (rel, dir) = split_path(root, "").unwrap();
        assert_eq!((rel, dir.as_str()), (PathBuf::from("."), ""));
        let (rel, dir) = split_path(root, "a//b/./c/").unwrap();
        assert_eq!((rel, dir.as_str()), (PathBuf::from("a//b/./c/"), "a/b/c"));
        // An absolute path under the root loses the root, component-wise.
        let (rel, dir) = split_path(root, "/w/proj/src/x.rs").unwrap();
        assert_eq!((rel, dir.as_str()), (PathBuf::from("src/x.rs"), "src/x.rs"));
        let (rel, _) = split_path(root, "/w/proj").unwrap();
        assert_eq!(rel, PathBuf::from("."));
        // A string-prefix sibling is NOT under the root.
        for bad in ["/w/proj2/x", "/w", "/etc/passwd", "/"] {
            let e = split_path(root, bad).unwrap_err();
            assert_eq!(e.code(), "outside-root", "{bad}");
        }
        assert_eq!(split_path(root, "a\0b").unwrap_err().code(), "bad-request");
        let long = "a/".repeat(PATH_MAX_BYTES);
        assert_eq!(split_path(root, &long).unwrap_err().code(), "bad-request");
        // `..` is the kernel's to judge; the display form is lexically cleaned.
        let (rel, dir) = split_path(root, "a/../../b").unwrap();
        assert_eq!(rel, PathBuf::from("a/../../b"));
        assert_eq!(dir, "b");
    }

    #[test]
    fn root_grammar_is_closed() {
        assert_eq!(Root::parse("cwd").unwrap(), Root::Cwd);
        assert_eq!(Root::parse("up").unwrap(), Root::Up);
        for bad in ["", "CWD", "home", "cwd ", "/"] {
            assert_eq!(Root::parse(bad).unwrap_err().code(), "bad-request", "{bad}");
        }
    }
}
