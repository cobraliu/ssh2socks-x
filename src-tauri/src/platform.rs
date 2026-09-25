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

/// Tie the child's lifetime to ours.
///
/// Windows does not kill children when the parent dies, so every ssh.exe is
/// put into one job object flagged KILL_ON_JOB_CLOSE: when this process exits
/// (even by crashing) the handle closes and Windows kills the tunnels too.
#[cfg(windows)]
pub fn adopt(child: &Child) {
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    static JOB: OnceLock<usize> = OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return 0;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        job as usize
    });
    if let (true, Some(raw)) = (job != 0, child.raw_handle()) {
        unsafe {
            AssignProcessToJobObject(job as HANDLE, raw as HANDLE);
        }
    }
}

/// On Linux ask the kernel to SIGTERM ssh if we die; other Unixes rely on
/// the explicit cleanup at exit.
#[cfg(target_os = "linux")]
pub fn prepare(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
pub fn prepare(_cmd: &mut Command) {}

#[cfg(not(windows))]
pub fn adopt(_child: &Child) {}

/// Synchronously terminate a child by pid (used when the app is exiting and
/// the async runtime may no longer get a chance to run).
#[cfg(unix)]
pub fn terminate_pid(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
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
