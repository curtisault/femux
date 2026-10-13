//! POSIX shared memory for the probe's Kitty `t=s` query (ADR-021 #4): a three-byte image the
//! host reads and unlinks when it supports the medium.

use std::ffi::CString;
use std::io;

/// Creates the shared memory object `name` holding `bytes`.
pub fn write(name: &str, bytes: &[u8]) -> io::Result<()> {
    let c = CString::new(name).map_err(io::Error::other)?;
    // SAFETY: plain libc calls on a fresh object; the mapping is `bytes.len()` long and is
    // unmapped before returning (mmap rather than write(), which macOS refuses for shm).
    unsafe {
        let fd = libc::shm_open(
            c.as_ptr(),
            libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
            0o600 as libc::c_uint,
        );
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let ok = libc::ftruncate(fd, bytes.len() as libc::off_t) == 0 && {
            let map = libc::mmap(
                std::ptr::null_mut(),
                bytes.len(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
            if map == libc::MAP_FAILED {
                false
            } else {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), map as *mut u8, bytes.len());
                libc::munmap(map, bytes.len());
                true
            }
        };
        libc::close(fd);
        if !ok {
            let e = io::Error::last_os_error();
            libc::shm_unlink(c.as_ptr());
            return Err(e);
        }
    }
    Ok(())
}

/// Removes `name`; a host that read it already did.
pub fn unlink(name: &str) {
    if let Ok(c) = CString::new(name) {
        // SAFETY: a plain libc call.
        unsafe { libc::shm_unlink(c.as_ptr()) };
    }
}
