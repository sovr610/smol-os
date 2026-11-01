// SPDX-License-Identifier: GPL-3.0-or-later
//! The plugin's connection to the server: a Unix socket, or a Windows
//! named pipe, with reads that can time out.

use std::io;
use std::path::Path;
use std::time::Duration;

/// Reads with an optional timeout and blocking writes.
pub(crate) trait Conn: Send {
    /// Appends bytes up to and including the next `\n` to `buf`. Returns
    /// how many bytes were appended; 0 means the server hung up. Bytes of
    /// an unfinished line are appended before end of stream. When nothing
    /// complete arrives within `timeout`, fails with
    /// [`io::ErrorKind::TimedOut`] or [`io::ErrorKind::WouldBlock`] and
    /// keeps what it has for the next call.
    fn read_line(&mut self, buf: &mut Vec<u8>, timeout: Option<Duration>) -> io::Result<usize>;

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
}

/// Connects to the endpoint the server gave the plugin.
pub(crate) fn connect(endpoint: &Path) -> io::Result<Box<dyn Conn>> {
    sys::connect(endpoint)
}

#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
use windows as sys;

#[cfg(unix)]
mod unix {
    use std::io::{self, BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    pub(super) struct Socket {
        reader: BufReader<UnixStream>,
        writer: UnixStream,
    }

    pub(super) fn connect(path: &Path) -> io::Result<Box<dyn super::Conn>> {
        let stream = UnixStream::connect(path)?;
        let writer = stream.try_clone()?;
        Ok(Box::new(Socket {
            reader: BufReader::new(stream),
            writer,
        }))
    }

    impl super::Conn for Socket {
        fn read_line(&mut self, buf: &mut Vec<u8>, timeout: Option<Duration>) -> io::Result<usize> {
            match self.reader.get_ref().set_read_timeout(timeout) {
                // macOS refuses socket options once the server has hung
                // up; reading then returns what is left, then end of
                // stream.
                Err(e) if e.kind() == io::ErrorKind::InvalidInput => {}
                other => other?,
            }
            // A timeout leaves the bytes read so far in `buf`.
            self.reader.read_until(b'\n', buf)
        }

        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.writer.write_all(bytes)
        }
    }
}

#[cfg(windows)]
mod windows {
    //! A synchronous pipe handle serializes its I/O: a thread blocked in a
    //! read would block every write. The pipe is opened for overlapped I/O
    //! instead, so a read can wait with a timeout and be cancelled.

    use std::io;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED;

    /// `SECURITY_ANONYMOUS` (winbase.h): the server may not act as us.
    const SECURITY_ANONYMOUS: u32 = 0;
    const ERROR_PIPE_BUSY: i32 = 231;

    pub(super) struct Pipe {
        io: overlapped::Io,
        /// Bytes read but not yet returned (after the last `\n`).
        pending: Vec<u8>,
    }

    pub(super) fn connect(path: &Path) -> io::Result<Box<dyn super::Conn>> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let file = loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(FILE_FLAG_OVERLAPPED)
                .security_qos_flags(SECURITY_ANONYMOUS)
                .open(path)
            {
                Ok(f) => break f,
                Err(e)
                    if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(e),
            }
        };
        Ok(Box::new(Pipe {
            io: overlapped::Io::new(file)?,
            pending: Vec::new(),
        }))
    }

    impl super::Conn for Pipe {
        fn read_line(&mut self, buf: &mut Vec<u8>, timeout: Option<Duration>) -> io::Result<usize> {
            let deadline = timeout.map(|t| Instant::now() + t);
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                if let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
                    buf.extend(self.pending.drain(..=pos));
                    return Ok(pos + 1);
                }
                let left = match deadline {
                    Some(d) => {
                        let left = d.saturating_duration_since(Instant::now());
                        if left.is_zero() {
                            return Err(io::ErrorKind::TimedOut.into());
                        }
                        Some(left)
                    }
                    None => None,
                };
                match self.io.read(&mut chunk, left)? {
                    0 => {
                        // End of stream: hand over an unfinished line.
                        let n = self.pending.len();
                        buf.append(&mut self.pending);
                        return Ok(n);
                    }
                    n => self.pending.extend_from_slice(&chunk[..n]),
                }
            }
        }

        fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
            while !bytes.is_empty() {
                let n = self.io.write(bytes)?;
                if n == 0 {
                    return Err(io::ErrorKind::WriteZero.into());
                }
                bytes = &bytes[n..];
            }
            Ok(())
        }
    }

    /// Overlapped reads and writes on one handle, one at a time.
    #[allow(unsafe_code)]
    mod overlapped {
        use std::fs::File;
        use std::io;
        use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
        use std::time::Duration;

        use windows_sys::Win32::Foundation::{
            ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        };
        use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
        use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
        use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};

        pub(super) struct Io {
            file: File,
            /// Signalled when an operation completes.
            event: OwnedHandle,
        }

        fn is_eof(code: Option<i32>) -> bool {
            code.and_then(|c| u32::try_from(c).ok())
                .is_some_and(|c| c == ERROR_BROKEN_PIPE || c == ERROR_HANDLE_EOF)
        }

        impl Io {
            pub(super) fn new(file: File) -> io::Result<Self> {
                // SAFETY: plain call creating an unnamed manual-reset event;
                // a null result is checked below.
                let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
                if event.is_null() {
                    return Err(io::Error::last_os_error());
                }
                Ok(Self {
                    file,
                    // SAFETY: CreateEventW succeeded; we own the handle.
                    event: unsafe { OwnedHandle::from_raw_handle(event) },
                })
            }

            fn overlapped(&self) -> OVERLAPPED {
                // SAFETY: OVERLAPPED is a plain C struct (integers, a union
                // of integers and pointers, a handle) for which all-zero is
                // the documented initial state.
                let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
                ov.hEvent = self.event.as_raw_handle();
                ov
            }

            /// Reads at most `buf.len()` bytes, waiting at most `timeout`.
            /// 0 means end of stream.
            pub(super) fn read(
                &mut self,
                buf: &mut [u8],
                timeout: Option<Duration>,
            ) -> io::Result<usize> {
                let h = self.file.as_raw_handle();
                let mut ov = self.overlapped();
                let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
                // SAFETY: `buf` and `ov` outlive the operation: it either
                // completes before we return or is cancelled and waited
                // for below.
                let ok = unsafe {
                    ReadFile(h, buf.as_mut_ptr(), len, std::ptr::null_mut(), &raw mut ov)
                };
                if ok == 0 {
                    let err = io::Error::last_os_error();
                    if is_eof(err.raw_os_error()) {
                        return Ok(0);
                    }
                    if err.raw_os_error() != i32::try_from(ERROR_IO_PENDING).ok() {
                        return Err(err);
                    }
                    let ms = timeout.map_or(INFINITE, |t| {
                        u32::try_from(t.as_millis()).unwrap_or(INFINITE - 1).max(1)
                    });
                    // SAFETY: the event handle is valid for our lifetime.
                    let waited = unsafe { WaitForSingleObject(self.event.as_raw_handle(), ms) };
                    if waited == WAIT_TIMEOUT {
                        // SAFETY: `ov` identifies our pending read on `h`.
                        unsafe {
                            CancelIoEx(h, &raw const ov);
                        }
                    } else if waited != WAIT_OBJECT_0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                let mut n = 0u32;
                // SAFETY: waits (bWait = TRUE) for the operation, completed
                // or cancelled, so `buf` and `ov` are no longer in use
                // afterwards.
                let ok = unsafe { GetOverlappedResult(h, &raw const ov, &raw mut n, 1) };
                if ok == 0 {
                    let err = io::Error::last_os_error();
                    return match err.raw_os_error() {
                        c if is_eof(c) => Ok(0),
                        c if c == i32::try_from(ERROR_OPERATION_ABORTED).ok() => {
                            Err(io::ErrorKind::TimedOut.into())
                        }
                        _ => Err(err),
                    };
                }
                Ok(usize::try_from(n).unwrap_or(0))
            }

            /// Writes some of `buf`, waiting as long as it takes.
            pub(super) fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let h = self.file.as_raw_handle();
                let mut ov = self.overlapped();
                let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
                // SAFETY: `buf` and `ov` outlive the operation, which is
                // waited for below.
                let ok =
                    unsafe { WriteFile(h, buf.as_ptr(), len, std::ptr::null_mut(), &raw mut ov) };
                if ok == 0 {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error() != i32::try_from(ERROR_IO_PENDING).ok() {
                        return Err(err);
                    }
                }
                let mut n = 0u32;
                // SAFETY: waits for the write to finish before `buf` and
                // `ov` go away.
                if unsafe { GetOverlappedResult(h, &raw const ov, &raw mut n, 1) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(usize::try_from(n).unwrap_or(0))
            }
        }
    }
}
