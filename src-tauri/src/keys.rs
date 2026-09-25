//! Read-only view of the user's SSH public keys (`~/.ssh/*.pub`).

use std::fs;
use std::path::Path;

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine as _;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KeyInfo {
    /// File name without `.pub`, e.g. `id_ed25519`.
    pub name: String,
    /// `~/.ssh/<name>` when the private key sits next to the public one,
    /// ready to use as an `IdentityFile` value.
    pub identity_file: Option<String>,
    pub kind: String,
    /// `SHA256:…`, the same format `ssh-keygen -l` prints.
    pub fingerprint: String,
    pub comment: String,
    /// The whole public key line, what goes into `authorized_keys`.
    pub public_key: String,
}

pub fn list_keys() -> Vec<KeyInfo> {
    match crate::ssh_config::ssh_dir() {
        Some(dir) => list_keys_in(&dir),
        None => Vec::new(),
    }
}

pub fn list_keys_in(dir: &Path) -> Vec<KeyInfo> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut keys: Vec<KeyInfo> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let file_name = e.file_name().into_string().ok()?;
            let name = file_name.strip_suffix(".pub")?.to_string();
            let text = fs::read_to_string(e.path()).ok()?;
            let mut key = parse_public_key(text.lines().find(|l| !l.trim().is_empty())?)?;
            key.identity_file = dir.join(&name).is_file().then(|| format!("~/.ssh/{name}"));
            key.name = name;
            Some(key)
        })
        .collect();
    keys.sort_by_key(|k| k.name.to_lowercase());
    keys
}

fn parse_public_key(line: &str) -> Option<KeyInfo> {
    let line = line.trim();
    let mut parts = line.splitn(3, char::is_whitespace);
    let kind = parts.next()?.to_string();
    let blob = STANDARD.decode(parts.next()?).ok()?;
    let comment = parts.next().unwrap_or("").trim().to_string();
    Some(KeyInfo {
        name: String::new(),
        identity_file: None,
        kind,
        fingerprint: format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&blob))),
        comment,
        public_key: line.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Key and fingerprint produced by `ssh-keygen -t ed25519` / `ssh-keygen -lf`.
    const PUB: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOnEdk8yg/848TtCuX/KaPSMy7jtL7wG7Ha1qJE5bl9N cobraliu@ssh2socks-x";

    #[test]
    fn lists_keys_with_fingerprints() {
        let dir = std::env::temp_dir().join(format!("ssh2socks-keys-{}", crate::models::new_id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("work.pub"), format!("{PUB}\n")).unwrap();
        fs::write(dir.join("work"), "private").unwrap();
        fs::write(dir.join("orphan.pub"), format!("{PUB}\n")).unwrap();
        fs::write(dir.join("broken.pub"), "not a key").unwrap();
        fs::write(dir.join("config"), "Host x").unwrap();

        let keys = list_keys_in(&dir);
        let names: Vec<&str> = keys.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(names, ["orphan", "work"]);
        let work = &keys[1];
        assert_eq!(work.kind, "ssh-ed25519");
        assert_eq!(work.comment, "cobraliu@ssh2socks-x");
        assert_eq!(work.identity_file.as_deref(), Some("~/.ssh/work"));
        assert_eq!(keys[0].identity_file, None);
        assert_eq!(work.public_key, PUB);
        assert_eq!(work.fingerprint, FINGERPRINT);
        fs::remove_dir_all(dir).unwrap();
    }

    const FINGERPRINT: &str = "SHA256:JLUN9fwuse2QiSqFg+D8EZuRbl04QdgoVwG21+dJd+U";
}
