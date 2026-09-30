//! Host keys of jump hosts.
//!
//! Options on ssh's command line (our `StrictHostKeyChecking=accept-new`,
//! `BatchMode=yes`) do not reach the ssh that ProxyJump / ProxyCommand start
//! for a jump host. A jump host seen for the first time therefore stops at
//! the interactive "Are you sure (yes/no)" prompt that nobody can answer,
//! and the connection times out. So before connecting, look each jump host up
//! in known_hosts and, if it is not there, connect to it once on its own with
//! accept-new: the same trust-on-first-use the tunnel's server already gets.
//! A *changed* key is still refused, and a jump host configured with
//! `StrictHostKeyChecking yes` is left alone.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use crate::platform;

const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
const TRUST_TIMEOUT: Duration = Duration::from_secs(20);
/// Jump hosts behind jump hosts; also stops a config that loops.
const MAX_DEPTH: usize = 4;

/// Options of ssh(1) that take an argument.
const WITH_ARG: &str = "BbcDEeFIiJLlmOoPpQRSWw";

/// What `ssh -G` says about a destination.
#[derive(Debug, Default)]
struct Resolved {
    hostname: String,
    port: String,
    proxy_jump: Option<String>,
    proxy_command: Option<String>,
    key_alias: Option<String>,
    known_files: Vec<String>,
    strict: String,
}

fn parse_config(text: &str) -> Resolved {
    let mut map: HashMap<&str, &str> = HashMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once(' ') {
            map.entry(k).or_insert(v.trim());
        }
    }
    let get = |k: &str| {
        map.get(k)
            .copied()
            .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("none"))
            .map(str::to_string)
    };
    let mut known_files = Vec::new();
    for key in ["userknownhostsfile", "globalknownhostsfile"] {
        if let Some(v) = map.get(key) {
            known_files.extend(v.split_whitespace().map(str::to_string));
        }
    }
    Resolved {
        hostname: get("hostname").unwrap_or_default(),
        port: get("port").unwrap_or_else(|| "22".into()),
        proxy_jump: get("proxyjump"),
        proxy_command: get("proxycommand"),
        key_alias: get("hostkeyalias"),
        known_files,
        strict: get("stricthostkeychecking").unwrap_or_default(),
    }
}

/// `host`, or `[host]:port` off port 22, as known_hosts stores it.
fn known_name(r: &Resolved) -> String {
    let host = r.key_alias.as_deref().unwrap_or(&r.hostname);
    if r.port == "22" {
        host.to_string()
    } else {
        format!("[{host}]:{}", r.port)
    }
}

/// `host:port` for `ssh -W`.
fn forward_target(r: &Resolved) -> String {
    if r.hostname.contains(':') {
        format!("[{}]:{}", r.hostname, r.port)
    } else {
        format!("{}:{}", r.hostname, r.port)
    }
}

/// ssh arguments for one ProxyJump entry, `[ssh://][user@]host[:port]`.
fn jump_spec_args(spec: &str) -> Option<Vec<String>> {
    let spec = spec.strip_prefix("ssh://").unwrap_or(spec);
    let (user, rest) = match spec.rsplit_once('@') {
        Some((u, r)) => (Some(u), r),
        None => (None, spec),
    };
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let (host, tail) = v6.split_once(']')?;
        match tail {
            "" => (host, None),
            t => (host, Some(t.strip_prefix(':')?)),
        }
    } else {
        match rest.split_once(':') {
            Some((h, p)) if !p.contains(':') => (h, Some(p)),
            Some(_) => (rest, None),
            None => (rest, None),
        }
    };
    if host.is_empty() || host.starts_with('-') || user == Some("") {
        return None;
    }
    let mut args = Vec::new();
    if let Some(u) = user {
        args.extend(["-l".to_string(), u.to_string()]);
    }
    if let Some(p) = port {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        args.extend(["-p".to_string(), p.to_string()]);
    }
    args.push(host.to_string());
    Some(args)
}

/// The jump host a destination connects through, as ssh arguments that
/// reach it the same way.
fn next_hop(r: &Resolved) -> Option<Vec<String>> {
    if let Some(jump) = &r.proxy_jump {
        // `a,b,c`: ssh reaches c with `-J a,b`.
        let (rest, last) = match jump.rsplit_once(',') {
            Some((rest, last)) => (Some(rest), last),
            None => (None, jump.as_str()),
        };
        let mut args = Vec::new();
        if let Some(rest) = rest {
            args.extend(["-J".to_string(), rest.to_string()]);
        }
        args.extend(jump_spec_args(last)?);
        return Some(args);
    }
    proxy_command_hop(r.proxy_command.as_deref()?)
}

/// A ProxyCommand of the plain form `[exec] ssh [options] -W %h:%p host`,
/// as ssh arguments without the `-W`. Anything else (a shell pipeline,
/// quoting, a remote command, nc, …) is not touched.
fn proxy_command_hop(command: &str) -> Option<Vec<String>> {
    if command.contains(['"', '\'', '`', '$', ';', '|', '&', '<', '>', '(', ')']) {
        return None;
    }
    let mut words = command.split_whitespace().peekable();
    if words.peek() == Some(&"exec") {
        words.next();
    }
    let program = words.next()?;
    let name = program.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
    if name != "ssh" && name != "ssh.exe" {
        return None;
    }
    let mut args = Vec::new();
    let mut forward = false;
    let mut destination = None;
    while let Some(word) = words.next() {
        if destination.is_some() {
            return None; // a remote command
        }
        let Some(flags) = word.strip_prefix('-').filter(|f| !f.is_empty()) else {
            destination = Some(word);
            continue;
        };
        match flags.char_indices().find(|&(_, c)| WITH_ARG.contains(c)) {
            None => args.push(word.to_string()),
            Some((i, c)) => {
                let attached = &flags[i + c.len_utf8()..];
                let value = if attached.is_empty() {
                    words.next()?.to_string()
                } else {
                    attached.to_string()
                };
                if c == 'W' {
                    if i > 0 {
                        return None;
                    }
                    forward = true;
                } else {
                    args.push(word.to_string());
                    if attached.is_empty() {
                        args.push(value);
                    }
                }
            }
        }
    }
    let destination = destination?;
    if !forward || destination.contains('%') {
        return None;
    }
    args.push(destination.to_string());
    Some(args)
}

async fn resolve(ssh: &str, args: &[String]) -> Option<Resolved> {
    let mut cmd = Command::new(ssh);
    cmd.arg("-G")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    platform::hide_console(&mut cmd);
    let out = timeout(LOOKUP_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    let r = parse_config(&String::from_utf8_lossy(&out.stdout));
    (!r.hostname.is_empty()).then_some(r)
}

/// `ssh-keygen` next to the ssh in use.
fn keygen_program(ssh: &str) -> String {
    let path = std::path::Path::new(ssh);
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => {
            let exe = if cfg!(windows) {
                "ssh-keygen.exe"
            } else {
                "ssh-keygen"
            };
            dir.join(exe).to_string_lossy().into_owned()
        }
        None => "ssh-keygen".into(),
    }
}

fn expand_home(file: &str) -> std::path::PathBuf {
    match (
        file.strip_prefix("~/").or(file.strip_prefix("~\\")),
        dirs::home_dir(),
    ) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => file.into(),
    }
}

/// Whether any known_hosts file has a key for this host. Unknown (false)
/// when it can't be checked; the worst case is one extra connection.
async fn is_known(ssh: &str, r: &Resolved) -> bool {
    let name = known_name(r);
    for file in &r.known_files {
        let path = expand_home(file);
        if !path.is_file() {
            continue;
        }
        let mut cmd = Command::new(keygen_program(ssh));
        cmd.arg("-F")
            .arg(&name)
            .arg("-f")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        platform::hide_console(&mut cmd);
        if let Ok(Ok(status)) = timeout(LOOKUP_TIMEOUT, cmd.status()).await {
            if status.success() {
                return true;
            }
        }
    }
    false
}

/// Connect to the jump host once, forwarding to where the real connection
/// goes: the same authentication and `-W` the tunnel does, so nothing new
/// happens on either server. Returns ssh's last error line.
async fn connect_once(ssh: &str, hop: &[String], target: &str) -> Option<String> {
    let (destination, options) = hop.split_last()?;
    let mut cmd = Command::new(ssh);
    cmd.args([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "ConnectTimeout=10",
    ])
    .args(options)
    .args(["-W", target])
    .arg(destination)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    platform::hide_console(&mut cmd);
    platform::prepare(&mut cmd);
    let child = cmd.spawn().ok()?;
    let _tree = platform::adopt(&child);
    let out = timeout(TRUST_TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    platform::decode(&out.stderr)
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_string)
}

/// Make sure every jump host on the way to `destination` has its host key in
/// known_hosts, recording unknown ones (see the module docs). `log` gets a
/// line for each jump host that needed it.
pub async fn trust(ssh: &str, destination: &str, log: &(dyn Fn(String) + Send + Sync)) {
    let mut seen = Vec::new();
    trust_path(ssh, vec![destination.to_string()], 0, &mut seen, log).await;
}

async fn trust_path(
    ssh: &str,
    args: Vec<String>,
    depth: usize,
    seen: &mut Vec<Vec<String>>,
    log: &(dyn Fn(String) + Send + Sync),
) {
    if depth >= MAX_DEPTH || seen.contains(&args) {
        return;
    }
    seen.push(args.clone());
    let Some(here) = resolve(ssh, &args).await else {
        return;
    };
    let Some(hop) = next_hop(&here) else {
        return;
    };
    // Jump hosts further out first: this one is reached through them.
    Box::pin(trust_path(ssh, hop.clone(), depth + 1, seen, log)).await;
    let Some(jump) = resolve(ssh, &hop).await else {
        return;
    };
    if matches!(jump.strict.as_str(), "true" | "yes") || is_known(ssh, &jump).await {
        return;
    }
    let name = known_name(&jump);
    log(tr!(
        "跳板机 {name} 不在 known_hosts 中，首次连接并记录它的主机密钥（ssh 命令行上的选项不会传给跳板机）…",
        "Jump host {name} is not in known_hosts; connecting once to record its host key (ssh options on the command line do not reach jump hosts)…"
    ));
    let error = connect_once(ssh, &hop, &forward_target(&here)).await;
    if is_known(ssh, &jump).await {
        log(tr!(
            "已记录跳板机 {name} 的主机密钥",
            "Recorded the host key of jump host {name}"
        ));
    } else {
        let detail = error.unwrap_or_default();
        log(tr!(
            "未能记录跳板机 {name} 的主机密钥：{detail}",
            "Could not record the host key of jump host {name}: {detail}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_jump_specs() {
        assert_eq!(jump_spec_args("lab"), Some(strs(&["lab"])));
        assert_eq!(
            jump_spec_args("u@a:2222"),
            Some(strs(&["-l", "u", "-p", "2222", "a"]))
        );
        assert_eq!(jump_spec_args("ssh://b"), Some(strs(&["b"])));
        assert_eq!(jump_spec_args("[::1]:23"), Some(strs(&["-p", "23", "::1"])));
        assert_eq!(jump_spec_args("-oProxyCommand=x"), None);
        assert_eq!(jump_spec_args("a:port"), None);
    }

    #[test]
    fn finds_the_next_hop() {
        let r = |jump: Option<&str>, cmd: Option<&str>| Resolved {
            proxy_jump: jump.map(Into::into),
            proxy_command: cmd.map(Into::into),
            ..Default::default()
        };
        assert_eq!(next_hop(&r(Some("a"), None)), Some(strs(&["a"])));
        assert_eq!(
            next_hop(&r(Some("a,u@b:2200,c"), None)),
            Some(strs(&["-J", "a,u@b:2200", "c"]))
        );
        assert_eq!(
            next_hop(&r(None, Some("exec ssh -W 172.31.15.12:22 pm-eu"))),
            Some(strs(&["pm-eu"]))
        );
        assert_eq!(
            next_hop(&r(
                None,
                Some("C:\\Windows\\System32\\OpenSSH\\ssh.exe -q -p2222 -l me -W %h:%p jump")
            )),
            Some(strs(&["-q", "-p2222", "-l", "me", "jump"]))
        );
        assert_eq!(
            next_hop(&r(None, Some("ssh -W%h:%p -i ~/.ssh/k jump"))),
            Some(strs(&["-i", "~/.ssh/k", "jump"]))
        );
        for no in [
            "nc %h %p",
            "ssh jump nc %h %p",
            "ssh -W %h:%p jump extra",
            "ssh -qW %h:%p jump",
            "ssh -W %h:%p %r@jump",
            "ssh -W %h:%p jump | cat",
            "sh -c 'ssh -W %h:%p jump'",
            "ssh -q jump",
        ] {
            assert_eq!(next_hop(&r(None, Some(no))), None, "{no}");
        }
    }

    #[test]
    fn reads_ssh_g_output() {
        let r = parse_config(
            "user flab\nhostname 10.0.0.5\nport 2222\nstricthostkeychecking ask\n\
             hostkeyalias none\nglobalknownhostsfile /etc/ssh/ssh_known_hosts /etc/ssh/ssh_known_hosts2\n\
             userknownhostsfile ~/.ssh/known_hosts ~/.ssh/known_hosts2\nproxycommand exec ssh -W %h:%p pm-eu\n",
        );
        assert_eq!(known_name(&r), "[10.0.0.5]:2222");
        assert_eq!(forward_target(&r), "10.0.0.5:2222");
        assert_eq!(r.known_files.len(), 4);
        assert_eq!(r.known_files[0], "~/.ssh/known_hosts");
        assert_eq!(r.proxy_jump, None);
        assert_eq!(next_hop(&r), Some(strs(&["pm-eu"])));
        let v6 = parse_config("hostname ::1\nport 22\nhostkeyalias box\n");
        assert_eq!(known_name(&v6), "box");
        assert_eq!(forward_target(&v6), "[::1]:22");
    }
}
