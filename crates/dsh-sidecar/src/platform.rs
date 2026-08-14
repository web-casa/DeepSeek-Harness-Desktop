//! Platform-specific child lifecycle glue.
//!
//! Both implementations must guarantee: when the sidecar goes down (for any
//! reason), the entire Harness process tree goes down with it.
//!   * Unix:    child is spawned in its own process group (pgid == child pid);
//!              we signal the whole group, so agents' shells and grandchildren
//!              are covered too.
//!   * Windows: child is spawned with CREATE_NEW_PROCESS_GROUP (needed for
//!              CTRL_BREAK) and enrolled in a Job Object configured with
//!              JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE — closing the last job
//!              handle (or terminating the job) kills the whole tree, even if
//!              the sidecar itself is killed.

use std::io;
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
            cmd.arg(&spec.script)
                .args(&spec.args)
                .current_dir(&spec.cwd)
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
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    struct JobGuard(HANDLE);

    impl Drop for JobGuard {
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

    /// A spawned child plus the platform handle used to kill its whole tree.
    pub struct PlatformChild {
        pub child: Child,
        kill: Kill,
    }

    impl PlatformChild {
        pub fn spawn(spec: &SpawnSpec) -> io::Result<Self> {
            let mut cmd = Command::new(&spec.node);
            cmd.arg(&spec.script)
                .args(&spec.args)
                .current_dir(&spec.cwd)
                .envs(spec.env.iter().map(|(k, v)| (k, v)))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
            let mut child = cmd.spawn()?;
            let pid = child.id();
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                let error = io::Error::last_os_error();
                let _ = child.kill();
                return Err(io::Error::other(format!(
                    "job object setup failed: CreateJobObjectW: {error}"
                )));
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
                let error = io::Error::last_os_error();
                let _ = child.kill();
                return Err(io::Error::other(format!(
                    "job object setup failed: SetInformationJobObject: {error}"
                )));
            }

            // Safe: job is a valid handle, child handle lives in `child`.
            let assign_ok =
                unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) };
            if assign_ok == 0 {
                let error = io::Error::last_os_error();
                let _ = child.kill();
                return Err(io::Error::other(format!(
                    "job object setup failed: AssignProcessToJobObject: {error}"
                )));
            }
            Ok(PlatformChild {
                child,
                kill: Kill { job, pid },
            })
        }

        /// Polite shutdown: CTRL_BREAK to the child's console process group.
        /// Node treats it as SIGBREAK and tears down its own subprocesses.
        pub fn graceful(&self) -> bool {
            self.kill.pid != 0
                && unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.kill.pid) != 0 }
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
