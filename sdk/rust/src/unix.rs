//! Unix socket helpers for plugins that run a private per-user daemon.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

/// Create `path` (mode 0700) if needed and verify it is a directory owned by
/// the current user, refusing squatted paths in shared locations like `/tmp`.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no preconditions.
    if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other(format!(
            "{} is not a directory owned by the current user",
            path.display()
        )));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Bind a listener whose socket file only the current user can reach.
pub fn bind_private_socket(path: &Path) -> io::Result<UnixListener> {
    // SAFETY: umask has no preconditions.
    let previous_umask = unsafe { libc::umask(0o077) };
    let result = UnixListener::bind(path);
    // SAFETY: umask has no preconditions.
    unsafe {
        libc::umask(previous_umask);
    }
    result
}

/// Remove a socket file, treating an already-missing file as success.
pub fn remove_socket(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Unlinks the wrapped socket path on drop.
pub struct SocketCleanup<'a>(pub &'a Path);

impl Drop for SocketCleanup<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

/// Verify the connected peer runs as the current effective user.
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
pub fn peer_is_current_user(stream: &UnixStream) -> io::Result<()> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: both output pointers are valid and stream owns a live descriptor.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    require_current_user(uid)
}

/// Verify the connected peer runs as the current effective user.
#[cfg(target_os = "linux")]
pub fn peer_is_current_user(stream: &UnixStream) -> io::Result<()> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: credentials and length are valid writable values for getsockopt.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    require_current_user(credentials.uid)
}

fn require_current_user(uid: libc::uid_t) -> io::Result<()> {
    // SAFETY: geteuid has no preconditions.
    if uid == unsafe { libc::geteuid() } {
        Ok(())
    } else {
        Err(io::Error::other("peer belongs to another user"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("herdr-client-unix-{name}-{}", std::process::id()))
    }

    #[test]
    fn private_dir_is_created_with_owner_only_permissions() {
        let path = temp_path("dir");
        let _ = fs::remove_dir(&path);
        create_private_dir(&path).unwrap();
        create_private_dir(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        fs::remove_dir(&path).unwrap();
    }

    #[test]
    fn private_dir_rejects_a_squatted_path() {
        let path = temp_path("squat");
        let _ = fs::remove_file(&path);
        fs::write(&path, b"").unwrap();
        assert!(create_private_dir(&path).is_err());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn private_socket_is_bound_with_owner_only_permissions() {
        let path = temp_path("sock");
        let _ = fs::remove_file(&path);
        let listener = bind_private_socket(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);
        drop(listener);
        remove_socket(&path).unwrap();
        remove_socket(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn socket_cleanup_unlinks_on_drop() {
        let path = temp_path("cleanup");
        fs::write(&path, b"").unwrap();
        drop(SocketCleanup(&path));
        assert!(!path.exists());
    }

    #[test]
    fn peers_in_the_same_process_pass_the_user_check() {
        let (client, _server) = UnixStream::pair().unwrap();
        peer_is_current_user(&client).unwrap();
    }
}
