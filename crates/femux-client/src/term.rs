//! The host terminal: raw mode, the alternate screen, mouse and paste reporting, size, and
//! byte-level reads and writes. termwiz parses the input bytes; everything written is ours,
//! so synchronized output (REQ-UI-050), OSC 52 and Kitty APCs go out verbatim.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::time::Duration;

use termwiz::input::{InputEvent, InputParser};

pub struct Term {
    tty_in: std::fs::File,
    tty_out: std::fs::File,
    orig: Option<libc::termios>,
    entered: bool,
    mouse: bool,
    parser: InputParser,
}

fn errno() -> io::Error {
    io::Error::last_os_error()
}

/// What a panic needs to give the host terminal back before anything is printed: the tty's
/// descriptors, its saved attributes and whether mouse reporting is on (REQ-LOG-005).
type Restore = (i32, i32, Option<libc::termios>, bool);
static RESTORE: std::sync::Mutex<Option<Restore>> = std::sync::Mutex::new(None);

fn leave_sequence(mouse: bool) -> String {
    // The host's own cursor shape back (REQ-VT-002).
    let mut s = String::from("\x1b[0m\x1b[?25h\x1b[0 q");
    if mouse {
        s.push_str("\x1b[?1006l\x1b[?1002l\x1b[?1000l");
    }
    s.push_str("\x1b[?2004l\x1b[2J\x1b[H\x1b[?1049l");
    s
}

/// Restores the host terminal on a panic, then runs the hook that was there (the log's), so
/// the panic is recorded and its message lands on the normal screen.
pub fn restore_on_panic() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Ok(mut g) = RESTORE.try_lock()
                && let Some((tin, tout, orig, mouse)) = g.take()
            {
                let seq = leave_sequence(mouse);
                // SAFETY: writing to and restoring the tty this process opened.
                unsafe {
                    libc::write(tout, seq.as_ptr().cast(), seq.len());
                    if let Some(o) = orig {
                        libc::tcsetattr(tin, libc::TCSANOW, &o);
                    }
                }
            }
            prev(info);
        }));
    });
}

impl Term {
    /// Uses stdin and stdout; both must be terminals.
    pub fn open() -> io::Result<Term> {
        use std::os::fd::FromRawFd;
        // SAFETY: fds 0 and 1 are open for the process lifetime; dup keeps ownership clean.
        let (tty_in, tty_out) = unsafe {
            let i = libc::dup(0);
            let o = libc::dup(1);
            if i < 0 || o < 0 {
                return Err(errno());
            }
            (std::fs::File::from_raw_fd(i), std::fs::File::from_raw_fd(o))
        };
        // SAFETY: isatty has no preconditions.
        if unsafe { libc::isatty(tty_in.as_raw_fd()) } == 0 {
            return Err(io::Error::other("stdin is not a terminal"));
        }
        Ok(Term {
            tty_in,
            tty_out,
            orig: None,
            entered: false,
            mouse: false,
            parser: InputParser::new(),
        })
    }

    pub fn raw(&mut self) -> io::Result<()> {
        if self.orig.is_some() {
            return Ok(());
        }
        // SAFETY: termios is plain data; the fd is valid.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(self.tty_in.as_raw_fd(), &mut t) != 0 {
                return Err(errno());
            }
            let orig = t;
            libc::cfmakeraw(&mut t);
            if libc::tcsetattr(self.tty_in.as_raw_fd(), libc::TCSANOW, &t) != 0 {
                return Err(errno());
            }
            self.orig = Some(orig);
        }
        Ok(())
    }

    /// Alternate screen, bracketed paste, and SGR mouse reporting when `mouse` is on.
    pub fn enter(&mut self, mouse: bool) -> io::Result<()> {
        self.raw()?;
        let mut s = String::from("\x1b[?1049h\x1b[2J\x1b[H\x1b[?2004h");
        if mouse {
            s.push_str("\x1b[?1000h\x1b[?1002h\x1b[?1006h");
        }
        self.mouse = mouse;
        self.entered = true;
        *RESTORE.lock().unwrap_or_else(|e| e.into_inner()) = Some((
            self.tty_in.as_raw_fd(),
            self.tty_out.as_raw_fd(),
            self.orig,
            mouse,
        ));
        self.write(s.as_bytes())
    }

    pub fn leave(&mut self) {
        RESTORE.lock().unwrap_or_else(|e| e.into_inner()).take();
        if self.entered {
            let s = leave_sequence(self.mouse);
            let _ = self.write(s.as_bytes());
            self.entered = false;
        }
        if let Some(orig) = self.orig.take() {
            // SAFETY: restoring the attributes we saved.
            unsafe {
                libc::tcsetattr(self.tty_in.as_raw_fd(), libc::TCSANOW, &orig);
            }
        }
    }

    pub fn size(&self) -> (u16, u16) {
        // SAFETY: TIOCGWINSZ fills a winsize.
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(self.tty_out.as_raw_fd(), libc::TIOCGWINSZ, &mut ws) == 0
                && ws.ws_col > 0
                && ws.ws_row > 0
            {
                (ws.ws_col, ws.ws_row)
            } else {
                (80, 24)
            }
        }
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.tty_out.write_all(bytes)?;
        self.tty_out.flush()
    }

    /// Raw bytes available within `timeout`, or `None` on timeout.
    pub fn read_raw(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let mut pfd = libc::pollfd {
            fd: self.tty_in.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let n = unsafe { libc::poll(&mut pfd, 1, timeout.as_millis() as i32) };
        if n < 0 {
            let e = errno();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(e);
        }
        if n == 0 {
            return Ok(None);
        }
        let mut buf = vec![0u8; 4096];
        let n = self.tty_in.read(&mut buf)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "terminal closed",
            ));
        }
        buf.truncate(n);
        Ok(Some(buf))
    }

    /// Feeds bytes to the input parser; `more` says whether a partial escape may continue.
    pub fn parse(&mut self, bytes: &[u8], more: bool) -> Vec<InputEvent> {
        self.parser.parse_as_vec(bytes, more)
    }

    /// Input events available within `timeout`, returning early (with none) when `wake`
    /// becomes readable; `wake` is drained.
    pub fn read_or_wake(
        &mut self,
        timeout: Duration,
        wake: std::os::fd::RawFd,
    ) -> io::Result<Vec<InputEvent>> {
        let mut fds = [
            libc::pollfd {
                fd: self.tty_in.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: two valid pollfds.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout.as_millis() as i32) };
        if n < 0 {
            let e = errno();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(Vec::new());
            }
            return Err(e);
        }
        if fds[1].revents != 0 {
            let mut buf = [0u8; 256];
            // SAFETY: draining a non-blocking pipe into a local buffer.
            while unsafe { libc::read(wake, buf.as_mut_ptr() as *mut _, buf.len()) } > 0 {}
        }
        if fds[0].revents != 0 {
            return self.read(Duration::ZERO);
        }
        Ok(self.parse(&[], false))
    }

    /// Input events available within `timeout`.
    pub fn read(&mut self, timeout: Duration) -> io::Result<Vec<InputEvent>> {
        match self.read_raw(timeout)? {
            Some(bytes) => Ok(self.parse(&bytes, false)),
            None => Ok(self.parse(&[], false)),
        }
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.leave();
    }
}

/// OSC 52: copy `text` to the host clipboard.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x1b\\", base64(text.as_bytes()))
}

pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_is_base64() {
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b"hi"), "aGk=");
        assert_eq!(base64(b"abc"), "YWJj");
        assert_eq!(osc52("hello"), "\x1b]52;c;aGVsbG8=\x1b\\");
    }
}
