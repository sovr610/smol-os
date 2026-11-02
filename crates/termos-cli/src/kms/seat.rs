// SPDX-License-Identifier: GPL-3.0-or-later
//! The seat: libseat (seatd, or logind where there is one) gives this
//! unprivileged process the display and input devices, takes them away
//! when the user switches to another virtual terminal, and gives them back.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use libseat::{Seat, SeatEvent};

/// An open device: its descriptor, and libseat's handle for closing it.
struct Opened {
    device: libseat::Device,
}

/// The seat, shared by the display and libinput.
#[derive(Clone)]
pub(super) struct SeatHandle {
    seat: Rc<RefCell<Seat>>,
    events: Rc<RefCell<VecDeque<SeatEvent>>>,
    devices: Rc<RefCell<HashMap<RawFd, Opened>>>,
}

/// libseat's errors are errno values.
macro_rules! errno {
    ($e:expr) => {
        io::Error::from_raw_os_error($e.0)
    };
}

/// Takes ownership of a descriptor libseat opened for us.
#[allow(unsafe_code)]
fn own(device: &libseat::Device) -> OwnedFd {
    // SAFETY: libseat_open_device returns a new descriptor that belongs to
    // the caller: libseat never closes it (libseat_close_device only
    // releases the device on the seat), so wrapping it gives it exactly one
    // owner, which closes it once.
    unsafe { OwnedFd::from_raw_fd(device.as_fd().as_raw_fd()) }
}

impl SeatHandle {
    /// Opens the seat and waits (up to 10 s) until it is ours.
    pub(super) fn open() -> io::Result<Self> {
        let events: Rc<RefCell<VecDeque<SeatEvent>>> = Rc::default();
        let queue = Rc::clone(&events);
        let seat = Seat::open(move |_, ev| queue.borrow_mut().push_back(ev)).map_err(|e| {
            let e = errno!(e);
            io::Error::new(
                e.kind(),
                format!(
                    "cannot open the seat (is seatd running, and is this user in its group?): {e}"
                ),
            )
        })?;
        let h = Self {
            seat: Rc::new(RefCell::new(seat)),
            events,
            devices: Rc::default(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            h.seat.borrow_mut().dispatch(100).map_err(|e| errno!(e))?;
            if h.events
                .borrow_mut()
                .drain(..)
                .any(|e| matches!(e, SeatEvent::Enable))
            {
                return Ok(h);
            }
            if Instant::now() > deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the seat was not given to us within 10 s",
                ));
            }
        }
    }

    pub(super) fn name(&self) -> String {
        self.seat.borrow_mut().name().to_owned()
    }

    /// The descriptor to wait on for seat events.
    pub(super) fn fd(&self) -> io::Result<RawFd> {
        Ok(self
            .seat
            .borrow_mut()
            .get_fd()
            .map_err(|e| errno!(e))?
            .as_raw_fd())
    }

    /// Reads seat events without waiting.
    pub(super) fn dispatch(&self) -> io::Result<Vec<SeatEvent>> {
        self.seat.borrow_mut().dispatch(0).map_err(|e| errno!(e))?;
        Ok(self.events.borrow_mut().drain(..).collect())
    }

    /// Lets another session have the devices (after a Disable event).
    pub(super) fn disable(&self) -> io::Result<()> {
        self.seat.borrow_mut().disable().map_err(|e| errno!(e))
    }

    pub(super) fn switch_session(&self, vt: i32) -> io::Result<()> {
        self.seat
            .borrow_mut()
            .switch_session(vt)
            .map_err(|e| errno!(e))
    }

    /// Opens a device through the seat.
    pub(super) fn open_device(&self, path: &Path) -> io::Result<OwnedFd> {
        let device = self
            .seat
            .borrow_mut()
            .open_device(&path)
            .map_err(|e| errno!(e))?;
        let fd = own(&device);
        self.devices
            .borrow_mut()
            .insert(fd.as_raw_fd(), Opened { device });
        Ok(fd)
    }

    /// Gives a device back to the seat and closes it.
    pub(super) fn close_device(&self, fd: OwnedFd) {
        if let Some(o) = self.devices.borrow_mut().remove(&fd.as_raw_fd())
            && let Err(e) = self.seat.borrow_mut().close_device(o.device)
        {
            tracing::debug!("closing a seat device: {}", errno!(e));
        }
        drop(fd);
    }
}

/// libinput opens and closes input devices through the seat.
pub(super) struct InputInterface(pub(super) SeatHandle);

impl input::LibinputInterface for InputInterface {
    fn open_restricted(&mut self, path: &Path, _flags: i32) -> Result<OwnedFd, i32> {
        self.0.open_device(path).map_err(|e| {
            tracing::debug!("cannot open {}: {e}", path.display());
            -e.raw_os_error().unwrap_or(EIO)
        })
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        self.0.close_device(fd);
    }
}

/// EIO, for errors without an OS code.
const EIO: i32 = 5;
