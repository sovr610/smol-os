// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows: each CLI runs in a job object, so stopping it stops everything
//! it started, as its own process group does on Unix (D-107).
//!
//! The CLI is started suspended, put in the job, and only then let run,
//! so not even its first child can start outside the job.

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

pub(crate) use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

/// A handle kept as an integer, so the job can move between tasks.
fn handle(h: usize) -> HANDLE {
    h as HANDLE
}

/// A job holding a CLI and whatever it starts.
pub(crate) struct Job(usize);

impl Job {
    /// Puts `child`, started with [`CREATE_SUSPENDED`], into a new job,
    /// then lets it run. Without a job (one could not be made) the child
    /// still runs, and stopping it stops only the child: that is logged.
    /// An error means the child could not be let run, and was stopped.
    #[allow(unsafe_code)]
    pub(crate) fn start(child: &mut tokio::process::Child) -> io::Result<Option<Job>> {
        let (Some(process), Some(pid)) = (child.raw_handle(), child.id()) else {
            return Err(io::Error::other("the program exited before it started"));
        };
        // SAFETY: default security and no name; the handle is checked.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        let job = if job.is_null() {
            tracing::warn!(
                "no job object ({}): stopping it will stop only the program itself",
                io::Error::last_os_error()
            );
            None
        } else {
            let job = Job(job as usize);
            // SAFETY: an open job and the child's open process handle.
            if unsafe { AssignProcessToJobObject(handle(job.0), process as HANDLE) } == 0 {
                tracing::warn!(
                    "cannot put the program in a job ({}): stopping it will stop only the program itself",
                    io::Error::last_os_error()
                );
                None
            } else {
                Some(job)
            }
        };
        if let Err(e) = resume(pid) {
            let _ = child.start_kill();
            return Err(e);
        }
        Ok(job)
    }

    /// Stops every process in the job.
    #[allow(unsafe_code)]
    pub(crate) fn terminate(&self) {
        // SAFETY: the job handle is open while `self` lives.
        unsafe { TerminateJobObject(handle(self.0), 1) };
    }
}

impl Drop for Job {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateJobObjectW and is closed once.
        // The job has no kill-on-close limit: closing it stops nothing.
        unsafe { CloseHandle(handle(self.0)) };
    }
}

/// Resumes the threads of process `pid` (a process started suspended has
/// one).
#[allow(unsafe_code)]
fn resume(pid: u32) -> io::Result<()> {
    // SAFETY: a snapshot of all threads; the handle is checked and closed
    // below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: u32::try_from(std::mem::size_of::<THREADENTRY32>()).expect("a small struct"),
        cntUsage: 0,
        th32ThreadID: 0,
        th32OwnerProcessID: 0,
        tpBasePri: 0,
        tpDeltaPri: 0,
        dwFlags: 0,
    };
    let mut resumed = 0;
    // SAFETY: an open snapshot and an entry whose size field is set.
    let mut more = unsafe { Thread32First(snap, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: only suspend/resume access to a thread of our child.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if !thread.is_null() {
                // SAFETY: an open thread handle, closed right after.
                unsafe {
                    if ResumeThread(thread) != u32::MAX {
                        resumed += 1;
                    }
                    CloseHandle(thread);
                }
            }
        }
        // SAFETY: as for Thread32First.
        more = unsafe { Thread32Next(snap, &mut entry) } != 0;
    }
    // SAFETY: the snapshot is open and closed once.
    unsafe { CloseHandle(snap) };
    if resumed == 0 {
        return Err(io::Error::other(
            "cannot let the program run (no thread to resume)",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Runs `outer.cmd`, which starts `inner.cmd` in the background and
    /// exits; `inner.cmd` writes a file after a few seconds. Stopping the
    /// job stops `inner.cmd` too, so the file never appears.
    async fn background_survives(stop: bool) -> bool {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("outer.cmd"),
            "@start \"\" /b cmd /c inner.cmd\r\n@exit 0\r\n",
        )
        .expect("write");
        std::fs::write(
            dir.path().join("inner.cmd"),
            "@ping -n 4 127.0.0.1 >nul\r\n@echo late> marker.txt\r\n",
        )
        .expect("write");
        let mut cmd = tokio::process::Command::new("cmd.exe");
        cmd.args(["/c", "outer.cmd"])
            .current_dir(dir.path())
            .creation_flags(CREATE_SUSPENDED);
        let mut child = cmd.spawn().expect("spawn");
        let job = Job::start(&mut child).expect("runs").expect("a job");
        let _ = child.wait().await;
        if stop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            job.terminate();
        }
        tokio::time::sleep(Duration::from_secs(8)).await;
        dir.path().join("marker.txt").exists()
    }

    #[tokio::test]
    async fn stopping_the_job_stops_what_the_program_started() {
        assert!(
            background_survives(false).await,
            "the background script should finish when nothing stops it"
        );
        assert!(
            !background_survives(true).await,
            "the background script outlived the job"
        );
    }
}
