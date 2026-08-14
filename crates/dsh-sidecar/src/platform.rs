//! Platform-specific child lifecycle glue.
//!
//! Both implementations must guarantee: when the sidecar goes down (for any
//! reason), the entire Harness process tree goes down with it.
//!
//! * Unix: child is spawned in its own process group (pgid == child pid);
//!   we signal the whole group, so agents' shells and grandchildren are
//!   covered too.
//! * Windows: child is spawned with CREATE_NEW_PROCESS_GROUP (needed for
//!   CTRL_C) and enrolled in a Job Object configured with
//!   JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE — closing the last job handle (or
//!   terminating the job) kills the whole tree, even if the sidecar itself
//!   is killed.

use std::io;
#[cfg(unix)]
use std::process::{Child, Command, Stdio};

/// Everything needed to launch the bundled Node runtime with the Harness entry.
#[derive(Clone)]
pub struct SpawnSpec {
    pub node: String,
    pub script: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::process::CommandExt;

    struct Kill {
        pgid: i32,
    }

    /// A spawned child plus the platform handle used to kill its whole tree.
    pub struct PlatformChild {
        pub child: Child,
        kill: Kill,
    }

    impl PlatformChild {
        pub fn spawn(spec: &SpawnSpec) -> io::Result<Self> {
            let mut cmd = Command::new(&spec.node);
            // Injection-safe environment: Command inherits the FULL parent
            // env by default and `.envs()` only overlays — so a key omitted
            // from the overlay would still leak through. env_clear() first,
            // then re-add the sanitized snapshot (parent env minus node/npm
            // control keys), then the start command's own overrides (DSH_HOME
            // etc.) which are exempt from the filter by design.
            let inherited = crate::sanitize_inherited_env(std::env::vars().collect());
            cmd.env_clear()
                .arg(&spec.script)
                .args(&spec.args)
                .current_dir(&spec.cwd)
                .envs(inherited)
                .envs(spec.env.iter().map(|(k, v)| (k, v)))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // New process group: pgid == child pid, signalable as a tree.
            cmd.process_group(0);
            let child = cmd.spawn()?;
            let pid = child.id();
            Ok(PlatformChild {
                child,
                kill: Kill { pgid: pid as i32 },
            })
        }

        /// Polite shutdown: give the tree a chance to clean up.
        pub fn graceful(&self) -> bool {
            // Safe: pgid refers to a live process group we created.
            unsafe {
                libc::kill(-self.kill.pgid, libc::SIGTERM);
            }
            true
        }

        /// Immediate teardown of the whole tree.
        pub fn force(&self) {
            unsafe {
                libc::kill(-self.kill.pgid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::quote_arg;
    use std::fs::File;
    use std::os::windows::io::FromRawHandle;
    use std::os::windows::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows_sys::Win32::Foundation::{
        CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Console::{
        AllocConsole, FreeConsole, GenerateConsoleCtrlEvent, GetConsoleWindow,
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT,
    };
    use windows_sys::Win32::System::Environment::{
        FreeEnvironmentStringsW, GetEnvironmentStringsW,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, GetExitCodeProcess, TerminateProcess, PROCESS_INFORMATION,
        STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES, STARTUPINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

    /// True once `SetConsoleCtrlHandler` succeeded. `graceful()` refuses
    /// CTRL_C when false: without our handler the broadcast would hit the
    /// CRT default handler and terminate the sidecar mid-teardown (dev
    /// builds), so the force path is the only safe one.
    static CTRL_HANDLER_OK: AtomicBool = AtomicBool::new(false);

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
    const STILL_ACTIVE: u32 = 259;

    /// Give the sidecar a PRIVATE hidden console so a child spawned WITHOUT
    /// CREATE_NO_WINDOW inherits it — which is what makes
    /// GenerateConsoleCtrlEvent(CTRL_C_EVENT) reachable at all. CTRL_C →
    /// node SIGINT → dsh's graceful interrupt path (exit 130).
    ///
    /// Two hardening properties:
    /// - `GenerateConsoleCtrlEvent` with CTRL_C ignores its process-group
    ///   argument and broadcasts to EVERY process sharing the console. A
    ///   private console therefore contains the broadcast: in dev mode it can
    ///   never leak into the `cargo run` terminal (we FreeConsole first, which
    ///   only detaches us — the user's console window is untouched).
    /// - The sidecar registers a ctrl handler that swallows the broadcast, so
    ///   it never terminates itself (the CRT default handler would, in
    ///   console-subsystem/dev builds).
    ///
    /// Direct-run caveat: FreeConsole detaches this process from whatever
    /// console it started on, so a manually launched sidecar's console
    /// output disappears (NDJSON on stdout is unaffected in production —
    /// the Tauri shell always spawns us with piped stdio).
    pub fn ensure_hidden_console() {
        unsafe {
            // Release builds have no console (FreeConsole fails harmlessly);
            // dev builds inherit the dev terminal's console — detach from it.
            FreeConsole();
            if AllocConsole() != 0 {
                // Swallow CTRL_C/CTRL_BREAK: only the child should react.
                // Failure is rare; graceful() then degrades to the force
                // path instead of broadcasting an unguarded CTRL_C.
                let registered = SetConsoleCtrlHandler(Some(ignore_console_ctrl), 1);
                CTRL_HANDLER_OK.store(registered != 0, Ordering::Relaxed);
                ShowWindow(GetConsoleWindow(), SW_HIDE);
            }
        }
    }

    /// Swallow only the interrupt events we deliver ourselves (CTRL_C; BREAK
    /// for symmetry). CLOSE/LOGOFF/SHUTDOWN fall through (return FALSE) to the
    /// default handler, which terminates the process — termination closes the
    /// Job handle, and KILL_ON_JOB_CLOSE then force-kills the child tree.
    /// Returning TRUE for those would hang the supervisor during logoff.
    unsafe extern "system" fn ignore_console_ctrl(ctrl_type: u32) -> i32 {
        if ctrl_type == CTRL_C_EVENT || ctrl_type == CTRL_BREAK_EVENT {
            1
        } else {
            0
        }
    }

    struct JobGuard(HANDLE);

    impl Drop for JobGuard {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    pub struct ProcessGuard(HANDLE);

    impl Drop for ProcessGuard {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    struct Kill {
        job: JobGuard,
        pid: u32,
    }

    /// std::process::Child-compatible wrapper around a raw process handle
    /// (hand-rolled spawn is required for console inheritance). stdin is not
    /// surfaced: the child's stdin pipe is closed at spawn (immediate EOF).
    pub struct WindowsChild {
        pub process: ProcessGuard,
        pub stdout: Option<File>,
        pub stderr: Option<File>,
        pid: u32,
    }

    impl WindowsChild {
        pub fn id(&self) -> u32 {
            self.pid
        }

        pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            let mut code: u32 = 0;
            let ok = unsafe { GetExitCodeProcess(self.process.0, &mut code) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if code == STILL_ACTIVE {
                return Ok(None);
            }
            Ok(Some(ExitStatus::from_raw(code)))
        }

        pub fn kill(&mut self) -> io::Result<()> {
            let ok = unsafe { TerminateProcess(self.process.0, 1) };
            if ok == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    /// A spawned child plus the platform handle used to kill its whole tree.
    pub struct PlatformChild {
        pub child: WindowsChild,
        kill: Kill,
    }

    /// Quote one argument for the Windows command line (CommandLineToArgvW
    /// semantics) — lives in main.rs so it is unit-testable on every platform.
    fn win_err(context: &str) -> io::Error {
        io::Error::other(format!("{context}: {}", io::Error::last_os_error()))
    }

    /// Create an anonymous pipe pair; the read end is made non-inheritable
    /// (the parent keeps it), the write end stays inheritable for the child.
    fn create_pipe() -> io::Result<(HANDLE, HANDLE)> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let mut read: HANDLE = std::ptr::null_mut();
        let mut write: HANDLE = std::ptr::null_mut();
        let ok = unsafe { CreatePipe(&mut read, &mut write, &sa, 0) };
        if ok == 0 {
            return Err(win_err("CreatePipe"));
        }
        unsafe {
            if SetHandleInformation(read, HANDLE_FLAG_INHERIT, 0) == 0 {
                CloseHandle(read);
                CloseHandle(write);
                return Err(win_err("SetHandleInformation"));
            }
        }
        Ok((read, write))
    }

    /// Build the child environment block from the inherited environment plus
    /// overrides (case-insensitive key match, like Windows itself). Operates
    /// on raw UTF-16 units: entries we do NOT touch are forwarded verbatim
    /// (no lossy UTF-8 round-trip), so an unrelated entry containing
    /// unpaired surrogates survives intact.
    fn build_env_block(overrides: &[(String, String)]) -> io::Result<Vec<u16>> {
        let raw = unsafe { GetEnvironmentStringsW() };
        if raw.is_null() {
            return Err(win_err("GetEnvironmentStringsW"));
        }
        let mut lines: Vec<Vec<u16>> = Vec::new();
        let mut current: Vec<u16> = Vec::new();
        let mut i = 0usize;
        loop {
            let w = unsafe { *raw.add(i) };
            i += 1;
            if w == 0 {
                if current.is_empty() {
                    break; // double null terminator
                }
                lines.push(std::mem::take(&mut current));
            } else {
                current.push(w);
            }
        }
        unsafe { FreeEnvironmentStringsW(raw) };

        // Injection-safe environment: strip node/npm control keys at the
        // UTF-16 level (no lossy round-trip), then apply the overrides —
        // overrides come last, so they win and are exempt from the filter.
        let mut lines = crate::sanitize_env_lines(lines);

        for (key, value) in overrides {
            let mut entry: Vec<u16> = key.encode_utf16().collect();
            entry.push(b'=' as u16);
            entry.extend(value.encode_utf16());
            // ASCII-folded `KEY=` prefix, compared at the UTF-16 level.
            let prefix: Vec<u16> = key
                .encode_utf16()
                .map(crate::fold_ascii_u16)
                .chain(std::iter::once(b'=' as u16))
                .collect();
            let matches = |l: &[u16]| {
                l.len() >= prefix.len()
                    && l[..prefix.len()]
                        .iter()
                        .map(|&w| crate::fold_ascii_u16(w))
                        .eq(prefix.iter().copied())
            };
            match lines.iter_mut().find(|l| matches(l)) {
                Some(line) => *line = entry,
                None => lines.push(entry),
            }
        }

        let mut block: Vec<u16> = Vec::new();
        for line in lines {
            block.extend(line);
            block.push(0);
        }
        block.push(0); // double null terminator
        Ok(block)
    }

    impl PlatformChild {
        pub fn spawn(spec: &SpawnSpec) -> io::Result<Self> {
            // Command line: node <script> <args…>, quoted per Windows rules.
            let mut cmdline = quote_arg(&spec.node);
            cmdline.push(' ');
            cmdline.push_str(&quote_arg(&spec.script));
            for arg in &spec.args {
                cmdline.push(' ');
                cmdline.push_str(&quote_arg(arg));
            }
            let mut cmdline_w: Vec<u16> = cmdline.encode_utf16().collect();
            cmdline_w.push(0);

            let cwd_w: Vec<u16> = spec.cwd.encode_utf16().chain(std::iter::once(0)).collect();
            let env_block = build_env_block(&spec.env)?;
            let mut env_block = env_block;

            // stdin: pipe whose write end we close immediately (child sees EOF),
            // stdout/stderr: pipes the parent reads.
            let (stdin_read, stdin_write) = create_pipe()?;
            let (stdout_read, stdout_write) = create_pipe()?;
            let (stderr_read, stderr_write) = create_pipe()?;

            let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
            si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            si.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
            si.wShowWindow = SW_HIDE as u16;
            si.hStdInput = stdin_read;
            si.hStdOutput = stdout_write;
            si.hStdError = stderr_write;

            let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            // No CREATE_NO_WINDOW / DETACHED_PROCESS: the child INHERITS the
            // sidecar's (hidden) console, which is what allows CTRL_C delivery.
            let flags = CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT;
            let ok = unsafe {
                CreateProcessW(
                    std::ptr::null(),
                    cmdline_w.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    1, // inherit handles (child pipe ends + console)
                    flags,
                    env_block.as_mut_ptr() as *const core::ffi::c_void,
                    cwd_w.as_ptr(),
                    &si,
                    &mut pi,
                )
            };
            // Parent-side cleanup regardless of outcome.
            unsafe {
                CloseHandle(stdin_read);
                CloseHandle(stdin_write);
                CloseHandle(stdout_write);
                CloseHandle(stderr_write);
            }
            if ok == 0 {
                unsafe {
                    CloseHandle(stdout_read);
                    CloseHandle(stderr_read);
                }
                return Err(win_err("CreateProcessW"));
            }
            unsafe {
                CloseHandle(pi.hThread);
            }
            let pid = pi.dwProcessId;

            // Job Object containment (fail the spawn if unavailable).
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                unsafe {
                    TerminateProcess(pi.hProcess, 1);
                    CloseHandle(pi.hProcess);
                    CloseHandle(stdout_read);
                    CloseHandle(stderr_read);
                }
                return Err(win_err("CreateJobObjectW"));
            }
            let job = JobGuard(job);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set_ok = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if set_ok == 0 {
                unsafe {
                    TerminateProcess(pi.hProcess, 1);
                    CloseHandle(pi.hProcess);
                    CloseHandle(stdout_read);
                    CloseHandle(stderr_read);
                }
                return Err(win_err("SetInformationJobObject"));
            }
            let assign_ok = unsafe { AssignProcessToJobObject(job.0, pi.hProcess) };
            if assign_ok == 0 {
                unsafe {
                    TerminateProcess(pi.hProcess, 1);
                    CloseHandle(pi.hProcess);
                    CloseHandle(stdout_read);
                    CloseHandle(stderr_read);
                }
                return Err(win_err("AssignProcessToJobObject"));
            }

            let stdout = unsafe { File::from_raw_handle(stdout_read) };
            let stderr = unsafe { File::from_raw_handle(stderr_read) };
            Ok(PlatformChild {
                child: WindowsChild {
                    process: ProcessGuard(pi.hProcess),
                    stdout: Some(stdout),
                    stderr: Some(stderr),
                    pid,
                },
                kill: Kill { job, pid },
            })
        }

        /// Polite shutdown: CTRL_C to the child's console. Windows ignores the
        /// process-group argument for CTRL_C and broadcasts to every process
        /// on the console — the sidecar's own handler swallows it, leaving
        /// only the child to react. Node maps CTRL_C to SIGINT, which dsh
        /// handles with a real graceful teardown (interrupt → dispose),
        /// unlike CTRL_BREAK/SIGBREAK.
        pub fn graceful(&self) -> bool {
            CTRL_HANDLER_OK.load(Ordering::Relaxed)
                && self.kill.pid != 0
                && unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, self.kill.pid) != 0 }
        }

        /// Immediate teardown of the whole tree via the Job Object.
        pub fn force(&self) {
            // Safe: job handle was validated at spawn time.
            unsafe {
                TerminateJobObject(self.kill.job.0, 1);
            }
        }
    }
}

#[cfg(unix)]
pub use imp::PlatformChild;

#[cfg(windows)]
pub use imp::PlatformChild;

#[cfg(windows)]
pub use imp::ensure_hidden_console;

#[cfg(not(windows))]
pub fn ensure_hidden_console() {}
