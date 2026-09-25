//! Visual editing of `~/.ssh/config` Host blocks.
//!
//! Edits are surgical: only the Host line and the handful of options the UI
//! manages (HostName, User, Port, IdentityFile, ProxyJump, ProxyCommand) are
//! touched. Comments, blank lines, other options, `Include`/`Match` sections,
//! line endings and the file itself (permissions, symlinks) are preserved;
//! the previous content is copied to `<file>.ssh2socks.bak` before writing.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ssh_config::{expand_include, split_keyword};

/// Options managed by the form, with their canonical spelling.
const MANAGED: [&str; 6] = [
    "HostName",
    "User",
    "Port",
    "IdentityFile",
    "ProxyJump",
    "ProxyCommand",
];

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct HostBlock {
    pub file: String,
    /// 0-based index of the `Host` line; with `patterns` it identifies the
    /// block and detects concurrent edits.
    pub line: usize,
    pub patterns: String,
    pub hostname: String,
    pub user: String,
    pub port: String,
    pub identity_file: String,
    pub proxy_jump: String,
    pub proxy_command: String,
    /// Other option lines of the block, kept as-is.
    pub others: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct HostInput {
    /// Set when editing an existing block; absent for a new one.
    pub file: Option<String>,
    pub line: Option<usize>,
    pub original_patterns: Option<String>,
    pub patterns: String,
    pub hostname: String,
    pub user: String,
    pub port: String,
    pub identity_file: String,
    pub proxy_jump: String,
    pub proxy_command: String,
}

impl HostInput {
    fn values(&self) -> [&str; 6] {
        [
            &self.hostname,
            &self.user,
            &self.port,
            &self.identity_file,
            &self.proxy_jump,
            &self.proxy_command,
        ]
    }
}

pub fn main_config() -> Option<PathBuf> {
    crate::ssh_config::ssh_dir().map(|d| d.join("config"))
}

// ---- text model -------------------------------------------------------------

struct Doc {
    lines: Vec<String>,
    eol: &'static str,
}

impl Doc {
    fn read(path: &Path) -> Result<Doc, String> {
        let text = match fs::read(path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("读取 {} 失败：{e}", path.display())),
        };
        Ok(Doc::parse(&text))
    }

    fn parse(text: &str) -> Doc {
        let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let mut lines: Vec<String> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect();
        if lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        Doc { lines, eol }
    }

    fn render(&self) -> String {
        let mut out = self.lines.join(self.eol);
        if !out.is_empty() {
            out.push_str(self.eol);
        }
        out
    }

    fn keyword(&self, i: usize) -> String {
        let line = self.lines[i].trim();
        if line.is_empty() || line.starts_with('#') {
            return String::new();
        }
        split_keyword(line).0
    }

    /// `[start, end)` ranges of every Host block.
    fn blocks(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut open: Option<usize> = None;
        for i in 0..self.lines.len() {
            let kw = self.keyword(i);
            if kw == "host" || kw == "match" {
                if let Some(start) = open.take() {
                    out.push((start, i));
                }
                if kw == "host" {
                    open = Some(i);
                }
            }
        }
        if let Some(start) = open {
            out.push((start, self.lines.len()));
        }
        out
    }

    /// Indentation used for options in this file, so new blocks match it.
    fn option_indent(&self) -> String {
        self.lines
            .iter()
            .find(|l| {
                let t = l.trim_start();
                !t.is_empty() && !t.starts_with('#') && t.len() < l.len()
            })
            .map(|l| l[..l.len() - l.trim_start().len()].to_string())
            .unwrap_or_else(|| "    ".to_string())
    }

    fn block_at(&self, line: usize) -> Option<(usize, usize)> {
        self.blocks().into_iter().find(|(s, _)| *s == line)
    }
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    v.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(v)
        .to_string()
}

fn quote_if_needed(key: &str, v: &str) -> String {
    // ProxyCommand is passed to a shell verbatim; never quote it.
    if key != "ProxyCommand" && v.contains(char::is_whitespace) && !v.starts_with('"') {
        format!("\"{v}\"")
    } else {
        v.to_string()
    }
}

fn block_view(doc: &Doc, file: &Path, (start, end): (usize, usize)) -> HostBlock {
    let mut b = HostBlock {
        file: file.display().to_string(),
        line: start,
        patterns: split_keyword(doc.lines[start].trim()).1,
        ..Default::default()
    };
    for i in start + 1..end {
        let raw = doc.lines[i].trim();
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        let (kw, value) = split_keyword(raw);
        let slot = match kw.as_str() {
            "hostname" => &mut b.hostname,
            "user" => &mut b.user,
            "port" => &mut b.port,
            "identityfile" => &mut b.identity_file,
            "proxyjump" => &mut b.proxy_jump,
            "proxycommand" => &mut b.proxy_command,
            _ => {
                b.others.push(raw.to_string());
                continue;
            }
        };
        if slot.is_empty() {
            // ssh uses the first value; later duplicates stay untouched.
            *slot = if kw == "proxycommand" {
                value
            } else {
                unquote(&value)
            };
        } else {
            b.others.push(raw.to_string());
        }
    }
    b
}

// ---- listing ------------------------------------------------------------------

pub fn list_blocks() -> Vec<HostBlock> {
    let Some(config) = main_config() else {
        return Vec::new();
    };
    let base = config.parent().map(Path::to_path_buf).unwrap_or_default();
    list_blocks_from(&config, &base)
}

pub fn list_blocks_from(config: &Path, base: &Path) -> Vec<HostBlock> {
    let mut out = Vec::new();
    collect(config, base, &mut HashSet::new(), &mut out);
    out
}

fn collect(path: &Path, base: &Path, seen: &mut HashSet<PathBuf>, out: &mut Vec<HostBlock>) {
    let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !real.is_file() || !seen.insert(real) {
        return;
    }
    let Ok(doc) = Doc::read(path) else { return };
    // Keep ssh's reading order: an Include pulls its blocks in where it
    // appears, so walk lines and blocks together.
    let blocks = doc.blocks();
    let mut next_block = blocks.iter().peekable();
    for i in 0..doc.lines.len() {
        if let Some(&&(start, end)) = next_block.peek() {
            if start == i {
                out.push(block_view(&doc, path, (start, end)));
                next_block.next();
            }
        }
        if doc.keyword(i) == "include" {
            let value = split_keyword(doc.lines[i].trim()).1;
            for token in value.split_whitespace() {
                for file in expand_include(token, base) {
                    collect(&file, base, seen, out);
                }
            }
        }
    }
}

// ---- editing ------------------------------------------------------------------

fn validate(input: &HostInput) -> Result<(), String> {
    let patterns = input.patterns.trim();
    if patterns.is_empty() {
        return Err("请填写别名（Host）。".into());
    }
    if input
        .values()
        .iter()
        .chain([&patterns])
        .any(|v| v.contains(['\n', '\r']))
    {
        return Err("字段中不能包含换行。".into());
    }
    let port = input.port.trim();
    if !port.is_empty() && port.parse::<u16>().map_or(true, |p| p == 0) {
        return Err("端口必须在 1–65535 之间。".into());
    }
    if !input.proxy_jump.trim().is_empty() && !input.proxy_command.trim().is_empty() {
        return Err("ProxyJump 和 ProxyCommand 只能选一个。".into());
    }
    Ok(())
}

/// Rewrite the managed options of one block body in place.
fn apply(body: &mut Vec<String>, input: &HostInput, indent: &str) {
    for (key, value) in MANAGED.iter().zip(input.values()) {
        let value = value.trim();
        let lower = key.to_lowercase();
        let hit = body.iter().position(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#') && split_keyword(t).0 == lower
        });
        let rendered = format!("{indent}{key} {}", quote_if_needed(key, value));
        match (hit, value.is_empty()) {
            (Some(i), true) => {
                body.remove(i);
            }
            (Some(i), false) => {
                let old = split_keyword(body[i].trim()).1;
                // Keep the line byte-for-byte if the value did not change.
                let same = if *key == "ProxyCommand" {
                    old == value
                } else {
                    unquote(&old) == value
                };
                if !same {
                    body[i] = rendered;
                }
            }
            (None, false) => {
                let at = body
                    .iter()
                    .rposition(|l| {
                        let t = l.trim();
                        !t.is_empty() && !t.starts_with('#')
                    })
                    .map_or(0, |i| i + 1);
                body.insert(at, rendered);
            }
            (None, true) => {}
        }
    }
}

fn concrete_aliases(patterns: &str) -> impl Iterator<Item = &str> {
    patterns
        .split_whitespace()
        .filter(|a| !a.contains(['*', '?', '!']))
}

pub fn save_block(config: &Path, input: &HostInput) -> Result<HostBlock, String> {
    validate(input)?;
    let patterns = input
        .patterns
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    match (&input.file, input.line) {
        (Some(file), Some(line)) => {
            let path = PathBuf::from(file);
            let mut doc = Doc::read(&path)?;
            let (start, end) = doc
                .block_at(line)
                .filter(|&(s, _)| {
                    Some(split_keyword(doc.lines[s].trim()).1.as_str())
                        == input.original_patterns.as_deref()
                })
                .ok_or("配置文件已在别处被修改，请刷新后重试。")?;
            let host_line = doc.lines[start].clone();
            let lead = &host_line[..host_line.len() - host_line.trim_start().len()];
            if split_keyword(host_line.trim()).1 != patterns {
                doc.lines[start] = format!("{lead}Host {patterns}");
            }
            let mut body = doc.lines[start + 1..end].to_vec();
            let indent = body
                .iter()
                .find(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
                .map(|l| l[..l.len() - l.trim_start().len()].to_string())
                .unwrap_or_else(|| format!("{lead}    "));
            apply(&mut body, input, &indent);
            doc.lines.splice(start + 1..end, body);
            write(&path, &doc)?;
            Ok(block_view(
                &doc,
                &path,
                doc.block_at(start).unwrap_or((start, start + 1)),
            ))
        }
        _ => {
            let base = config.parent().map(Path::to_path_buf).unwrap_or_default();
            let existing: HashSet<String> = list_blocks_from(config, &base)
                .iter()
                .flat_map(|b| {
                    concrete_aliases(&b.patterns)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .collect();
            if let Some(dup) = concrete_aliases(&patterns).find(|a| existing.contains(*a)) {
                return Err(format!("主机「{dup}」已存在。"));
            }
            let mut doc = Doc::read(config)?;
            let mut block = vec![format!("Host {patterns}")];
            apply(&mut block, input, &doc.option_indent());
            // ssh takes the first value it sees, so a new host must come
            // before catch-all sections (`Host *`, `Match`) to take effect.
            let at = (0..doc.lines.len())
                .find(|&i| match doc.keyword(i).as_str() {
                    "match" => true,
                    "host" => split_keyword(doc.lines[i].trim())
                        .1
                        .split_whitespace()
                        .any(|p| p.contains(['*', '?'])),
                    _ => false,
                })
                .unwrap_or(doc.lines.len());
            let gap_before = at > 0 && !doc.lines[at - 1].trim().is_empty();
            let host_line = at + usize::from(gap_before);
            let mut insert = Vec::new();
            if gap_before {
                insert.push(String::new());
            }
            insert.extend(block);
            if at < doc.lines.len() {
                insert.push(String::new());
            }
            doc.lines.splice(at..at, insert);
            write(config, &doc)?;
            let range = doc
                .block_at(host_line)
                .unwrap_or((host_line, host_line + 1));
            Ok(block_view(&doc, config, range))
        }
    }
}

pub fn delete_block(file: &str, line: usize, patterns: &str) -> Result<(), String> {
    let path = PathBuf::from(file);
    let mut doc = Doc::read(&path)?;
    let (start, end) = doc
        .block_at(line)
        .filter(|&(s, _)| split_keyword(doc.lines[s].trim()).1 == patterns)
        .ok_or("配置文件已在别处被修改，请刷新后重试。")?;
    doc.lines.drain(start..end);
    // Collapse the blank line left between the neighbours.
    if start > 0
        && start < doc.lines.len()
        && doc.lines[start - 1].trim().is_empty()
        && doc.lines[start].trim().is_empty()
    {
        doc.lines.remove(start);
    }
    while doc.lines.last().is_some_and(|l| l.trim().is_empty()) {
        doc.lines.pop();
    }
    write(&path, &doc)
}

fn write(path: &Path, doc: &Doc) -> Result<(), String> {
    let err = |e: std::io::Error| format!("写入 {} 失败：{e}", path.display());
    let created = !path.exists();
    if !created {
        let mut backup = path.as_os_str().to_owned();
        backup.push(".ssh2socks.bak");
        fs::copy(path, PathBuf::from(backup)).map_err(err)?;
    } else if let Some(dir) = path.parent() {
        create_ssh_dir(dir).map_err(err)?;
    }
    // Rewrite in place (not tmp + rename) so permissions, ACLs and symlinks
    // of the existing file survive.
    fs::write(path, doc.render()).map_err(err)?;
    if created {
        restrict(path);
    }
    Ok(())
}

#[cfg(unix)]
fn create_ssh_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_ssh_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)
}

/// A config we create is private to the user, like ssh's own files.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ssh2socks-edit-{}", crate::models::new_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn input(patterns: &str) -> HostInput {
        HostInput {
            patterns: patterns.into(),
            ..Default::default()
        }
    }

    const CONFIG: &str = "# my config\r\n\
Include conf.d/*\r\n\
\r\n\
Host web\r\n\
\x20 # production box\r\n\
\x20 HostName 10.0.0.5\r\n\
\x20 User root\r\n\
\x20 ServerAliveInterval 30\r\n\
\r\n\
Host *\r\n\
\x20 AddKeysToAgent yes\r\n";

    #[test]
    fn lists_blocks_in_ssh_order_with_includes() {
        let dir = tmp();
        fs::create_dir_all(dir.join("conf.d")).unwrap();
        fs::write(dir.join("config"), CONFIG).unwrap();
        fs::write(
            dir.join("conf.d/jump"),
            "Host jump\n  HostName=j.example.com\n  ProxyCommand ssh -W %h:%p gw\n",
        )
        .unwrap();
        let blocks = list_blocks_from(&dir.join("config"), &dir);
        let names: Vec<&str> = blocks.iter().map(|b| b.patterns.as_str()).collect();
        assert_eq!(names, ["jump", "web", "*"]);
        assert_eq!(blocks[0].hostname, "j.example.com");
        assert_eq!(blocks[0].proxy_command, "ssh -W %h:%p gw");
        assert_eq!(blocks[1].line, 3);
        assert_eq!(blocks[1].user, "root");
        assert_eq!(blocks[1].others, ["ServerAliveInterval 30"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn edit_touches_only_managed_lines() {
        let dir = tmp();
        let cfg = dir.join("config");
        fs::write(&cfg, CONFIG).unwrap();
        let edit = HostInput {
            file: Some(cfg.display().to_string()),
            line: Some(3),
            original_patterns: Some("web".into()),
            patterns: "web prod".into(),
            hostname: "10.0.0.5".into(),
            user: String::new(),
            port: "2222".into(),
            identity_file: "~/.ssh/my key".into(),
            proxy_command: "nc -X 5 -x 127.0.0.1:1080 %h %p".into(),
            ..Default::default()
        };
        let saved = save_block(&cfg, &edit).unwrap();
        assert_eq!(saved.port, "2222");
        assert_eq!(saved.identity_file, "~/.ssh/my key");
        let text = fs::read_to_string(&cfg).unwrap();
        assert_eq!(
            text,
            "# my config\r\n\
Include conf.d/*\r\n\
\r\n\
Host web prod\r\n\
\x20 # production box\r\n\
\x20 HostName 10.0.0.5\r\n\
\x20 ServerAliveInterval 30\r\n\
\x20 Port 2222\r\n\
\x20 IdentityFile \"~/.ssh/my key\"\r\n\
\x20 ProxyCommand nc -X 5 -x 127.0.0.1:1080 %h %p\r\n\
\r\n\
Host *\r\n\
\x20 AddKeysToAgent yes\r\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("config.ssh2socks.bak")).unwrap(),
            CONFIG
        );

        // Stale identity is rejected instead of editing the wrong block.
        assert!(save_block(&cfg, &edit).unwrap_err().contains("刷新"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn add_goes_before_catch_all_and_rejects_duplicates() {
        let dir = tmp();
        let cfg = dir.join("config");
        fs::write(&cfg, CONFIG).unwrap();
        let new = HostInput {
            hostname: "192.168.1.9".into(),
            proxy_jump: "web".into(),
            ..input("nas")
        };
        let saved = save_block(&cfg, &new).unwrap();
        assert_eq!((saved.line, saved.proxy_jump.as_str()), (9, "web"));
        let text = fs::read_to_string(&cfg).unwrap();
        assert!(text.contains(
            "  ServerAliveInterval 30\r\n\r\nHost nas\r\n  HostName 192.168.1.9\r\n  ProxyJump web\r\n\r\nHost *\r\n"
        ), "{text}");
        assert!(save_block(&cfg, &input("web"))
            .unwrap_err()
            .contains("已存在"));

        let both = HostInput {
            proxy_jump: "a".into(),
            proxy_command: "b".into(),
            ..input("x")
        };
        assert!(save_block(&cfg, &both).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn add_to_missing_file_and_delete() {
        let dir = tmp();
        let cfg = dir.join("sub").join("config");
        save_block(
            &cfg,
            &HostInput {
                hostname: "a.example".into(),
                ..input("a")
            },
        )
        .unwrap();
        save_block(
            &cfg,
            &HostInput {
                user: "me".into(),
                ..input("b")
            },
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&cfg).unwrap(),
            "Host a\n    HostName a.example\n\nHost b\n    User me\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&cfg).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
        delete_block(&cfg.display().to_string(), 0, "a").unwrap();
        assert_eq!(fs::read_to_string(&cfg).unwrap(), "Host b\n    User me\n");
        assert!(delete_block(&cfg.display().to_string(), 0, "a").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
