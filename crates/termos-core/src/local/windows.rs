// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows named pipes (DECISIONS.md D-036).
//!
//! - Every pipe instance carries a protected DACL that grants access to the
//!   current user's SID only, and rejects remote clients.
//! - The first instance is created with `first_pipe_instance`, so a second
//!   server on the same name is refused ("already running").
//! - The server checks the SID of each client's process; the client checks
//!   the SID of the server's process before saying anything, so a process
//!   that grabbed our pipe name first ("pipe squatting") is refused.
//! - Clients connect with `SECURITY_ANONYMOUS`: a server can never act with
//!   the client's identity.

use std::ffi::OsStr;
use std::io;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};

pub(super) type Peer = String;
pub(super) type Read = ReadHalf<Pipe>;
pub(super) type Write = WriteHalf<Pipe>;

/// `SECURITY_ANONYMOUS` (winbase.h): the server may not impersonate us.
const SECURITY_ANONYMOUS: u32 = 0;
/// `ERROR_PIPE_BUSY`: every instance is in use; retry shortly.
const ERROR_PIPE_BUSY: i32 = 231;
/// `ERROR_ACCESS_DENIED`: creating the first instance of a name that exists.
const ERROR_ACCESS_DENIED: i32 = 5;

pub(super) fn current_peer() -> io::Result<Peer> {
    security::current_user_sid()
}

pub(super) fn nobody() -> Peer {
    // The Null SID: no account has it.
    String::from("S-1-0-0")
}

/// Either end of a pipe.
#[derive(Debug)]
pub(super) enum Pipe {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}

impl AsyncRead for Pipe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Pipe::Server(p) => Pin::new(p).poll_read(cx, buf),
            Pipe::Client(p) => Pin::new(p).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Pipe {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Pipe::Server(p) => Pin::new(p).poll_write(cx, buf),
            Pipe::Client(p) => Pin::new(p).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Pipe::Server(p) => Pin::new(p).poll_flush(cx),
            Pipe::Client(p) => Pin::new(p).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Pipe::Server(p) => Pin::new(p).poll_shutdown(cx),
            Pipe::Client(p) => Pin::new(p).poll_shutdown(cx),
        }
    }
}

/// How many pipe instances wait for clients at once. A client that finds
/// none free gets `ERROR_PIPE_BUSY`; ours retry, but plugins in other
/// languages simply `open()` the pipe, and several start together.
const WAITING: usize = 4;

#[derive(Debug)]
pub(super) struct Listener {
    name: std::ffi::OsString,
    descriptor: security::Descriptor,
    /// The first instance, created by `bind` to claim the name; it starts
    /// waiting with the first `accept`.
    first: Option<NamedPipeServer>,
    /// Instances waiting for a client, each in its own task (a pending
    /// overlapped connect: no wakeups until a client arrives).
    waiting: tokio::task::JoinSet<io::Result<NamedPipeServer>>,
}

fn create(
    name: &OsStr,
    descriptor: &security::Descriptor,
    first: bool,
) -> io::Result<NamedPipeServer> {
    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first).reject_remote_clients(true);
    security::create_pipe(&opts, name, descriptor)
}

impl Listener {
    pub(super) fn bind(path: &Path) -> io::Result<Self> {
        let name = path.as_os_str().to_owned();
        let descriptor = security::Descriptor::current_user_only()?;
        let next = create(&name, &descriptor, true).map_err(|e| {
            if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
                io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("a TermOS server is already running at {}", path.display()),
                )
            } else {
                e
            }
        })?;
        Ok(Self {
            name,
            descriptor,
            first: Some(next),
            waiting: tokio::task::JoinSet::new(),
        })
    }

    pub(super) async fn accept(&mut self) -> io::Result<Stream> {
        while self.waiting.len() < WAITING {
            let pipe = match self.first.take() {
                Some(p) => p,
                None => create(&self.name, &self.descriptor, false)?,
            };
            self.waiting.spawn(async move {
                pipe.connect().await?;
                Ok(pipe)
            });
        }
        match self.waiting.join_next().await {
            Some(Ok(connected)) => connected.map(|p| Stream(Pipe::Server(p))),
            Some(Err(e)) => Err(io::Error::other(e)),
            None => Err(io::Error::other("no pipe instance is waiting")),
        }
    }
}

#[derive(Debug)]
pub(super) struct Stream(Pipe);

impl Stream {
    pub(super) async fn connect(path: &Path) -> io::Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let client = loop {
            match ClientOptions::new()
                .security_qos_flags(SECURITY_ANONYMOUS)
                .open(path.as_os_str())
            {
                Ok(c) => break c,
                Err(e)
                    if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(e) => return Err(e),
            }
        };
        let server = security::pipe_server_sid(client.as_raw_handle())?;
        if server != security::current_user_sid()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is served by another user's process; refusing to connect",
                    path.display()
                ),
            ));
        }
        Ok(Self(Pipe::Client(client)))
    }

    pub(super) fn peer(&self) -> io::Result<Peer> {
        match &self.0 {
            Pipe::Server(p) => security::pipe_client_sid(p.as_raw_handle()),
            Pipe::Client(p) => security::pipe_server_sid(p.as_raw_handle()),
        }
    }

    pub(super) fn into_split(self) -> (Read, Write) {
        tokio::io::split(self.0)
    }
}

pub(super) fn probe(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt as _;
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .security_qos_flags(SECURITY_ANONYMOUS)
        .open(path)
    {
        Ok(_) => true,
        Err(e) => e.raw_os_error() == Some(ERROR_PIPE_BUSY),
    }
}

pub(super) fn exists(path: &Path) -> bool {
    probe(path)
}

pub(super) fn remove_endpoint(_path: &Path) {
    // Pipes disappear with their last instance.
}

/// The few Win32 calls the safe wrappers above need.
#[allow(unsafe_code)]
mod security {
    use std::ffi::{OsStr, c_void};
    use std::io;
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};

    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::System::Pipes::{
        GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// A security descriptor built from SDDL, freed on drop.
    #[derive(Debug)]
    pub(super) struct Descriptor(PSECURITY_DESCRIPTOR);

    // SAFETY: the descriptor is never modified after creation; Windows only
    // reads it when a pipe instance is created, from whichever thread.
    unsafe impl Send for Descriptor {}
    // SAFETY: as above, shared access is read-only.
    unsafe impl Sync for Descriptor {}

    impl Descriptor {
        /// Full access for the current user's SID, nothing for anyone else
        /// (protected: no inherited entries).
        pub(super) fn current_user_only() -> io::Result<Self> {
            let sddl = format!("D:P(A;;GA;;;{})", current_user_sid()?);
            let sddl = wide(OsStr::new(&sddl));
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            // SAFETY: `sddl` is a NUL-terminated UTF-16 string that lives for
            // the call; `sd` is a valid out-pointer; the size out-pointer may
            // be null.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &raw mut sd,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 || sd.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(sd))
        }
    }

    impl Drop for Descriptor {
        fn drop(&mut self) {
            // SAFETY: the pointer came from
            // ConvertStringSecurityDescriptorToSecurityDescriptorW, which
            // allocates with LocalAlloc, and is freed exactly once.
            unsafe {
                LocalFree(self.0);
            }
        }
    }

    /// Creates a pipe instance protected by `descriptor`.
    pub(super) fn create_pipe(
        opts: &ServerOptions,
        name: &OsStr,
        descriptor: &Descriptor,
    ) -> io::Result<NamedPipeServer> {
        let mut attrs = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        // SAFETY: `attrs` is a valid SECURITY_ATTRIBUTES for the duration of
        // the call, and its descriptor is kept alive by `descriptor`.
        unsafe { opts.create_with_security_attributes_raw(name, (&raw mut attrs).cast::<c_void>()) }
    }

    /// The user SID of the process `process`, as a string (`S-1-5-21-…`).
    fn process_sid(process: HANDLE) -> io::Result<String> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: `process` is a valid process handle with query access;
        // `token` is a valid out-pointer.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: OpenProcessToken succeeded, so `token` is an open handle
        // that we now own and close on drop.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut len = 0u32;
        // SAFETY: a size query: null buffer of length 0, valid out-pointer.
        // It fails with ERROR_INSUFFICIENT_BUFFER and sets `len`.
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &raw mut len,
            );
        }
        // u64 elements keep the TOKEN_USER header aligned.
        let mut buf = vec![0u64; usize::try_from(len).unwrap_or(0).div_ceil(8).max(1)];
        let cap = u32::try_from(buf.len() * 8).unwrap_or(u32::MAX);
        // SAFETY: `buf` has room for `cap` bytes and outlives the call.
        let ok = unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buf.as_mut_ptr().cast::<c_void>(),
                cap,
                &raw mut len,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: GetTokenInformation(TokenUser) filled `buf` with a
        // TOKEN_USER followed by the SID it points to; `buf` is aligned for
        // it and still alive.
        let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let mut text: *mut u16 = std::ptr::null_mut();
        // SAFETY: `sid` points into `buf` (alive); `text` is a valid
        // out-pointer that receives a LocalAlloc'd string.
        if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `text` is a NUL-terminated UTF-16 string from
        // ConvertSidToStringSidW; we read up to the NUL, then free it once.
        let s = unsafe {
            let mut n = 0usize;
            while *text.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
            LocalFree(text.cast::<c_void>());
            s
        };
        Ok(s)
    }

    pub(super) fn current_user_sid() -> io::Result<String> {
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
        // closing and is always valid.
        process_sid(unsafe { GetCurrentProcess() })
    }

    fn pid_sid(pid: u32) -> io::Result<String> {
        // SAFETY: plain call; a null result is checked below.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "cannot check the other process's user ({}); run every TermOS process with the same elevation",
                    io::Error::last_os_error()
                ),
            ));
        }
        // SAFETY: OpenProcess succeeded; we own the handle and close it on
        // drop.
        let h = unsafe { OwnedHandle::from_raw_handle(h) };
        process_sid(h.as_raw_handle())
    }

    /// The user of the process at the client end of a server pipe.
    pub(super) fn pipe_client_sid(pipe: RawHandle) -> io::Result<String> {
        let mut pid = 0u32;
        // SAFETY: `pipe` is the open handle of a connected server instance;
        // `pid` is a valid out-pointer.
        if unsafe { GetNamedPipeClientProcessId(pipe, &raw mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        pid_sid(pid)
    }

    /// The user of the process that serves a client pipe.
    pub(super) fn pipe_server_sid(pipe: RawHandle) -> io::Result<String> {
        let mut pid = 0u32;
        // SAFETY: `pipe` is the open handle of a connected client; `pid` is
        // a valid out-pointer.
        if unsafe { GetNamedPipeServerProcessId(pipe, &raw mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        pid_sid(pid)
    }
}
