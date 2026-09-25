//! Cleans up ssh processes left running after the app was killed or crashed
//! (Unix only; on Windows the job objects in `platform` already take ssh down
//! with the app).
//!
//! Every ssh we start is recorded, with its start time and name, in a file
//! named after this app instance. On the next launch the files of instances
//! that are no longer running are replayed, and an ssh is sent SIGTERM only
//! when a process with the same pid, the same start time and the same name
//! is still running in the same boot, i.e. it is provably the process we
//! started. Anything that fails a check, or that cannot be checked, is left
//! alone: missing a leftover is acceptable, signalling someone else's
//! process is not. Only the recorded ssh itself is signalled; ssh ends its
//! own ProxyJump / ProxyCommand helper when it exits.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

/// What identifies one process: a pid alone can be reused, but not together
/// with the exact moment the process started.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub start: u64,
    pub name: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Record {
    /// Start times are only comparable within one boot.
    boot: String,
    /// The app instance that started `procs`.
    owner: Option<Proc>,
    procs: Vec<Proc>,
}

struct Registry {
    path: PathBuf,
    rec: Record,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn registry() -> MutexGuard<'static, Option<Registry>> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}

/// Clean up after earlier instances that are gone, then start recording
/// this one's ssh processes. Returns how many leftovers were stopped.
pub fn init(config_dir: &Path) -> usize {
    let dir = config_dir.join("run");
    let reaped = reap_dir(&dir);
    let (Some(owner), Some(boot)) = (identify(std::process::id()), boot_id()) else {
        return reaped;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return reaped;
    }
    let path = dir.join(format!("{}.json", owner.pid));
    let rec = Record {
        boot,
        owner: Some(owner),
        procs: Vec::new(),
    };
    save(&path, &rec);
    *registry() = Some(Registry { path, rec });
    reaped
}

/// Record a freshly started ssh.
pub fn track(pid: u32) {
    if let Some(p) = identify(pid) {
        update(|rec| rec.procs.push(p));
    }
}

/// Forget an ssh we have stopped or seen exit.
pub fn untrack(pid: u32) {
    update(|rec| rec.procs.retain(|p| p.pid != pid));
}

fn update(f: impl FnOnce(&mut Record)) {
    if let Some(reg) = registry().as_mut() {
        f(&mut reg.rec);
        save(&reg.path, &reg.rec);
    }
}

fn save(path: &Path, rec: &Record) {
    if let Ok(json) = serde_json::to_string(rec) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// Replay the record files of instances that are no longer running.
fn reap_dir(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut reaped = 0;
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension() != Some(std::ffi::OsStr::new("json")) {
            continue;
        }
        let rec: Record = match std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            Some(rec) => rec,
            None => {
                // Unreadable: nothing in it can be trusted, so just drop it.
                let _ = std::fs::remove_file(&path);
                continue;
            }
        };
        // That instance is still running and owns its ssh processes.
        if rec
            .owner
            .as_ref()
            .is_some_and(|o| identify(o.pid).as_ref() == Some(o))
        {
            continue;
        }
        reaped += reap(&rec);
        let _ = std::fs::remove_file(&path);
    }
    reaped
}

fn reap(rec: &Record) -> usize {
    if rec.boot.is_empty() || boot_id().as_deref() != Some(rec.boot.as_str()) {
        return 0;
    }
    rec.procs.iter().filter(|p| terminate_if(p)).count()
}

// ---- Linux ---------------------------------------------------------------

/// Identity from /proc/<pid>/stat: the name, and the start time in clock
/// ticks since boot (field 22). Zombies count as gone.
#[cfg(target_os = "linux")]
fn identify(pid: u32) -> Option<Proc> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(pid, &stat)
}

#[cfg(target_os = "linux")]
fn parse_stat(pid: u32, stat: &str) -> Option<Proc> {
    // The name is in parentheses and may itself contain ") ".
    let (head, rest) = stat.rsplit_once(')')?;
    let name = head.split_once('(')?.1.to_string();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // `fields` starts at field 3 (state).
    if matches!(fields.first(), None | Some(&"Z") | Some(&"X")) {
        return None;
    }
    let start = fields.get(19)?.parse().ok()?;
    Some(Proc { pid, start, name })
}

#[cfg(target_os = "linux")]
fn boot_id() -> Option<String> {
    let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(id.trim().to_string()).filter(|s| !s.is_empty())
}

/// Pin the process with a pidfd first, then check who it is: if the pid was
/// reused in between, the check sees the new process and fails; if the check
/// passes, the pidfd refers to exactly that process, and signalling it can
/// never reach another one. Kernels without pidfd (< 5.3): do nothing.
#[cfg(target_os = "linux")]
fn terminate_if(p: &Proc) -> bool {
    let Some(pid) = i32::try_from(p.pid).ok().filter(|&pid| pid > 1) else {
        return false;
    };
    unsafe {
        let fd = libc::syscall(libc::SYS_pidfd_open, pid, 0);
        if fd < 0 {
            return false;
        }
        let fd = fd as libc::c_int;
        let ok = identify(p.pid).as_ref() == Some(p)
            && libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd,
                libc::SIGTERM,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            ) == 0;
        libc::close(fd);
        ok
    }
}

// ---- macOS ---------------------------------------------------------------

/// Identity from proc_pidinfo: the name, and the start time in microseconds
/// since the epoch. Zombies count as gone.
#[cfg(target_os = "macos")]
fn identify(pid: u32) -> Option<Proc> {
    const SZOMB: u32 = 5;
    let ipid = i32::try_from(pid).ok().filter(|&p| p > 0)?;
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            ipid,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n != size || info.pbi_pid != pid || info.pbi_status == SZOMB {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(info.pbi_comm.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    let start = info
        .pbi_start_tvsec
        .checked_mul(1_000_000)?
        .checked_add(info.pbi_start_tvusec)?;
    Some(Proc { pid, start, name })
}

#[cfg(target_os = "macos")]
fn boot_id() -> Option<String> {
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::timeval>();
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            &mut tv as *mut _ as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && tv.tv_sec > 0).then(|| format!("{}.{}", tv.tv_sec, tv.tv_usec))
}

/// macOS has no pidfd. The check and the signal are back to back, so the
/// pid would have to be freed and handed to a new process in between, which
/// takes a full cycle through the pid space within microseconds.
#[cfg(target_os = "macos")]
fn terminate_if(p: &Proc) -> bool {
    let Some(pid) = i32::try_from(p.pid).ok().filter(|&pid| pid > 1) else {
        return false;
    };
    identify(p.pid).as_ref() == Some(p) && unsafe { libc::kill(pid, libc::SIGTERM) } == 0
}

// ---- other Unixes: record nothing, reap nothing --------------------------

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn identify(_pid: u32) -> Option<Proc> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn boot_id() -> Option<String> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn terminate_if(_p: &Proc) -> bool {
    false
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::process::{Child, Command};

    fn sleeper() -> Child {
        Command::new("sleep").arg("3600").spawn().unwrap()
    }

    fn running(child: &mut Child) -> bool {
        // Give SIGTERM a moment, then reap so a killed child isn't a zombie.
        for _ in 0..50 {
            if child.try_wait().unwrap().is_some() {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        true
    }

    fn write(dir: &Path, owner: Option<Proc>, procs: Vec<Proc>, boot: Option<String>) {
        std::fs::create_dir_all(dir).unwrap();
        let rec = Record {
            boot: boot.unwrap_or_default(),
            owner,
            procs,
        };
        save(&dir.join("1.json"), &rec);
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ssh2socks-reaper-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// An owner that provably is not running: our own pid, wrong start time.
    fn dead_owner() -> Option<Proc> {
        let mut me = identify(std::process::id()).unwrap();
        me.start += 1;
        Some(me)
    }

    #[test]
    fn parses_names_with_parens() {
        let stat = "42 (a) b) S 1 42 42 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 12345 0 0";
        let p = parse_stat(42, stat).unwrap();
        assert_eq!((p.name.as_str(), p.start), ("a) b", 12345));
        let zombie = stat.replace(") S ", ") Z ");
        assert!(parse_stat(42, &zombie).is_none());
    }

    #[test]
    fn stops_a_verified_leftover() {
        let dir = tmpdir("ok");
        let mut child = sleeper();
        let p = identify(child.id()).unwrap();
        write(&dir, dead_owner(), vec![p], boot_id());
        assert_eq!(reap_dir(&dir), 1);
        assert!(!running(&mut child));
        assert!(!dir.join("1.json").exists());
    }

    #[test]
    fn leaves_anything_that_does_not_match() {
        let dir = tmpdir("mismatch");
        let mut child = sleeper();
        let real = identify(child.id()).unwrap();
        let reused = Proc {
            start: real.start + 1,
            ..real.clone()
        };
        let renamed = Proc {
            name: "ssh".into(),
            ..real.clone()
        };
        let bad_pids = [0u32, 1, u32::MAX]
            .map(|pid| Proc {
                pid,
                ..real.clone()
            })
            .to_vec();
        // Wrong start time (pid reused), wrong name, nonsense pids.
        let mut procs = vec![reused, renamed];
        procs.extend(bad_pids);
        write(&dir, dead_owner(), procs, boot_id());
        assert_eq!(reap_dir(&dir), 0);
        // Right process, but recorded in another boot or with no boot id.
        write(
            &dir,
            dead_owner(),
            vec![real.clone()],
            Some("other-boot".into()),
        );
        assert_eq!(reap_dir(&dir), 0);
        write(&dir, dead_owner(), vec![real.clone()], None);
        assert_eq!(reap_dir(&dir), 0);
        // Right process, but the instance that owns it is still running.
        write(&dir, identify(std::process::id()), vec![real], boot_id());
        assert_eq!(reap_dir(&dir), 0);
        assert!(dir.join("1.json").exists());
        assert!(running(&mut child));
        child.kill().unwrap();
        child.wait().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
