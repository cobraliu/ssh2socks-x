//! Creating new key pairs and importing existing ones into `~/.ssh`.
//!
//! Imported keys are checked before anything is written: the private key
//! must parse and decrypt, match the public key (when one is given), sign a
//! random message that the public key verifies, and for RSA also decrypt
//! what the public key encrypted.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use rsa::pkcs1::DecodeRsaPrivateKey as _;
use rsa::pkcs8::DecodePrivateKey as _;
use rsa::traits::PublicKeyParts as _;
use serde::{Deserialize, Serialize};
use ssh_key::private::{Ed25519Keypair, KeypairData, RsaKeypair};
use ssh_key::public::KeyData;
use ssh_key::rand_core::{OsRng, RngCore as _};
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey, PublicKey, Signature, SshSig};

use crate::keys::{self, KeyInfo};

const NAMESPACE: &str = "ssh2socks-selftest";
const RESERVED: &[&str] = &[
    "config",
    "known_hosts",
    "known_hosts.old",
    "authorized_keys",
    "environment",
];

#[derive(Debug, Deserialize)]
pub struct GenerateInput {
    pub name: String,
    /// `ed25519` or `rsa`.
    pub kind: String,
    #[serde(default)]
    pub bits: Option<usize>,
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub passphrase: String,
}

#[derive(Debug, Deserialize)]
pub struct ImportInput {
    pub name: String,
    pub private_key: String,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub passphrase: String,
}

#[derive(Debug, Serialize)]
pub struct Step {
    pub title: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct ImportReport {
    pub ok: bool,
    pub steps: Vec<Step>,
    /// Summary of the key, e.g. `RSA 3072 · SHA256:…`.
    pub summary: String,
}

/// `user@host`, what `ssh-keygen` uses as the default comment.
pub fn default_comment() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    match hostname() {
        Some(host) if !user.is_empty() => format!("{user}@{host}"),
        _ => user,
    }
}

#[cfg(unix)]
fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its whole length.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]);
    let short = name.split('.').next().unwrap_or("").to_string();
    (!short.is_empty()).then_some(short)
}

#[cfg(not(unix))]
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").ok().filter(|h| !h.is_empty())
}

/// First free `base`, `base_2`, `base_3`… in `dir`.
pub fn suggest_name(dir: &Path, base: &str) -> String {
    (1..)
        .map(|i| {
            if i == 1 {
                base.to_string()
            } else {
                format!("{base}_{i}")
            }
        })
        .find(|n| !dir.join(n).exists() && !dir.join(format!("{n}.pub")).exists())
        .unwrap_or_else(|| base.to_string())
}

fn check_name(dir: &Path, name: &str) -> Result<(PathBuf, PathBuf), String> {
    if name.is_empty() {
        return Err(tr!("请填写文件名。", "Enter a file name."));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        || name.starts_with('.')
    {
        return Err(tr!(
            "文件名只能包含字母、数字、点、下划线和减号。",
            "File names may only contain letters, digits, dots, underscores and hyphens."
        ));
    }
    if name.ends_with(".pub") || RESERVED.contains(&name) {
        return Err(tr!(
            "不能使用「{name}」作为密钥文件名。",
            "\"{name}\" can't be used as a key file name."
        ));
    }
    let private = dir.join(name);
    let public = dir.join(format!("{name}.pub"));
    for p in [&private, &public] {
        if p.exists() {
            let file = p.file_name().unwrap_or_default().to_string_lossy();
            return Err(tr!(
                "~/.ssh/{file} 已存在，请换一个文件名。",
                "~/.ssh/{file} already exists. Choose another file name."
            ));
        }
    }
    Ok((private, public))
}

pub fn generate(dir: &Path, input: &GenerateInput) -> Result<KeyInfo, String> {
    let name = input.name.trim();
    let (private_path, public_path) = check_name(dir, name)?;
    let data = match input.kind.as_str() {
        "ed25519" => KeypairData::from(Ed25519Keypair::random(&mut OsRng)),
        "rsa" => {
            let bits = input.bits.unwrap_or(3072);
            if !matches!(bits, 2048 | 3072 | 4096) {
                return Err(tr!(
                    "RSA 长度只支持 2048、3072、4096。",
                    "RSA key size must be 2048, 3072 or 4096."
                ));
            }
            KeypairData::from(
                RsaKeypair::random(&mut OsRng, bits)
                    .map_err(|e| tr!("生成失败：{e}", "Generation failed: {e}"))?,
            )
        }
        other => {
            return Err(tr!(
                "不支持的密钥类型：{other}",
                "Unsupported key type: {other}"
            ))
        }
    };
    let key = PrivateKey::new(data, input.comment.trim())
        .map_err(|e| tr!("生成失败：{e}", "Generation failed: {e}"))?;
    let public = key.public_key().clone();
    // Same check an imported key gets, so a broken RNG or library can't
    // hand out a key that doesn't work.
    sign_and_verify(&key, &public)?;
    encrypt_round_trip(&key, &public).transpose()?;
    let stored = if input.passphrase.is_empty() {
        key
    } else {
        key.encrypt(&mut OsRng, &input.passphrase).map_err(|e| {
            tr!(
                "加密私钥失败：{e}",
                "Could not encrypt the private key: {e}"
            )
        })?
    };
    let pem = stored
        .to_openssh(LineEnding::LF)
        .map_err(|e| tr!("编码私钥失败：{e}", "Could not encode the private key: {e}"))?;
    save_pair(dir, &private_path, &public_path, pem.as_bytes(), &public)?;
    find_key(dir, name)
}

/// Runs every check on a key pair without writing anything.
pub fn check_import(dir: &Path, input: &ImportInput) -> ImportReport {
    let mut steps = Vec::new();
    let summary = checked_import(dir, input, &mut steps)
        .map(|(_, _, summary)| summary)
        .unwrap_or_default();
    ImportReport {
        ok: steps.iter().all(|s| s.ok),
        steps,
        summary,
    }
}

pub fn import(dir: &Path, input: &ImportInput) -> Result<KeyInfo, String> {
    let mut steps = Vec::new();
    let Ok((paths, public, _)) = checked_import(dir, input, &mut steps) else {
        let failed = steps.iter().find(|s| !s.ok);
        return Err(failed.map_or(tr!("校验失败", "Checks failed"), |s| {
            format!("{}: {}", s.title, s.detail)
        }));
    };
    let mut text = input.private_key.replace("\r\n", "\n").trim().to_string();
    text.push('\n');
    save_pair(dir, &paths.0, &paths.1, text.as_bytes(), &public)?;
    find_key(dir, input.name.trim())
}

/// Key checks, then a last step making sure neither the file name nor the
/// key itself already exists in `dir`.
fn checked_import(
    dir: &Path,
    input: &ImportInput,
    steps: &mut Vec<Step>,
) -> Result<((PathBuf, PathBuf), PublicKey, String), ()> {
    let (public, summary) = run_checks(input, steps)?;
    let paths = check_name(dir, input.name.trim()).and_then(|paths| match same_key(dir, &public) {
        Some(existing) => Err(tr!(
            "这把密钥已经存在，文件名是「{existing}」。",
            "This key already exists as \"{existing}\"."
        )),
        None => Ok(paths),
    });
    let ok = paths.is_ok();
    steps.push(Step {
        title: tr!("重复检查", "Duplicate check"),
        ok,
        detail: match &paths {
            Ok(_) => tr!(
                "将保存为 ~/.ssh/{0} 和 {0}.pub",
                "Will be saved as ~/.ssh/{0} and {0}.pub",
                input.name.trim()
            ),
            Err(e) => e.clone(),
        },
    });
    Ok((paths.map_err(|_| ())?, public, summary))
}

/// Private key as parsed from the upload, plus how it was stored.
struct Parsed {
    key: PrivateKey,
    format: &'static str,
}

fn parse_private(text: &str) -> Result<Parsed, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(tr!("请选择或粘贴私钥。", "Choose or paste a private key."));
    }
    if text.starts_with("PuTTY-User-Key-File") {
        return Err(tr!("这是 PuTTY 的 .ppk 格式，请先在 PuTTYgen 里「Conversions → Export OpenSSH key」导出后再导入。", "This is a PuTTY .ppk file. Export it in PuTTYgen with \"Conversions → Export OpenSSH key\" first, then import that."));
    }
    if text.starts_with("ssh-") || text.starts_with("ecdsa-") {
        return Err(tr!(
            "这是公钥，请在「私钥」处选择不带 .pub 的那个文件。",
            "This is a public key. For the private key, choose the file without .pub."
        ));
    }
    if text.contains("BEGIN OPENSSH PRIVATE KEY") {
        return PrivateKey::from_openssh(text)
            .map(|key| Parsed {
                key,
                format: "OpenSSH",
            })
            .map_err(|e| {
                tr!(
                    "私钥内容损坏或不完整（{e}）",
                    "The private key is damaged or incomplete ({e})"
                )
            });
    }
    if text.contains("ENCRYPTED") {
        return Err(tr!("暂不支持加密的 PEM 私钥，请先运行 ssh-keygen -p -f <文件> 转为 OpenSSH 格式后再导入。", "Encrypted PEM private keys are not supported. Run ssh-keygen -p -f <file> to convert it to OpenSSH format first."));
    }
    let rsa = if text.contains("BEGIN RSA PRIVATE KEY") {
        rsa::RsaPrivateKey::from_pkcs1_pem(text)
            .ok()
            .map(|k| (k, "PEM (PKCS#1)"))
    } else if text.contains("BEGIN PRIVATE KEY") {
        rsa::RsaPrivateKey::from_pkcs8_pem(text)
            .ok()
            .map(|k| (k, "PEM (PKCS#8)"))
    } else {
        return Err(tr!("无法识别的私钥格式，需要 OpenSSH（BEGIN OPENSSH PRIVATE KEY）或 RSA PEM 格式。", "Unrecognized private key format. OpenSSH (BEGIN OPENSSH PRIVATE KEY) or RSA PEM is required."));
    };
    let (rsa, format) = rsa.ok_or_else(|| {
        tr!(
            "私钥内容损坏，或不是 RSA 私钥。",
            "The private key is damaged, or is not an RSA key."
        )
    })?;
    let pair = RsaKeypair::try_from(rsa).map_err(|e| {
        tr!(
            "无法读取 RSA 私钥：{e}",
            "Could not read the RSA private key: {e}"
        )
    })?;
    let key = PrivateKey::new(KeypairData::from(pair), "").map_err(|e| e.to_string())?;
    Ok(Parsed { key, format })
}

fn describe(data: &KeyData) -> String {
    match data {
        KeyData::Rsa(pk) => match rsa::RsaPublicKey::try_from(pk) {
            Ok(k) => format!("RSA {}", k.size() * 8),
            Err(_) => "RSA".into(),
        },
        KeyData::Ed25519(_) => "Ed25519".into(),
        other => other.algorithm().to_string(),
    }
}

/// Each check pushes one step; stops at the first failure.
fn run_checks(input: &ImportInput, steps: &mut Vec<Step>) -> Result<(PublicKey, String), ()> {
    let mut step = |title: &str, r: Result<String, String>| -> Result<String, ()> {
        let ok = r.is_ok();
        let detail = match r {
            Ok(d) | Err(d) => d,
        };
        steps.push(Step {
            title: title.into(),
            ok,
            detail: detail.clone(),
        });
        if ok {
            Ok(detail)
        } else {
            Err(())
        }
    };

    let mut parsed = None;
    step(
        &tr!("读取私钥", "Read private key"),
        parse_private(&input.private_key).map(|p| {
            let d = tr!(
                "{} 格式，{}",
                "{} format, {}",
                p.format,
                describe(p.key.public_key().key_data())
            );
            parsed = Some(p);
            d
        }),
    )?;
    let Parsed { key, .. } = parsed.ok_or(())?;

    let mut decrypted = None;
    step(
        &tr!("解密私钥", "Decrypt private key"),
        if key.is_encrypted() {
            if input.passphrase.is_empty() {
                Err(tr!(
                    "私钥已加密，请填写口令。",
                    "The private key is encrypted. Enter its passphrase."
                ))
            } else {
                key.decrypt(&input.passphrase)
                    .map(|k| {
                        decrypted = Some(k);
                        tr!("口令正确，已解密", "Passphrase correct, decrypted")
                    })
                    .map_err(|_| {
                        tr!(
                            "口令错误，无法解密私钥。",
                            "Wrong passphrase; the private key could not be decrypted."
                        )
                    })
            }
        } else {
            decrypted = Some(key.clone());
            Ok(tr!(
                "私钥未加密（无口令）",
                "The private key is not encrypted (no passphrase)"
            ))
        },
    )?;
    let key = decrypted.ok_or(())?;

    let derived = key.public_key().clone();
    let mut public = derived.clone();
    step(
        &tr!("公私钥配对", "Key pair match"),
        match input
            .public_key
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
        {
            None => Ok(tr!("未提供公钥，将从私钥导出", "No public key given; it will be derived from the private key")),
            Some(line) => match PublicKey::from_openssh(line) {
                Err(_) => Err(tr!("公钥格式不正确，应为 ssh-ed25519 / ssh-rsa 开头的一行。", "Invalid public key. It should be one line starting with ssh-ed25519 / ssh-rsa.")),
                Ok(p) if p.key_data() != derived.key_data() => {
                    Err(tr!("公钥和私钥不是一对（指纹不一致）。", "The public and private keys are not a pair (fingerprints differ)."))
                }
                Ok(p) => {
                    public = p;
                    Ok(tr!("公钥与私钥匹配", "Public key matches the private key"))
                }
            },
        },
    )?;
    if public.comment().is_empty() && !key.comment().is_empty() {
        public.set_comment(key.comment());
    }

    step(
        &tr!("签名验证", "Sign and verify"),
        sign_and_verify(&key, &public),
    )?;
    if let Some(result) = encrypt_round_trip(&key, &public) {
        step(&tr!("加密解密", "Encrypt and decrypt"), result)?;
    }

    let summary = format!(
        "{} · {}",
        describe(public.key_data()),
        public.fingerprint(HashAlg::Sha256)
    );
    Ok((public, summary))
}

/// Signs a random message with the private key and verifies it with the
/// public key; a tampered copy of the message must fail verification.
fn sign_and_verify(key: &PrivateKey, public: &PublicKey) -> Result<String, String> {
    let mut msg = [0u8; 32];
    OsRng.fill_bytes(&mut msg);
    let sig = match key.key_data() {
        // ssh-key 0.6 rebuilds the RSA key with `p` twice and can't sign,
        // so RSA signatures are made here from our own reconstruction.
        KeypairData::Rsa(pair) => {
            use rsa::signature::{SignatureEncoding as _, Signer as _};
            let signing = rsa::pkcs1v15::SigningKey::<sha2::Sha512>::new(rsa_private(pair)?);
            let data = SshSig::signed_data(NAMESPACE, HashAlg::Sha512, &msg)
                .map_err(|e| tr!("私钥无法签名：{e}", "The private key can't sign: {e}"))?;
            let raw = signing
                .try_sign(&data)
                .map_err(|e| tr!("私钥无法签名：{e}", "The private key can't sign: {e}"))?;
            let alg = Algorithm::Rsa {
                hash: Some(HashAlg::Sha512),
            };
            Signature::new(alg, raw.to_vec())
                .and_then(|sig| {
                    SshSig::new(
                        key.public_key().key_data().clone(),
                        NAMESPACE,
                        HashAlg::Sha512,
                        sig,
                    )
                })
                .map_err(|e| tr!("私钥无法签名：{e}", "The private key can't sign: {e}"))?
        }
        _ => key
            .sign(NAMESPACE, HashAlg::Sha512, &msg)
            .map_err(|e| tr!("私钥无法签名：{e}", "The private key can't sign: {e}"))?,
    };
    public.verify(NAMESPACE, &msg, &sig).map_err(|_| {
        tr!(
            "公钥无法验证私钥的签名。",
            "The public key can't verify the private key's signature."
        )
    })?;
    msg[0] ^= 1;
    if public.verify(NAMESPACE, &msg, &sig).is_ok() {
        return Err(tr!(
            "篡改后的消息仍能通过验证，密钥异常。",
            "A tampered message still verified; the key is faulty."
        ));
    }
    Ok(tr!(
        "私钥签名、公钥验签通过",
        "Signed with the private key, verified with the public key"
    ))
}

fn rsa_private(pair: &RsaKeypair) -> Result<rsa::RsaPrivateKey, String> {
    let int = |m: &ssh_key::Mpint| {
        m.as_positive_bytes()
            .map(rsa::BigUint::from_bytes_be)
            .ok_or_else(|| tr!("RSA 私钥参数无效。", "Invalid RSA private key parameters."))
    };
    let key = rsa::RsaPrivateKey::from_components(
        int(&pair.public.n)?,
        int(&pair.public.e)?,
        int(&pair.private.d)?,
        vec![int(&pair.private.p)?, int(&pair.private.q)?],
    )
    .map_err(|e| {
        tr!(
            "RSA 私钥参数无效：{e}",
            "Invalid RSA private key parameters: {e}"
        )
    })?;
    key.validate().map_err(|e| {
        tr!(
            "RSA 私钥参数无效：{e}",
            "Invalid RSA private key parameters: {e}"
        )
    })?;
    Ok(key)
}

/// Encrypts random bytes with the public key and decrypts them with the
/// private key. `None` for key types without an encryption test.
fn encrypt_round_trip(key: &PrivateKey, public: &PublicKey) -> Option<Result<String, String>> {
    let mut plain = [0u8; 32];
    OsRng.fill_bytes(&mut plain);
    let back = match (key.key_data(), public.key_data()) {
        (KeypairData::Rsa(pair), KeyData::Rsa(pk)) => rsa_round_trip(pair, pk, &plain),
        (KeypairData::Ed25519(pair), KeyData::Ed25519(pk)) => x25519_round_trip(pair, pk, &plain),
        _ => return None,
    };
    Some(back.and_then(|back| {
        if back == plain {
            Ok(tr!(
                "公钥加密、私钥解密还原一致",
                "Encrypted with the public key, decrypted with the private key, contents match"
            ))
        } else {
            Err(tr!(
                "私钥解密结果与原文不一致。",
                "The decrypted data does not match the original."
            ))
        }
    }))
}

fn rsa_round_trip(
    pair: &RsaKeypair,
    public: &ssh_key::public::RsaPublicKey,
    plain: &[u8],
) -> Result<Vec<u8>, String> {
    let private = rsa_private(pair)?;
    let public = rsa::RsaPublicKey::try_from(public).map_err(|e| {
        tr!(
            "无法读取 RSA 公钥：{e}",
            "Could not read the RSA public key: {e}"
        )
    })?;
    let cipher = public
        .encrypt(&mut OsRng, rsa::Oaep::new::<sha2::Sha256>(), plain)
        .map_err(|e| tr!("公钥加密失败：{e}", "Public key encryption failed: {e}"))?;
    private
        .decrypt(rsa::Oaep::new::<sha2::Sha256>(), &cipher)
        .map_err(|e| tr!("私钥解密失败：{e}", "Private key decryption failed: {e}"))
}

/// Ed25519 can only sign, so the key is mapped to X25519 (as `age` does for
/// ssh-ed25519 recipients): the public key encrypts to an ephemeral key
/// exchange, and only the matching private key derives the same secret.
fn x25519_round_trip(
    pair: &Ed25519Keypair,
    public: &ssh_key::public::Ed25519PublicKey,
    plain: &[u8; 32],
) -> Result<Vec<u8>, String> {
    use curve25519_dalek::MontgomeryPoint;
    use sha2::{Digest as _, Sha256};

    let keystream = |shared: MontgomeryPoint, ephemeral: MontgomeryPoint| {
        Sha256::new()
            .chain_update(shared.as_bytes())
            .chain_update(ephemeral.as_bytes())
            .chain_update(public.0)
            .finalize()
    };
    let xor = |data: &[u8], stream: &[u8]| -> Vec<u8> {
        data.iter().zip(stream).map(|(a, b)| a ^ b).collect()
    };

    // Encrypt with the public key only.
    let recipient = ed25519_dalek::VerifyingKey::from_bytes(&public.0)
        .map_err(|_| tr!("Ed25519 公钥无效。", "Invalid Ed25519 public key."))?
        .to_montgomery();
    let mut ephemeral_secret = [0u8; 32];
    OsRng.fill_bytes(&mut ephemeral_secret);
    let ephemeral = MontgomeryPoint::mul_base_clamped(ephemeral_secret);
    let cipher = xor(
        plain,
        &keystream(recipient.mul_clamped(ephemeral_secret), ephemeral),
    );

    // Decrypt with the private key.
    let scalar = ed25519_dalek::SigningKey::from_bytes(&pair.private.to_bytes()).to_scalar_bytes();
    Ok(xor(
        &cipher,
        &keystream(ephemeral.mul_clamped(scalar), ephemeral),
    ))
}

fn save_pair(
    dir: &Path,
    private_path: &Path,
    public_path: &Path,
    private: &[u8],
    public: &PublicKey,
) -> Result<(), String> {
    if !dir.exists() {
        fs::create_dir_all(dir).map_err(|e| {
            tr!(
                "无法创建 {}：{e}",
                "Could not create {}: {e}",
                dir.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
    }
    let mut line = public
        .to_openssh()
        .map_err(|e| tr!("编码公钥失败：{e}", "Could not encode the public key: {e}"))?;
    line.push('\n');
    write_new(private_path, private, 0o600)?;
    if let Err(e) = write_new(public_path, line.as_bytes(), 0o644) {
        let _ = fs::remove_file(private_path);
        return Err(e);
    }
    Ok(())
}

/// Creates `path`, refusing to replace an existing file.
fn write_new(path: &Path, data: &[u8], mode: u32) -> Result<(), String> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = opts.open(path).map_err(|e| {
        tr!(
            "无法写入 {}：{e}",
            "Could not write {}: {e}",
            path.display()
        )
    })?;
    file.write_all(data)
        .and_then(|()| file.sync_all())
        .map_err(|e| {
            tr!(
                "无法写入 {}：{e}",
                "Could not write {}: {e}",
                path.display()
            )
        })
}

/// Name of a key in `dir` with the same public key, if any.
fn same_key(dir: &Path, public: &PublicKey) -> Option<String> {
    let fingerprint = public.fingerprint(HashAlg::Sha256).to_string();
    keys::list_keys_in(dir)
        .into_iter()
        .find(|k| k.fingerprint == fingerprint)
        .map(|k| k.name)
}

fn find_key(dir: &Path, name: &str) -> Result<KeyInfo, String> {
    keys::list_keys_in(dir)
        .into_iter()
        .find(|k| k.name == name)
        .ok_or_else(|| {
            tr!(
                "密钥已保存，但读取失败。",
                "The key was saved but could not be read back."
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ssh2socks-keygen-{}", crate::models::new_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn gen(name: &str, kind: &str, passphrase: &str) -> GenerateInput {
        GenerateInput {
            name: name.into(),
            kind: kind.into(),
            bits: Some(2048),
            comment: "me@test".into(),
            passphrase: passphrase.into(),
        }
    }

    fn import_input(name: &str, private: &str, public: &str, passphrase: &str) -> ImportInput {
        ImportInput {
            name: name.into(),
            private_key: private.into(),
            public_key: public.into(),
            passphrase: passphrase.into(),
        }
    }

    #[test]
    fn generates_ed25519_and_rsa() {
        let dir = tmp();
        let ed = generate(&dir, &gen("id_ed25519", "ed25519", "")).unwrap();
        assert_eq!(
            (ed.kind.as_str(), ed.comment.as_str()),
            ("ssh-ed25519", "me@test")
        );
        assert_eq!(ed.identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
        let rsa = generate(&dir, &gen("id_rsa", "rsa", "secret")).unwrap();
        assert_eq!(rsa.kind, "ssh-rsa");
        let pem = fs::read_to_string(dir.join("id_rsa")).unwrap();
        assert!(PrivateKey::from_openssh(&pem).unwrap().is_encrypted());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(dir.join("id_rsa"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Existing files are never replaced.
        assert!(generate(&dir, &gen("id_rsa", "ed25519", ""))
            .unwrap_err()
            .contains("已存在"));
        assert!(generate(&dir, &gen("config", "ed25519", "")).is_err());
        assert!(generate(&dir, &gen("../x", "ed25519", "")).is_err());
        assert_eq!(suggest_name(&dir, "id_ed25519"), "id_ed25519_2");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn import_checks_pair_passphrase_and_saves() {
        let src = tmp();
        generate(&src, &gen("k", "rsa", "pw")).unwrap();
        generate(&src, &gen("other", "ed25519", "")).unwrap();
        let private = fs::read_to_string(src.join("k")).unwrap();
        let public = fs::read_to_string(src.join("k.pub")).unwrap();
        let other_pub = fs::read_to_string(src.join("other.pub")).unwrap();

        let dst0 = tmp();
        let report = check_import(&dst0, &import_input("x", &private, &public, "pw"));
        assert!(report.ok, "{report:?}");
        let titles: Vec<&str> = report.steps.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "读取私钥",
                "解密私钥",
                "公私钥配对",
                "签名验证",
                "加密解密",
                "重复检查"
            ]
        );
        assert!(report.summary.starts_with("RSA 2048 · SHA256:"));

        let wrong_pw = check_import(&dst0, &import_input("x", &private, &public, "nope"));
        assert!(!wrong_pw.ok && wrong_pw.steps.last().unwrap().detail.contains("口令错误"));
        let mismatch = check_import(&dst0, &import_input("x", &private, &other_pub, "pw"));
        assert!(!mismatch.ok && mismatch.steps.last().unwrap().detail.contains("不是一对"));
        assert!(!check_import(&dst0, &import_input("x", &public, "", "")).ok);
        // Same key under another name, or a taken name, is refused.
        let dup = check_import(&src, &import_input("x", &private, "", "pw"));
        assert!(
            !dup.ok && dup.steps.last().unwrap().detail.contains("「k」"),
            "{dup:?}"
        );
        let taken = check_import(&src, &import_input("other", &private, "", "pw"));
        assert!(!taken.ok && taken.steps.last().unwrap().detail.contains("已存在"));

        let dst = tmp();
        assert!(import(&dst, &import_input("x", &private, &other_pub, "pw")).is_err());
        assert!(!dst.join("x").exists());
        let saved = import(&dst, &import_input("x", &private, "", "pw")).unwrap();
        assert_eq!(saved.public_key, public.trim());
        // The private key is stored as uploaded (still encrypted).
        assert_eq!(fs::read_to_string(dst.join("x")).unwrap(), private);
        assert!(import(&dst, &import_input("y", &private, "", "pw"))
            .unwrap_err()
            .contains("「x」"));
        fs::remove_dir_all(src).unwrap();
        fs::remove_dir_all(dst).unwrap();
        fs::remove_dir_all(dst0).unwrap();
    }

    #[test]
    fn imports_legacy_rsa_pem() {
        use rsa::pkcs1::EncodeRsaPrivateKey as _;
        let key = rsa::RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        let pem = key.to_pkcs1_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        let dir = tmp();
        let report = check_import(&dir, &import_input("x", &pem, "", ""));
        assert!(report.ok, "{report:?}");
        assert!(report.steps[0].detail.starts_with("PEM (PKCS#1)"));
        let saved = import(&dir, &import_input("legacy", &pem, "", "")).unwrap();
        assert_eq!(saved.kind, "ssh-rsa");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn encryption_test_fails_for_a_foreign_public_key() {
        let ed =
            |_| PrivateKey::new(KeypairData::from(Ed25519Keypair::random(&mut OsRng)), "").unwrap();
        let (a, b) = (ed(0), ed(1));
        assert!(encrypt_round_trip(&a, a.public_key()).unwrap().is_ok());
        assert!(encrypt_round_trip(&a, b.public_key()).unwrap().is_err());
        let rsa = |_| {
            let pair = RsaKeypair::random(&mut OsRng, 2048).unwrap();
            PrivateKey::new(KeypairData::from(pair), "").unwrap()
        };
        let (c, d) = (rsa(0), rsa(1));
        assert!(encrypt_round_trip(&c, c.public_key()).unwrap().is_ok());
        assert!(encrypt_round_trip(&c, d.public_key()).unwrap().is_err());
        assert!(encrypt_round_trip(&a, c.public_key()).is_none());
    }
}
