// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows: agent commands in an AppContainer (D-101, D-108).
//!
//! An AppContainer process reads what Windows lets every app read (the
//! system, Program Files) and nothing of the user's profile; it writes
//! nowhere it has not been granted, and has no network without the
//! `internetClient` capability. TermOS grants its container the
//! workspace and the `--write` folders (modify) and, read-only, the
//! toolchains a command needs: the folders on `PATH` inside the user's
//! profile (a `bin` or `shims` folder's parent, so `~/.cargo` and mise's
//! data come along), `~/.rustup`, the program itself when named by path,
//! and `--read` folders.
//!
//! Granting a folder makes Windows rewrite the permissions of every file
//! in it, which takes a while for a big tree, so grants are kept (they
//! are for this container only) and recorded; `termos sandbox reset`
//! takes them all back and deletes the container.
//!
//! The command starts suspended in a job that ends with the helper, gets
//! the helper's console, and the helper waits for it and passes on its
//! exit code.

use std::path::{Path, PathBuf};

/// The container for one TermOS installation (its state folder), so a
/// test's container is not the user's.
pub(crate) fn container_name(state_dir: &Path) -> String {
    let key = state_dir.to_string_lossy().to_lowercase();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("TermOS.Agents.{h:016x}")
}

/// One argument quoted the way the C runtime reads a command line.
pub(crate) fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            c => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// The command line for `argv`.
pub(crate) fn command_line(argv: &[String]) -> String {
    argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
}

/// The toolchain folders to grant read access to: each `PATH` folder
/// inside `profile` (a `bin` or `shims` folder's parent instead, the
/// tool's home), and `~/.rustup`. Folders that do not exist are left out.
pub(crate) fn toolchain_folders(path: &[PathBuf], profile: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if p.is_dir() && p != profile && !out.iter().any(|o| p.starts_with(o)) {
            out.retain(|o| !o.starts_with(&p));
            out.push(p);
        }
    };
    for dir in path {
        if !dir.starts_with(profile) {
            continue;
        }
        let home = match dir.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.eq_ignore_ascii_case("bin") || n.eq_ignore_ascii_case("shims") => {
                dir.parent().map(Path::to_path_buf)
            }
            _ => Some(dir.clone()),
        };
        if let Some(h) = home {
            add(h);
        }
    }
    add(profile.join(".rustup"));
    out
}

/// A grant TermOS made, as recorded for `termos sandbox reset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Grant {
    pub write: bool,
    pub path: PathBuf,
}

/// The record's lines: `w` or `r`, a tab, the path.
pub(crate) fn parse_grants(text: &str) -> Vec<Grant> {
    text.lines()
        .filter_map(|l| {
            let (kind, path) = l.split_once('\t')?;
            let write = match kind {
                "w" => true,
                "r" => false,
                _ => return None,
            };
            Some(Grant {
                write,
                path: PathBuf::from(path),
            })
        })
        .collect()
}

pub(crate) fn grant_line(g: &Grant) -> String {
    format!(
        "{}\t{}\n",
        if g.write { "w" } else { "r" },
        g.path.display()
    )
}

/// Whether `want` is covered by what is already granted (the same or a
/// containing folder, with write if `want` needs it).
pub(crate) fn covered(granted: &[Grant], want: &Grant) -> bool {
    granted
        .iter()
        .any(|g| want.path.starts_with(&g.path) && (g.write || !want.write))
}

#[cfg(windows)]
pub(crate) use sys::{probe, reset, run};

#[cfg(windows)]
mod sys {
    use std::ffi::OsString;
    use std::io::{self, Write as _};
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::{Path, PathBuf};

    use windows_sys::Win32::Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, HLOCAL, LocalFree,
        WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW,
        NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS, SE_FILE_OBJECT, SetEntriesInAclW,
        SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::Isolation::{
        CreateAppContainerProfile, DeleteAppContainerProfile,
        DeriveAppContainerSidFromAppContainerName, GetAppContainerFolderPath,
    };
    use windows_sys::Win32::Security::{
        ACL, CreateWellKnownSid, DACL_SECURITY_INFORMATION, FreeSid, NO_INHERITANCE,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES, SECURITY_MAX_SID_SIZE,
        SID_AND_ATTRIBUTES, SUB_CONTAINERS_AND_OBJECTS_INHERIT, WinCapabilityInternetClientSid,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_DELETE_CHILD, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
        GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
        LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
        PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
        UpdateProcThreadAttribute, WaitForSingleObject,
    };

    use super::{Grant, command_line, container_name, covered, grant_line, parse_grants};

    /// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`.
    const ALREADY_EXISTS: i32 = 0x8007_00B7_u32 as i32;
    /// A capability SID's attribute: in force (`SE_GROUP_ENABLED`).
    const SE_GROUP_ENABLED: u32 = 0x4;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// An AppContainer SID, freed when dropped.
    struct Sid(PSID);

    impl Drop for Sid {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: the SID came from CreateAppContainerProfile or
            // DeriveAppContainerSidFromAppContainerName, freed with FreeSid
            // once.
            unsafe { FreeSid(self.0) };
        }
    }

    /// The container's SID, making the container if needed.
    #[allow(unsafe_code)]
    fn container(name: &str) -> io::Result<Sid> {
        let wname = wide(name.as_ref());
        let display = wide("TermOS agents".as_ref());
        let about = wide("Commands that TermOS's AI agents run".as_ref());
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: valid strings, no capabilities, a place for the SID.
        let hr = unsafe {
            CreateAppContainerProfile(
                wname.as_ptr(),
                display.as_ptr(),
                about.as_ptr(),
                std::ptr::null(),
                0,
                &mut sid,
            )
        };
        if hr == ALREADY_EXISTS {
            // SAFETY: a valid name and a place for the SID.
            let hr = unsafe { DeriveAppContainerSidFromAppContainerName(wname.as_ptr(), &mut sid) };
            if hr < 0 {
                return Err(io::Error::other(format!(
                    "cannot find the container ({hr:#x})"
                )));
            }
        } else if hr < 0 {
            return Err(io::Error::other(format!(
                "cannot make the container ({hr:#x})"
            )));
        }
        Ok(Sid(sid))
    }

    /// Adds (or with `revoke`, removes) the container's access to `path`.
    #[allow(unsafe_code)]
    fn set_access(path: &Path, sid: &Sid, write: bool, revoke: bool) -> io::Result<()> {
        let wpath = wide(path.as_os_str());
        let mut old: *mut ACL = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a valid path; the DACL points into `sd`, freed below.
        let err = unsafe {
            GetNamedSecurityInfoW(
                wpath.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut old,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if err != 0 {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        let rights = if write {
            FILE_GENERIC_READ
                | FILE_GENERIC_WRITE
                | FILE_GENERIC_EXECUTE
                | DELETE
                | FILE_DELETE_CHILD
        } else {
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE
        };
        let ea = EXPLICIT_ACCESS_W {
            grfAccessPermissions: if revoke { 0 } else { rights },
            grfAccessMode: if revoke { REVOKE_ACCESS } else { GRANT_ACCESS },
            grfInheritance: if path.is_dir() {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            } else {
                NO_INHERITANCE
            },
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                ptstrName: sid.0.cast(),
            },
        };
        let mut new: *mut ACL = std::ptr::null_mut();
        // SAFETY: one entry, the old DACL (still owned by `sd`), and a place
        // for the new one, freed with LocalFree.
        let err = unsafe { SetEntriesInAclW(1, &ea, old, &mut new) };
        // SAFETY: `sd` came from GetNamedSecurityInfoW; `old` is not used
        // after this.
        unsafe { LocalFree(sd as HLOCAL) };
        if err != 0 {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        // SAFETY: a valid path and the new DACL; Windows passes inheritable
        // entries on to everything inside.
        let err = unsafe {
            SetNamedSecurityInfoW(
                wpath.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                new,
                std::ptr::null(),
            )
        };
        // SAFETY: `new` came from SetEntriesInAclW.
        unsafe { LocalFree(new as HLOCAL) };
        if err != 0 {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        Ok(())
    }

    fn grants_file(state_dir: &Path) -> PathBuf {
        state_dir.join("sandbox").join("grants")
    }

    /// Grants what `wanted` asks for that is not granted yet, and records it.
    fn ensure_grants(state_dir: &Path, sid: &Sid, wanted: &[Grant]) -> io::Result<()> {
        let file = grants_file(state_dir);
        let mut granted = parse_grants(&std::fs::read_to_string(&file).unwrap_or_default());
        for g in wanted {
            if covered(&granted, g) || !g.path.exists() {
                continue;
            }
            match set_access(&g.path, sid, g.write, false) {
                Ok(()) => {
                    if let Some(dir) = file.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&file)?;
                    f.write_all(grant_line(g).as_bytes())?;
                    granted.push(g.clone());
                }
                // The workspace must be granted; a toolchain folder that
                // cannot be (another user's, a protected one) is left out.
                Err(e) if g.write => {
                    return Err(io::Error::other(format!(
                        "cannot give the sandbox {}: {e}",
                        g.path.display()
                    )));
                }
                Err(e) => eprintln!(
                    "termos sandbox-exec: not readable inside: {}: {e}",
                    g.path.display()
                ),
            }
        }
        Ok(())
    }

    /// `sandbox-exec --probe`: whether the container can be made.
    pub(crate) fn probe(state_dir: &Path) -> Result<(), String> {
        container(&container_name(state_dir))
            .map(drop)
            .map_err(|e| e.to_string())
    }

    /// Takes back every recorded grant and deletes the container. Returns
    /// how many grants were taken back.
    #[allow(unsafe_code)]
    pub(crate) fn reset(state_dir: &Path) -> io::Result<usize> {
        let name = container_name(state_dir);
        let file = grants_file(state_dir);
        let grants = parse_grants(&std::fs::read_to_string(&file).unwrap_or_default());
        let mut done = 0;
        if !grants.is_empty() {
            let sid = container(&name)?;
            for g in &grants {
                // A folder that is gone has nothing left to take back.
                if !g.path.exists() {
                    done += 1;
                    continue;
                }
                match set_access(&g.path, &sid, g.write, true) {
                    Ok(()) => done += 1,
                    Err(e) => eprintln!("cannot take back {}: {e}", g.path.display()),
                }
            }
        }
        let _ = std::fs::remove_file(&file);
        let wname = wide(name.as_ref());
        // SAFETY: a valid name; a container that is not there is fine.
        unsafe { DeleteAppContainerProfile(wname.as_ptr()) };
        Ok(done)
    }

    /// The container's own temporary folder.
    #[allow(unsafe_code)]
    fn temp_dir(sid: &Sid) -> Option<PathBuf> {
        let mut text: windows_sys::core::PWSTR = std::ptr::null_mut();
        // SAFETY: a valid SID; the string is freed with LocalFree.
        if unsafe { ConvertSidToStringSidW(sid.0, &mut text) } == 0 {
            return None;
        }
        let mut folder: windows_sys::core::PWSTR = std::ptr::null_mut();
        // SAFETY: the SID string just made and a place for the path.
        let hr = unsafe { GetAppContainerFolderPath(text, &mut folder) };
        // SAFETY: from ConvertSidToStringSidW.
        unsafe { LocalFree(text as HLOCAL) };
        if hr < 0 || folder.is_null() {
            return None;
        }
        // SAFETY: a NUL-terminated string from GetAppContainerFolderPath.
        let path = unsafe {
            let len = (0..).take_while(|&i| *folder.add(i) != 0).count();
            <OsString as std::os::windows::ffi::OsStringExt>::from_wide(std::slice::from_raw_parts(
                folder, len,
            ))
        };
        // SAFETY: the path's buffer, which the caller frees with
        // CoTaskMemFree.
        unsafe { windows_sys::Win32::System::Com::CoTaskMemFree(folder.cast()) };
        let temp = PathBuf::from(path).join("Temp");
        std::fs::create_dir_all(&temp).ok()?;
        Some(temp)
    }

    /// Ctrl+C reaches the command too; the helper waits for it to end.
    extern "system" fn ignore_ctrl(_kind: u32) -> windows_sys::core::BOOL {
        1
    }

    /// An inheritable copy of one of our standard handles.
    #[allow(unsafe_code)]
    fn inheritable(which: u32) -> HANDLE {
        // SAFETY: GetStdHandle has no preconditions.
        let h = unsafe { GetStdHandle(which) };
        let mut dup: HANDLE = std::ptr::null_mut();
        // SAFETY: our own process and handle; the copy is closed by the
        // caller after CreateProcessW.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                h,
                GetCurrentProcess(),
                &mut dup,
                0,
                1,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 { std::ptr::null_mut() } else { dup }
    }

    /// Runs `argv` in the container and returns its exit code.
    #[allow(unsafe_code)]
    pub(crate) fn run(
        state_dir: &Path,
        argv: &[String],
        wanted: &[Grant],
        net: bool,
        env: &[(OsString, OsString)],
    ) -> io::Result<u32> {
        let sid = container(&container_name(state_dir))?;
        ensure_grants(state_dir, &sid, wanted)?;

        // The network, if asked for.
        let mut cap_sid = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut size = SECURITY_MAX_SID_SIZE;
        // SAFETY: a buffer of SECURITY_MAX_SID_SIZE bytes.
        if unsafe {
            CreateWellKnownSid(
                WinCapabilityInternetClientSid,
                std::ptr::null_mut(),
                cap_sid.as_mut_ptr().cast(),
                &mut size,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut caps = [SID_AND_ATTRIBUTES {
            Sid: cap_sid.as_mut_ptr().cast(),
            Attributes: SE_GROUP_ENABLED,
        }];
        let sc = SECURITY_CAPABILITIES {
            AppContainerSid: sid.0,
            Capabilities: if net {
                caps.as_mut_ptr()
            } else {
                std::ptr::null_mut()
            },
            CapabilityCount: u32::from(net),
            Reserved: 0,
        };

        // One attribute: the security capabilities.
        let mut list_size = 0usize;
        // SAFETY: asking for the size only (the call fails by design).
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut list_size) };
        let mut list = vec![0usize; list_size.div_ceil(std::mem::size_of::<usize>()) + 1];
        let attrs: LPPROC_THREAD_ATTRIBUTE_LIST = list.as_mut_ptr().cast();
        // SAFETY: a buffer of at least `list_size` bytes, suitably aligned.
        if unsafe { InitializeProcThreadAttributeList(attrs, 1, 0, &mut list_size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `sc` and what it points to outlive CreateProcessW below.
        if unsafe {
            UpdateProcThreadAttribute(
                attrs,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                (&raw const sc).cast(),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            let e = io::Error::last_os_error();
            // SAFETY: initialised above.
            unsafe { DeleteProcThreadAttributeList(attrs) };
            return Err(e);
        }

        let handles = [
            inheritable(STD_INPUT_HANDLE),
            inheritable(STD_OUTPUT_HANDLE),
            inheritable(STD_ERROR_HANDLE),
        ];
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>()).expect("small");
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = handles[0];
        si.StartupInfo.hStdOutput = handles[1];
        si.StartupInfo.hStdError = handles[2];
        si.lpAttributeList = attrs;

        let mut cmdline: Vec<u16> = wide(command_line(argv).as_ref());
        let mut block: Vec<u16> = Vec::new();
        for (k, v) in env {
            block.extend(k.encode_wide());
            block.push(u16::from(b'='));
            block.extend(v.encode_wide());
            block.push(0);
        }
        block.push(0);
        let cwd = std::env::current_dir()?;
        let wcwd = wide(cwd.as_os_str());
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: a mutable command line, the attribute list and handles
        // prepared above, a valid environment block and folder.
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmdline.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
                block.as_ptr().cast(),
                wcwd.as_ptr(),
                &si.StartupInfo,
                &mut pi,
            )
        };
        let spawn_error = (ok == 0).then(io::Error::last_os_error);
        // SAFETY: initialised above and no longer needed.
        unsafe { DeleteProcThreadAttributeList(attrs) };
        for h in handles {
            if !h.is_null() {
                // SAFETY: copies made for the child, which has its own now.
                unsafe { CloseHandle(h) };
            }
        }
        if let Some(e) = spawn_error {
            return Err(e);
        }

        // A job that ends the command (and what it starts) with us.
        // SAFETY: default security, no name.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if !job.is_null() {
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: an open job, a limits structure of the stated size,
            // and the suspended process.
            unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                        .expect("small"),
                );
                AssignProcessToJobObject(job, pi.hProcess);
            }
        }
        // SAFETY: a handler that returns, registered for this process only.
        unsafe { SetConsoleCtrlHandler(Some(ignore_ctrl), 1) };
        let mut code = 1u32;
        // SAFETY: the handles CreateProcessW returned, closed once each.
        unsafe {
            ResumeThread(pi.hThread);
            CloseHandle(pi.hThread);
            if WaitForSingleObject(pi.hProcess, INFINITE) == WAIT_OBJECT_0 {
                GetExitCodeProcess(pi.hProcess, &mut code);
            }
            CloseHandle(pi.hProcess);
            if !job.is_null() {
                CloseHandle(job);
            }
        }
        Ok(code)
    }

    /// The container's temporary folder, for the command's TEMP and TMP.
    pub(crate) fn temp(state_dir: &Path) -> Option<PathBuf> {
        container(&container_name(state_dir))
            .ok()
            .and_then(|sid| temp_dir(&sid))
    }
}

#[cfg(windows)]
pub(crate) use sys::temp;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_quoting() {
        let a = container_name(Path::new(r"C:\Users\me\AppData\Local\termos"));
        assert!(a.starts_with("TermOS.Agents.") && a.len() == 30, "{a}");
        assert_eq!(
            a,
            container_name(Path::new(r"c:\users\ME\AppData\Local\termos"))
        );
        assert_ne!(a, container_name(Path::new(r"C:\tmp\test")));

        let argv: Vec<String> = [
            r"C:\Windows\system32\cmd.exe",
            "/D",
            "%TERMOS_COMMAND%",
            "a b",
            r#"say "hi""#,
            r"C:\dir with space\",
            "",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(
            command_line(&argv),
            r#"C:\Windows\system32\cmd.exe /D %TERMOS_COMMAND% "a b" "say \"hi\"" "C:\dir with space\\" """#
        );
    }

    #[test]
    fn toolchains_and_the_grant_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let profile = dir.path().join("me");
        for d in [
            ".cargo/bin",
            ".rustup",
            "AppData/Local/mise/shims",
            "tools",
            "other",
        ] {
            std::fs::create_dir_all(profile.join(d)).expect("mkdir");
        }
        let path = vec![
            profile.join(".cargo/bin"),
            profile.join("AppData/Local/mise/shims"),
            profile.join("tools"),
            profile.clone(),
            dir.path().join("elsewhere"),
            PathBuf::from(r"C:\Windows\System32"),
        ];
        assert_eq!(
            toolchain_folders(&path, &profile),
            vec![
                profile.join(".cargo"),
                profile.join("AppData/Local/mise"),
                profile.join("tools"),
                profile.join(".rustup"),
            ]
        );

        let granted = parse_grants("w\tC:\\ws\nr\tC:\\Users\\me\\.cargo\nx\tbad\nnot a line\n");
        assert_eq!(granted.len(), 2);
        let want = |write, p: &str| Grant {
            write,
            path: PathBuf::from(p),
        };
        assert_eq!(grant_line(&granted[0]), "w\tC:\\ws\n");
        if cfg!(windows) {
            assert!(covered(&granted, &want(true, r"C:\ws\sub")));
            assert!(covered(&granted, &want(false, r"C:\Users\me\.cargo\bin")));
            assert!(!covered(&granted, &want(true, r"C:\Users\me\.cargo")));
        }
        assert!(!covered(&granted, &want(false, "C:\\other")));
    }
}
