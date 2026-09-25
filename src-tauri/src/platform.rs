//! OS-specific helpers for the ssh child processes.

use tokio::process::{Child, Command};

/// Keep ssh.exe from opening a console window next to the GUI.
#[cfg(windows)]
pub fn hide_console(cmd: &mut Command) {
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_console(_cmd: &mut Command) {}

/// An ssh process together with everything it starts: ProxyJump and
/// ProxyCommand run as child processes of ssh, and killing only ssh leaves
/// them running (and holding their connections) on Windows.
pub struct ProcTree {
    #[cfg(windows)]
    job: usize,
    #[cfg(unix)]
    pgid: Option<i32>,
}

/// Windows: each ssh.exe goes into its own job object flagged
/// KILL_ON_JOB_CLOSE, which its children join automatically. Terminating the
/// job ends the whole tree, and if this process dies (even by crashing) the
/// handle closes and Windows kills the tree too.
#[cfg(windows)]
pub fn adopt(child: &Child) -> ProcTree {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    let Some(raw) = child.raw_handle() else {
        return ProcTree { job: 0 };
    };
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return ProcTree { job: 0 };
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        // ssh has only just started; it reads its config before it launches
        // a proxy, so the proxy is created inside the job.
        if AssignProcessToJobObject(job, raw as HANDLE) == 0 {
            CloseHandle(job);
            return ProcTree { job: 0 };
        }
        ProcTree { job: job as usize }
    }
}

/// Unix: ssh leads its own process group (see [`prepare`]), which its proxy
/// helpers inherit.
/// It is also recorded so a later launch can stop it if we die first (see
/// `reaper`).
#[cfg(unix)]
pub fn adopt(child: &Child) -> ProcTree {
    let pgid = child.id().and_then(|pid| i32::try_from(pid).ok());
    if let Some(pid) = child.id() {
        crate::reaper::track(pid);
    }
    ProcTree { pgid }
}

impl ProcTree {
    /// Ends ssh and every process it started.
    pub fn kill(&self) {
        #[cfg(windows)]
        if self.job != 0 {
            unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(
                    self.job as windows_sys::Win32::Foundation::HANDLE,
                    1,
                );
            }
        }
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            unsafe {
                libc::kill(-pgid, libc::SIGTERM);
            }
        }
    }
}

impl Drop for ProcTree {
    /// Nothing outlives the tunnel attempt that started it.
    fn drop(&mut self) {
        self.kill();
        #[cfg(windows)]
        if self.job != 0 {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(
                    self.job as windows_sys::Win32::Foundation::HANDLE,
                );
            }
        }
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            crate::reaper::untrack(pgid as u32);
        }
    }
}

/// Unix: put ssh in its own process group so [`ProcTree`] can signal it
/// with its helpers. On Linux also ask the kernel to SIGTERM ssh if we die;
/// other Unixes rely on the explicit cleanup at exit.
#[cfg(unix)]
pub fn prepare(cmd: &mut Command) {
    cmd.process_group(0);
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub fn prepare(_cmd: &mut Command) {}

/// Synchronously terminate an ssh process group by its leader's pid (used
/// when the app is exiting and the async runtime may no longer get a chance
/// to run).
#[cfg(unix)]
pub fn terminate_pid(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
    }
}

#[cfg(windows)]
pub fn terminate_pid(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !h.is_null() {
            TerminateProcess(h, 1);
            CloseHandle(h);
        }
    }
}

/// Decode a line of ssh output. Win32-OpenSSH prints errors in the ANSI code
/// page (e.g. GBK on Chinese Windows) rather than UTF-8.
pub fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => decode_fallback(bytes),
    }
}

#[cfg(windows)]
fn decode_fallback(bytes: &[u8]) -> String {
    let acp = unsafe { windows_sys::Win32::Globalization::GetACP() };
    match u16::try_from(acp).ok().and_then(codepage::to_encoding) {
        Some(enc) => enc.decode(bytes).0.into_owned(),
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

#[cfg(not(windows))]
fn decode_fallback(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Open `url` in the default browser without blocking.
#[cfg(windows)]
pub fn open_url(url: &str) -> std::io::Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (verb, file) = (wide("open"), wide(url));
    let rc = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values <= 32 are errors (documented ShellExecute contract).
    if rc as isize > 32 {
        Ok(())
    } else {
        Err(std::io::Error::other(tr!(
            "ShellExecute 返回 {}",
            "ShellExecute returned {}",
            rc as isize
        )))
    }
}

#[cfg(not(windows))]
pub fn open_url(url: &str) -> std::io::Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Reap it in the background so it does not linger as a zombie.
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Open a text file in a plain text editor.
pub fn open_in_editor(path: &std::path::Path) -> std::io::Result<()> {
    let mut cmd = if cfg!(windows) {
        // `config` has no extension, so the shell would ask which app to use.
        std::process::Command::new("notepad.exe")
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg("-t");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    let mut child = cmd
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    /// Start PowerShell in a job, and have it start a grandchild the way
    /// ssh starts `ssh -W` for ProxyJump. Returns the tree and a handle to
    /// the grandchild (opened before anything is killed, so its pid cannot
    /// be reused under us).
    async fn tree_with_grandchild(tag: &str) -> (Child, ProcTree, HANDLE) {
        let out = std::env::temp_dir().join(format!("ssh2socks-job-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let mut cmd = Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$p = Start-Process -FilePath ping -ArgumentList '-n','3600','127.0.0.1' \
             -PassThru -WindowStyle Hidden; \
             Set-Content -Path $env:TREE_OUT -Value $p.Id; Wait-Process -Id $p.Id",
        ])
        .env("TREE_OUT", &out)
        .kill_on_drop(true);
        hide_console(&mut cmd);
        let child = cmd.spawn().unwrap();
        let tree = adopt(&child);
        assert_ne!(tree.job, 0, "could not put the process in a job");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let pid: u32 = loop {
            if let Some(pid) = std::fs::read_to_string(&out)
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                break pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "grandchild never started"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        let _ = std::fs::remove_file(&out);
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!handle.is_null(), "grandchild {pid} not found");
        (child, tree, handle)
    }

    fn ends_within_5s(handle: HANDLE) -> bool {
        let ended = unsafe { WaitForSingleObject(handle, 5_000) } == WAIT_OBJECT_0;
        unsafe { CloseHandle(handle) };
        ended
    }

    #[tokio::test]
    async fn kill_ends_the_whole_tree() {
        let (_child, tree, grandchild) = tree_with_grandchild("kill").await;
        tree.kill();
        assert!(ends_within_5s(grandchild));
    }

    /// What happens when the app dies: nothing calls kill, the job handle is
    /// just closed, and KILL_ON_JOB_CLOSE takes the tree down.
    #[tokio::test]
    async fn closing_the_job_ends_the_whole_tree() {
        let (_child, tree, grandchild) = tree_with_grandchild("close").await;
        let job = tree.job;
        std::mem::forget(tree);
        unsafe { CloseHandle(job as HANDLE) };
        assert!(ends_within_5s(grandchild));
    }
}
