// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows: hears logoff and shutdown, so the server saves its sessions
//! first (D-107).
//!
//! A process that has loaded user32 gets no console control events for
//! logoff and shutdown; Windows tells it through `WM_QUERYENDSESSION` and
//! `WM_ENDSESSION` instead, sent to its top-level windows (message-only
//! windows do not get them). So a hidden window on a thread of its own
//! listens, and on `WM_ENDSESSION` hands the server an acknowledgement to
//! send once the snapshots are written. The window waits for it, up to
//! [`SAVE_WAIT`]: once it returns, Windows may end the process at any
//! moment.

use std::sync::OnceLock;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassW,
    TranslateMessage, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW,
};

/// How long the window holds Windows back while the server saves.
pub const SAVE_WAIT: Duration = Duration::from_secs(5);

/// Where `WM_ENDSESSION` sends its acknowledgement channel.
static SERVER: OnceLock<UnboundedSender<SyncSender<()>>> = OnceLock::new();
/// The window, for tests that send it messages.
static WINDOW: OnceLock<usize> = OnceLock::new();

/// Starts listening (once per process). Each message received is one
/// logoff or shutdown: save, then send `()` on the sender it carries.
pub fn listen() -> Option<UnboundedReceiver<SyncSender<()>>> {
    let (tx, rx) = unbounded_channel();
    SERVER.set(tx).ok()?;
    let (ready_tx, ready_rx) = sync_channel(1);
    std::thread::Builder::new()
        .name("termos-endsession".into())
        .spawn(move || run_window(&ready_tx))
        .ok()?;
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(true) => Some(rx),
        _ => {
            tracing::warn!("no window for logoff and shutdown: saving then is best effort");
            None
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Creates the hidden window and runs its message loop.
#[allow(unsafe_code)]
fn run_window(ready: &SyncSender<bool>) {
    let class = wide("TermOSEndSession");
    // SAFETY: the module handle of this executable; no name means ours.
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        ..WNDCLASSW::default()
    };
    // SAFETY: a class description whose strings outlive the call (and the
    // window, as this function runs the window's whole life).
    if unsafe { RegisterClassW(&wc) } == 0 {
        let _ = ready.send(false);
        return;
    }
    let title = wide("TermOS");
    // SAFETY: a top-level window (no parent), never shown, of the class
    // just registered.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        let _ = ready.send(false);
        return;
    }
    let _ = WINDOW.set(hwnd as usize);
    let _ = ready.send(true);
    // SAFETY: a zeroed MSG is a valid place for GetMessageW to fill.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: the loop runs on the thread that owns the window.
    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        // SAFETY: a message GetMessageW filled.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[allow(unsafe_code)]
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // Logoff or shutdown may go ahead.
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                ending();
            }
            0
        }
        // SAFETY: the arguments Windows passed in.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The session is ending: ask the server to save, and wait for it.
fn ending() {
    let Some(server) = SERVER.get() else { return };
    let (ack, saved) = sync_channel(1);
    if server.send(ack).is_ok() {
        let _ = saved.recv_timeout(SAVE_WAIT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;

    #[allow(unsafe_code)]
    fn send(msg: u32, wparam: WPARAM) -> LRESULT {
        let hwnd = *WINDOW.get().expect("the window exists") as HWND;
        // SAFETY: our own window, from another thread: SendMessageW waits
        // for its thread to handle the message.
        unsafe { SendMessageW(hwnd, msg, wparam, 0) }
    }

    /// Windows asks, then ends the session: the window lets it, hands the
    /// server an acknowledgement and waits until the server sends it.
    #[tokio::test]
    async fn the_server_saves_before_the_session_ends() {
        let mut rx = listen().expect("listening");
        assert!(listen().is_none(), "only once per process");
        assert_eq!(
            tokio::task::spawn_blocking(|| send(WM_QUERYENDSESSION, 0))
                .await
                .expect("join"),
            1
        );
        let started = std::time::Instant::now();
        let ending = tokio::task::spawn_blocking(|| send(WM_ENDSESSION, 1));
        let ack = rx.recv().await.expect("asked to save");
        // The window is still waiting while the server saves.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !ending.is_finished(),
            "the window let Windows go on before the save"
        );
        ack.send(()).expect("the window waits");
        ending.await.expect("join");
        assert!(
            started.elapsed() < SAVE_WAIT,
            "it waited out the limit instead"
        );
        // A session that is not ending after all asks nothing.
        tokio::task::spawn_blocking(|| send(WM_ENDSESSION, 0))
            .await
            .expect("join");
        assert!(rx.try_recv().is_err());
    }
}
