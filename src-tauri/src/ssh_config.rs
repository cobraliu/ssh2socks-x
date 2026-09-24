//! Minimal `~/.ssh/config` reader.
//!
//! Extracts concrete Host aliases together with their HostName so the UI can
//! offer a searchable picker. Expands `Include` (with `*`/`?` in the file name)
//! and skips wildcard / negated patterns such as `Host *`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HostEntry {
    pub alias: String,
    pub hostname: String,
}

fn ssh_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh"))
}

pub fn load_hosts() -> Vec<HostEntry> {
    match ssh_dir() {
        Some(dir) => load_hosts_from(&dir.join("config"), &dir),
        None => Vec::new(),
    }
}

/// `base` is where relative `Include` paths resolve (`~/.ssh` for user config).
pub fn load_hosts_from(config: &Path, base: &Path) -> Vec<HostEntry> {
    let mut lines = Vec::new();
    collect_lines(config, base, &mut HashSet::new(), &mut lines);

    let mut blocks: Vec<(Vec<String>, Option<String>)> = Vec::new();
    let mut in_host = false;
    for (keyword, value) in lines {
        match keyword.as_str() {
            "host" => {
                blocks.push((value.split_whitespace().map(str::to_string).collect(), None));
                in_host = true;
            }
            // A Match block ends the previous Host block.
            "match" => in_host = false,
            "hostname" if in_host => {
                if let Some(block) = blocks.last_mut() {
                    block.1.get_or_insert(value);
                }
            }
            _ => {}
        }
    }

    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for (aliases, hostname) in blocks {
        for alias in aliases {
            if alias.contains(['*', '?', '!']) || !seen.insert(alias.clone()) {
                continue;
            }
            let hostname = hostname.clone().unwrap_or_else(|| alias.clone());
            entries.push(HostEntry { alias, hostname });
        }
    }
    entries.sort_by_key(|e| e.alias.to_lowercase());
    entries
}

fn collect_lines(
    path: &Path,
    base: &Path,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<(String, String)>,
) {
    let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !real.is_file() || !seen.insert(real.clone()) {
        return;
    }
    let Ok(bytes) = fs::read(&real) else { return };
    for raw in String::from_utf8_lossy(&bytes).lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (keyword, value) = split_keyword(line);
        if keyword == "include" {
            for token in value.split_whitespace() {
                for file in expand_include(token, base) {
                    collect_lines(&file, base, seen, out);
                }
            }
            continue;
        }
        out.push((keyword, value));
    }
}

/// Split `Keyword value`, `Keyword=value` or `Keyword = value`.
fn split_keyword(line: &str) -> (String, String) {
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let keyword = line[..end].to_lowercase();
    let rest = line[end..].trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    (keyword, rest.trim().to_string())
}

fn expand_include(token: &str, base: &Path) -> Vec<PathBuf> {
    let expanded = match token
        .strip_prefix("~/")
        .or_else(|| token.strip_prefix("~\\"))
    {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest)).unwrap_or_default(),
        None => PathBuf::from(token),
    };
    let path = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| wildcard_match(name, n))
        })
        .map(|e| e.path())
        .collect();
    files.sort();
    files
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match p.split_first() {
            None => t.is_empty(),
            Some(('*', rest)) => (0..=t.len()).any(|i| go(rest, &t[i..])),
            Some(('?', rest)) => !t.is_empty() && go(rest, &t[1..]),
            Some((c, rest)) => t.first() == Some(c) && go(rest, &t[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&p, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hosts_includes_and_skips_wildcards() {
        let dir = std::env::temp_dir().join(format!("ssh2socks-cfg-{}", crate::models::new_id()));
        fs::create_dir_all(dir.join("config.d")).unwrap();
        fs::write(
            dir.join("config"),
            "Include config.d/*.conf\n\
             Host *\n  ServerAliveInterval 30\n\
             Host web web-alias\n  HostName=10.0.0.5\n\
             Host bastion !skip\n  User root\n\
             Match host foo\n  HostName ignored\n",
        )
        .unwrap();
        fs::write(
            dir.join("config.d/a.conf"),
            "Host Alpha\n    Hostname alpha.example.com\n",
        )
        .unwrap();
        fs::write(dir.join("config.d/b.txt"), "Host notincluded\n").unwrap();

        let hosts = load_hosts_from(&dir.join("config"), &dir);
        let got: Vec<(&str, &str)> = hosts
            .iter()
            .map(|h| (h.alias.as_str(), h.hostname.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Alpha", "alpha.example.com"),
                ("bastion", "bastion"),
                ("web", "10.0.0.5"),
                ("web-alias", "10.0.0.5"),
            ]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn wildcard() {
        assert!(wildcard_match("*.conf", "a.conf"));
        assert!(wildcard_match("h?st", "host"));
        assert!(!wildcard_match("*.conf", "a.txt"));
    }
}
