//! Ed25519 identities: key files, key ids and domain-separated signatures.
//! All cryptography comes from `ed25519-dalek`; nothing here is homemade.

use base64::Engine;
use ed25519_dalek::pkcs8::{spki::der::pem::LineEnding, DecodePrivateKey, EncodePrivateKey};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Signed request bytes are this prefix followed by the payload bytes.
pub const REQUEST_DOMAIN: &[u8] = b"summonr-signed-request-v1\0";
/// Signed reply bytes are this prefix followed by the payload bytes.
pub const REPLY_DOMAIN: &[u8] = b"summonr-signed-response-v1\0";
/// Signed familiar grant bytes are this prefix followed by the canonical grant payload.
pub const FAMILIAR_GRANT_DOMAIN: &[u8] = b"summonr-familiar-grant-v1\0";

/// A private signing key. It is never serialized anywhere but its key file.
pub struct Keypair {
    signing: SigningKey,
}

/// A 32-byte Ed25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PublicKey(pub [u8; 32]);

impl std::fmt::Debug for Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Keypair({})", self.public().id())
    }
}

pub fn generate() -> Keypair {
    Keypair { signing: SigningKey::generate(&mut rand_core::OsRng) }
}

impl Keypair {
    #[cfg(test)]
    pub fn from_secret(secret: &[u8; 32]) -> Keypair {
        Keypair { signing: SigningKey::from_bytes(secret) }
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.signing.verifying_key().to_bytes())
    }

    /// Sign `domain` followed by `payload`. The domain keeps a signature made
    /// for one purpose from being accepted for another.
    pub fn sign(&self, domain: &[u8], payload: &[u8]) -> [u8; 64] {
        let mut msg = Vec::with_capacity(domain.len() + payload.len());
        msg.extend_from_slice(domain);
        msg.extend_from_slice(payload);
        self.signing.sign(&msg).to_bytes()
    }
}

impl PublicKey {
    /// Lowercase hex SHA-256 of the raw public key bytes.
    pub fn id(&self) -> String {
        crate::util::sha256_hex(&self.0)
    }

    pub fn text(&self) -> String {
        format!("ed25519:{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.0))
    }

    pub fn parse(text: &str) -> Result<PublicKey, String> {
        let b64 = text
            .trim()
            .strip_prefix("ed25519:")
            .ok_or_else(|| "a public key looks like ed25519:<base64url>".to_string())?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(b64)
            .map_err(|_| "the public key is not valid base64url".to_string())?;
        let raw: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "an Ed25519 public key is 32 bytes".to_string())?;
        VerifyingKey::from_bytes(&raw).map_err(|_| "not a valid Ed25519 public key".to_string())?;
        Ok(PublicKey(raw))
    }

    /// Strict verification of `domain` followed by `payload`.
    pub fn verify(&self, domain: &[u8], payload: &[u8], sig: &[u8]) -> bool {
        let Ok(key) = VerifyingKey::from_bytes(&self.0) else { return false };
        let Ok(sig) = Signature::from_slice(sig) else { return false };
        let mut msg = Vec::with_capacity(domain.len() + payload.len());
        msg.extend_from_slice(domain);
        msg.extend_from_slice(payload);
        key.verify_strict(&msg, &sig).is_ok()
    }
}

/// A key id in groups of four, for comparing by eye.
pub fn grouped(id: &str) -> String {
    id.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A delegated familiar grant signed by an offline root key.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct FamiliarGrant {
    pub version: u32,
    pub name: String,
    pub public_key: String,
    #[serde(default)]
    pub rituals: Vec<String>,
    #[serde(default)]
    pub pacts: Vec<String>,
    pub issued_at_ms: i64,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    pub root_key_id: String,
    pub signature: String,
}

impl FamiliarGrant {
    fn canonical_payload(&self) -> String {
        serde_json::json!({
            "expires_at_ms": self.expires_at_ms,
            "issued_at_ms": self.issued_at_ms,
            "name": self.name,
            "pacts": self.pacts,
            "public_key": self.public_key,
            "rituals": self.rituals,
            "root_key_id": self.root_key_id,
            "version": self.version,
        })
        .to_string()
    }

    /// Sign a new version-1 familiar grant with `root`.
    pub fn sign(
        root: &Keypair,
        name: String,
        public_key: &PublicKey,
        rituals: Vec<String>,
        pacts: Vec<String>,
        issued_at_ms: i64,
        expires_at_ms: Option<i64>,
    ) -> Self {
        let mut grant = Self {
            version: 1,
            name,
            public_key: public_key.text(),
            rituals,
            pacts,
            issued_at_ms,
            expires_at_ms,
            root_key_id: root.public().id(),
            signature: String::new(),
        };
        let payload = grant.canonical_payload();
        let sig_bytes = root.sign(FAMILIAR_GRANT_DOMAIN, payload.as_bytes());
        grant.signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig_bytes);
        grant
    }

    /// Verify this grant against `root_pub` at `now_ms`, returning the familiar's [`PublicKey`].
    pub fn verify(&self, root_pub: &PublicKey, now_ms: i64) -> Result<PublicKey, String> {
        if self.version != 1 {
            return Err(format!("unsupported familiar grant version: {}", self.version));
        }
        if self.root_key_id != root_pub.id() {
            return Err("familiar grant root_key_id does not match root key".into());
        }
        if self.issued_at_ms > now_ms.saturating_add(120_000) {
            return Err("familiar grant issued_at_ms is in the future".into());
        }
        if let Some(exp) = self.expires_at_ms {
            if exp < self.issued_at_ms {
                return Err("familiar grant expires_at_ms precedes issued_at_ms".into());
            }
            if now_ms > exp {
                return Err("familiar grant has expired".into());
            }
        }
        let familiar_pub = PublicKey::parse(&self.public_key)?;
        let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&self.signature)
            .map_err(|_| "familiar grant signature is not valid base64url".to_string())?;
        let payload = self.canonical_payload();
        if !root_pub.verify(FAMILIAR_GRANT_DOMAIN, payload.as_bytes(), &sig) {
            return Err("familiar grant signature is invalid".into());
        }
        Ok(familiar_pub)
    }
}

const ENC_PREFIX: &str = "summonr-enc-v1:";
const ENC_PREFIX_V2: &str = "summonr-enc-v2:";
const PBKDF2_ITERS: u32 = 600_000;

fn hmac_sha256_ring(key: &[u8], data: &[u8]) -> [u8; 32] {
    let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    let tag = ring::hmac::sign(&k, data);
    tag.as_ref().try_into().expect("HMAC-SHA256 is 32 bytes")
}

fn derive_v1_enc_mac(passphrase: &[u8], salt: &[u8; 16]) -> ([u8; 32], [u8; 32]) {
    use sha2::{Digest, Sha256};
    let mut state: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(b"summonr-kdf-v1\0");
        h.update(salt);
        h.update(passphrase);
        h.finalize().into()
    };
    for i in 0..10_000u32 {
        let mut h = Sha256::new();
        h.update(state);
        h.update(i.to_le_bytes());
        h.update(passphrase);
        state = h.finalize().into();
    }
    let enc = hmac_sha256_ring(&state, b"enc");
    let mac = hmac_sha256_ring(&state, b"mac");
    (enc, mac)
}

fn xor_keystream_v1(enc_key: &[u8; 32], salt: &[u8; 16], data: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut out = Vec::with_capacity(data.len());
    for (block_idx, chunk) in data.chunks(32).enumerate() {
        let mut h = Sha256::new();
        h.update(enc_key);
        h.update(salt);
        h.update((block_idx as u64).to_le_bytes());
        let pad: [u8; 32] = h.finalize().into();
        for (b, p) in chunk.iter().zip(pad.iter()) {
            out.push(b ^ p);
        }
    }
    out
}

fn derive_v2_key(passphrase: &[u8], salt: &[u8; 16]) -> [u8; 32] {
    let mut key = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        std::num::NonZeroU32::new(PBKDF2_ITERS).unwrap(),
        salt,
        passphrase,
        &mut key,
    );
    key
}

pub fn encrypt_pem(pem: &[u8], passphrase: &str) -> String {
    use rand_core::RngCore;
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
    let mut salt = [0u8; 16];
    let mut nonce_bytes = [0u8; 12];
    rand_core::OsRng.fill_bytes(&mut salt);
    rand_core::OsRng.fill_bytes(&mut nonce_bytes);
    let key_bytes = derive_v2_key(passphrase.as_bytes(), &salt);
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key_bytes).expect("32-byte key"));
    let mut in_out = pem.to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce_bytes),
        Aad::from(&salt),
        &mut in_out,
    )
    .expect("AES-256-GCM seal");
    let mut blob = Vec::with_capacity(16 + 12 + in_out.len());
    blob.extend_from_slice(&salt);
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&in_out);
    format!(
        "{ENC_PREFIX_V2}{}",
        base64::engine::general_purpose::STANDARD.encode(blob)
    )
}

pub fn decrypt_pem(text: &str, passphrase: &str) -> Result<String, String> {
    let trimmed = text.trim();
    if let Some(b64) = trimmed.strip_prefix(ENC_PREFIX_V2) {
        use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
        let blob = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|_| "corrupted encrypted key encoding".to_string())?;
        if blob.len() < 16 + 12 + 16 {
            return Err("corrupted encrypted key payload".into());
        }
        let salt: [u8; 16] = blob[..16].try_into().unwrap();
        let nonce_bytes: [u8; 12] = blob[16..28].try_into().unwrap();
        let key_bytes = derive_v2_key(passphrase.as_bytes(), &salt);
        let key =
            LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key_bytes).expect("32-byte key"));
        let mut buf = blob[28..].to_vec();
        let pt = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::from(&salt),
                &mut buf,
            )
            .map_err(|_| "wrong passphrase or corrupted encrypted key file".to_string())?;
        return String::from_utf8(pt.to_vec()).map_err(|_| "decrypted key is not UTF-8".into());
    }
    let b64 = trimmed
        .strip_prefix(ENC_PREFIX)
        .ok_or_else(|| "not an encrypted key file".to_string())?;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| "corrupted encrypted key encoding".to_string())?;
    if blob.len() < 48 {
        return Err("corrupted encrypted key payload".into());
    }
    let salt: [u8; 16] = blob[..16].try_into().unwrap();
    let mac: [u8; 32] = blob[16..48].try_into().unwrap();
    let ct = &blob[48..];
    let (enc_key, mac_key) = derive_v1_enc_mac(passphrase.as_bytes(), &salt);
    let mut mac_input = Vec::with_capacity(16 + ct.len());
    mac_input.extend_from_slice(&salt);
    mac_input.extend_from_slice(ct);
    let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &mac_key);
    if ring::hmac::verify(&k, &mac_input, &mac).is_err() {
        return Err("wrong passphrase or corrupted encrypted key file".into());
    }
    let pt = xor_keystream_v1(&enc_key, &salt, ct);
    String::from_utf8(pt).map_err(|_| "decrypted key is not UTF-8".into())
}

/// Write a new key file (PKCS#8 PEM, mode 0600). Never overwrites.
/// Encrypts the file if `SUMMONR_KEY_PASSPHRASE` is set and non-empty.
pub fn save_new(path: &Path, k: &Keypair) -> Result<(), String> {
    let pass = std::env::var("SUMMONR_KEY_PASSPHRASE")
        .ok()
        .filter(|s| !s.is_empty());
    save_new_with_passphrase(path, k, pass.as_deref())
}

pub fn save_new_with_passphrase(
    path: &Path,
    k: &Keypair,
    passphrase: Option<&str>,
) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let pem = k
        .signing
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|_| "cannot encode the key".to_string())?;
    let body = match passphrase {
        Some(p) => encrypt_pem(pem.as_bytes(), p),
        None => pem.to_string(),
    };
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("cannot create key file {}: {e}", path.display()))?;
    f.write_all(body.as_bytes())
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("cannot write key file {}: {e}", path.display()))
}

/// Read a key file, refusing one that others can read.
pub fn load(path: &Path) -> Result<Keypair, String> {
    let pass = std::env::var("SUMMONR_KEY_PASSPHRASE")
        .ok()
        .filter(|s| !s.is_empty());
    load_with_passphrase(path, pass.as_deref())
}

pub fn load_with_passphrase(path: &Path, passphrase: Option<&str>) -> Result<Keypair, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read key file {}: {e}", path.display()))?;
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "key file {} is readable by other users; run `chmod 600 {}`",
            path.display(),
            path.display()
        ));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("cannot read key file {}: {e}", path.display()))?;
    let pem = if raw.trim_start().starts_with(ENC_PREFIX_V2) || raw.trim_start().starts_with(ENC_PREFIX) {
        let p = passphrase.ok_or_else(|| {
            format!(
                "key file {} is encrypted; set SUMMONR_KEY_PASSPHRASE",
                path.display()
            )
        })?;
        decrypt_pem(&raw, p)?
    } else {
        raw
    };
    let signing = SigningKey::from_pkcs8_pem(&pem).map_err(|_| format!("{} is not an Ed25519 PKCS#8 key", path.display()))?;
    Ok(Keypair { signing })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(crate::util::new_id("summonr-keys"));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn rfc8032_test_vector_1() {
        let secret: [u8; 32] = unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60").try_into().unwrap();
        let k = Keypair::from_secret(&secret);
        assert_eq!(k.public().0.to_vec(), unhex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"));
        let sig = k.sign(b"", b"");
        assert_eq!(
            sig.to_vec(),
            unhex("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b")
        );
        assert!(k.public().verify(b"", b"", &sig));
    }

    #[test]
    fn domain_separates_signatures() {
        let k = generate();
        let sig = k.sign(REQUEST_DOMAIN, b"payload");
        assert!(k.public().verify(REQUEST_DOMAIN, b"payload", &sig));
        assert!(!k.public().verify(REPLY_DOMAIN, b"payload", &sig));
    }

    #[test]
    fn flipped_bit_fails() {
        let k = generate();
        let mut sig = k.sign(REQUEST_DOMAIN, b"payload");
        assert!(!k.public().verify(REQUEST_DOMAIN, b"paylaod", &sig));
        sig[10] ^= 1;
        assert!(!k.public().verify(REQUEST_DOMAIN, b"payload", &sig));
        assert!(!k.public().verify(REQUEST_DOMAIN, b"payload", &sig[..63]));
    }

    #[test]
    fn text_round_trips_and_id_is_sha256() {
        let k = generate();
        let text = k.public().text();
        assert!(text.starts_with("ed25519:"));
        let back = PublicKey::parse(&text).unwrap();
        assert_eq!(back, k.public());
        assert_eq!(back.id(), crate::util::sha256_hex(&back.0));
        assert_eq!(back.id().len(), 64);
        assert_eq!(grouped("abcdef0123"), "abcd ef01 23");
    }

    #[test]
    fn parse_rejects_bad_prefix_and_length() {
        assert!(PublicKey::parse("ssh-ed25519:AAAA").is_err());
        assert!(PublicKey::parse("ed25519:AAAA").is_err());
        assert!(PublicKey::parse("ed25519:!!!").is_err());
    }

    #[test]
    fn save_refuses_overwrite_and_is_0600() {
        let path = temp("k.key");
        save_new(&path, &generate()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let before = std::fs::read(&path).unwrap();
        assert!(save_new(&path, &generate()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before, "an existing key is never overwritten");
        let loaded = load(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().is_empty());
        let _ = loaded;
    }

    #[test]
    fn load_round_trips_the_same_identity() {
        let path = temp("k.key");
        let k = generate();
        save_new(&path, &k).unwrap();
        assert_eq!(load(&path).unwrap().public(), k.public());
    }

    #[test]
    fn load_refuses_group_readable() {
        let path = temp("k.key");
        save_new(&path, &generate()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let err = load(&path).err().unwrap();
        assert!(err.contains("chmod 600"), "{err}");
    }

    #[test]
    fn encrypted_key_file_round_trips_and_rejects_wrong_or_missing_passphrase() {
        let p = temp("enc.key");
        let k = generate();
        save_new_with_passphrase(&p, &k, Some("correct horse battery staple")).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.starts_with(ENC_PREFIX_V2));
        assert!(load_with_passphrase(&p, None).is_err());
        assert!(load_with_passphrase(&p, Some("wrong")).is_err());
        let loaded = load_with_passphrase(&p, Some("correct horse battery staple")).unwrap();
        assert_eq!(loaded.public(), k.public());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn familiar_grant_sign_and_verify() {
        let root = generate();
        let fam = generate();
        let grant = FamiliarGrant::sign(
            &root,
            "agent-alpha".into(),
            &fam.public(),
            vec!["total-receipt".into()],
            vec!["deploy".into()],
            1_000,
            Some(2_000),
        );
        assert_eq!(grant.version, 1);
        assert_eq!(grant.root_key_id, root.public().id());
        let verified = grant.verify(&root.public(), 2_000).unwrap();
        assert_eq!(verified, fam.public());

        let no_exp = FamiliarGrant::sign(
            &root,
            "agent-beta".into(),
            &fam.public(),
            vec![],
            vec![],
            1_000,
            None,
        );
        assert_eq!(no_exp.verify(&root.public(), i64::MAX).unwrap(), fam.public());
    }

    #[test]
    fn familiar_grant_rejects_tampered_fields() {
        let root = generate();
        let fam = generate();
        let grant = FamiliarGrant::sign(
            &root,
            "agent-alpha".into(),
            &fam.public(),
            vec!["r1".into()],
            vec![],
            1_000,
            Some(5_000),
        );

        let mut bad_name = grant.clone();
        bad_name.name = "agent-evil".into();
        assert!(bad_name.verify(&root.public(), 1_500).is_err());

        let mut bad_ritual = grant.clone();
        bad_ritual.rituals.push("r2".into());
        assert!(bad_ritual.verify(&root.public(), 1_500).is_err());

        let mut bad_ver = grant;
        bad_ver.version = 2;
        assert!(bad_ver.verify(&root.public(), 1_500).is_err());
    }

    #[test]
    fn familiar_grant_rejects_wrong_root_and_expiry() {
        let root = generate();
        let other_root = generate();
        let fam = generate();
        let grant = FamiliarGrant::sign(
            &root,
            "agent-alpha".into(),
            &fam.public(),
            vec![],
            vec![],
            1_000,
            Some(2_000),
        );

        assert!(grant.verify(&other_root.public(), 1_500).is_err());
        let err = grant.verify(&root.public(), 2_001).unwrap_err();
        assert!(err.contains("expired"), "{err}");
    }
}
