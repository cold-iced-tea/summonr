//! Epitaphs (version 1): a signed link that chains, and a body of salted
//! fingerprints. Pure: no database, no core. Witnesses only ever see links.

use crate::keys::{Keypair, PublicKey};
use crate::util::{canonical_json, sha256_hex};
use base64::Engine;
use serde_json::{json, Value};

/// Signed link bytes are this prefix followed by the link's bytes.
pub const LINK_DOMAIN: &[u8] = b"summonr-epitaph-v1\0";
/// A witness countersigns this prefix followed by canonical `{hash, seq, time, ward}`.
pub const WITNESS_DOMAIN: &[u8] = b"summonr-witness-v1\0";
const FP_DOMAIN: &[u8] = b"summonr-fp-v1\0";
const EVENTS_DOMAIN: &[u8] = b"summonr-events-v1\0";
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub fn new_salt() -> [u8; 16] {
    use rand_core::RngCore;
    let mut s = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut s);
    s
}

/// A salted fingerprint of bytes exactly as stored.
pub fn fingerprint(salt: &[u8; 16], bytes: &[u8]) -> String {
    let mut m = Vec::with_capacity(FP_DOMAIN.len() + 16 + bytes.len());
    m.extend_from_slice(FP_DOMAIN);
    m.extend_from_slice(salt);
    m.extend_from_slice(bytes);
    sha256_hex(&m)
}

/// The bytes an event's fingerprint covers: its position, kind and stored data.
pub fn event_bytes(seq: i64, kind: &str, data: &str) -> Vec<u8> {
    format!("{seq}\0{kind}\0{data}").into_bytes()
}

/// One hash over a run's event fingerprints, in order.
pub fn events_root(fps: &[String]) -> String {
    let mut m = EVENTS_DOMAIN.to_vec();
    for fp in fps {
        m.extend_from_slice(fp.as_bytes());
    }
    sha256_hex(&m)
}

/// An epitaph as stored: the link and body bytes, the link's signature, and
/// the epitaph's hash (the link's SHA-256), by which the chain links.
#[derive(Debug, Clone)]
pub struct Sealed {
    pub link: String,
    pub body: String,
    pub sig: String,
    pub hash: String,
}

pub fn hash(link: &str) -> String {
    sha256_hex(link.as_bytes())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct MerkleStep {
    pub side: String,
    pub hash: String,
}

fn merkle_leaf(link_hash: &str) -> String {
    let mut m = b"summonr-merkle-leaf-v1\0".to_vec();
    m.extend_from_slice(link_hash.as_bytes());
    sha256_hex(&m)
}

fn merkle_node(left: &str, right: &str) -> String {
    let mut m = b"summonr-merkle-node-v1\0".to_vec();
    m.extend_from_slice(left.as_bytes());
    m.extend_from_slice(right.as_bytes());
    sha256_hex(&m)
}

pub fn merkle_root(link_hashes: &[String]) -> String {
    if link_hashes.is_empty() {
        return ZEROS.to_string();
    }
    let mut layer: Vec<String> = link_hashes.iter().map(|h| merkle_leaf(h)).collect();
    while layer.len() > 1 {
        let mut next = Vec::with_capacity(layer.len().div_ceil(2));
        for pair in layer.chunks(2) {
            if pair.len() == 2 {
                next.push(merkle_node(&pair[0], &pair[1]));
            } else {
                // Promote an unpaired trailing node rather than duplicating it,
                // so [a, b, c] and [a, b, c, c] never share a root.
                next.push(pair[0].clone());
            }
        }
        layer = next;
    }
    layer.pop().unwrap()
}

pub fn merkle_proof(link_hashes: &[String], mut idx: usize) -> Option<Vec<MerkleStep>> {
    if idx >= link_hashes.len() {
        return None;
    }
    let mut layer: Vec<String> = link_hashes.iter().map(|h| merkle_leaf(h)).collect();
    let mut proof = Vec::new();
    while layer.len() > 1 {
        if idx.is_multiple_of(2) {
            if idx + 1 < layer.len() {
                proof.push(MerkleStep {
                    side: "right".into(),
                    hash: layer[idx + 1].clone(),
                });
            }
        } else {
            proof.push(MerkleStep {
                side: "left".into(),
                hash: layer[idx - 1].clone(),
            });
        }
        let mut next = Vec::with_capacity(layer.len().div_ceil(2));
        for pair in layer.chunks(2) {
            if pair.len() == 2 {
                next.push(merkle_node(&pair[0], &pair[1]));
            } else {
                next.push(pair[0].clone());
            }
        }
        layer = next;
        idx /= 2;
    }
    Some(proof)
}

pub fn verify_merkle_proof(link_hash: &str, root: &str, proof: &[MerkleStep]) -> bool {
    let mut cur = merkle_leaf(link_hash);
    for step in proof {
        cur = match step.side.as_str() {
            "left" => merkle_node(&step.hash, &cur),
            "right" => merkle_node(&cur, &step.hash),
            _ => return false,
        };
    }
    cur == root
}

fn sign(k: &Keypair, domain: &[u8], bytes: &[u8]) -> String {
    B64.encode(k.sign(domain, bytes))
}

pub fn seal(core: &Keypair, seq: i64, prev: &str, body: &Value) -> Sealed {
    let body = canonical_json(body);
    let link = canonical_json(&json!({
        "v": 1, "chain": 1, "seq": seq, "prev": prev, "core": core.public().id(), "body": sha256_hex(body.as_bytes()),
    }));
    let sig = sign(core, LINK_DOMAIN, link.as_bytes());
    Sealed { hash: hash(&link), link, body, sig }
}

/// Verify a link's signature over its raw bytes, then parse it.
pub fn check_link(link: &str, sig: &str, core: &PublicKey) -> Result<Value, String> {
    let sig = B64.decode(sig).map_err(|_| "signature is not base64".to_string())?;
    if !core.verify(LINK_DOMAIN, link.as_bytes(), &sig) {
        return Err("link signature does not verify".into());
    }
    let v: Value = serde_json::from_str(link).map_err(|_| "link is not JSON".to_string())?;
    if v["v"] != 1 || v["chain"] != 1 {
        return Err(format!("unsupported epitaph version v{} chain {}", v["v"], v["chain"]));
    }
    if v["core"] != core.id() {
        return Err("link names a different core".into());
    }
    Ok(v)
}

/// The body must hash to what its signed link says.
pub fn check_body(link: &Value, body: &str) -> Result<Value, String> {
    if link["body"].as_str() != Some(sha256_hex(body.as_bytes()).as_str()) {
        return Err("body does not match its link".into());
    }
    serde_json::from_str(body).map_err(|_| "body is not JSON".to_string())
}

fn witness_bytes(ward: &str, seq: i64, hash: &str, time: i64) -> Vec<u8> {
    canonical_json(&json!({"ward": ward, "seq": seq, "hash": hash, "time": time})).into_bytes()
}

pub fn countersign(witness: &Keypair, ward: &str, seq: i64, hash: &str, time: i64) -> String {
    sign(witness, WITNESS_DOMAIN, &witness_bytes(ward, seq, hash, time))
}

pub fn check_countersign(witness: &PublicKey, ward: &str, seq: i64, hash: &str, time: i64, sig: &str) -> bool {
    B64.decode(sig)
        .map(|s| witness.verify(WITNESS_DOMAIN, &witness_bytes(ward, seq, hash, time), &s))
        .unwrap_or(false)
}

pub fn mark(seq: i64, hash: &str) -> String {
    format!("summonr-mark:v1:{seq}:{hash}")
}

pub fn parse_mark(s: &str) -> Option<(i64, String)> {
    let rest = s.trim().strip_prefix("summonr-mark:v1:")?;
    let (seq, hash) = rest.split_once(':')?;
    let seq: i64 = seq.parse().ok().filter(|n| *n >= 1)?;
    let hash = hash.split_whitespace().next()?;
    (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| (seq, hash.to_lowercase()))
}

pub const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub fn unhex16(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Check a revealed piece of an elegy against a (verified) epitaph body.
pub fn verify_reveal(body: &Value, reveal: &Value) -> Result<(), String> {
    if reveal["run"] != body["run"] {
        return Err("the reveal is for a different run".into());
    }
    let part = reveal["part"].as_str().ok_or("the reveal names no part")?;
    let salt = unhex16(reveal["salt"].as_str().unwrap_or("")).ok_or("the salt is not 16 hex bytes")?;
    let bytes = reveal["bytes"].as_str().ok_or("the reveal carries no bytes")?;
    let fp = fingerprint(&salt, bytes.as_bytes());
    if let Some(n) = part.strip_prefix("event:") {
        let n: i64 = n.parse().map_err(|_| "bad event number")?;
        // The bytes name their own event number, and the fingerprint list must
        // be exactly events 1..=count in order, so no label can be moved.
        if !bytes.starts_with(&format!("{n}\0")) {
            return Err(format!("the revealed bytes are not event {n}"));
        }
        let fps: Vec<(i64, String)> = reveal["fps"]
            .as_array()
            .ok_or("an event reveal carries the run's event fingerprints")?
            .iter()
            .map(|e| (e["seq"].as_i64().unwrap_or(-1), e["fp"].as_str().unwrap_or("").to_string()))
            .collect();
        if fps.iter().enumerate().any(|(i, (s, _))| *s != i as i64 + 1) {
            return Err("the event fingerprints are not events 1..count in order".into());
        }
        if fps.get((n - 1).max(0) as usize).map(|(_, f)| f.as_str()) != Some(fp.as_str()) {
            return Err(format!("event {n} does not match its fingerprint"));
        }
        let list: Vec<String> = fps.into_iter().map(|(_, f)| f).collect();
        let events = &body["fingerprints"]["events"];
        if events["count"].as_u64() != Some(list.len() as u64) || events["root"].as_str() != Some(events_root(&list).as_str()) {
            return Err("the event fingerprints do not match the epitaph's root".into());
        }
        return Ok(());
    }
    if body["fingerprints"][part].as_str() != Some(fp.as_str()) {
        return Err(format!("{part} does not match its fingerprint"));
    }
    Ok(())
}

/// Check an exported chain segment with only the core's public key. Returns
/// the last seq, or the first bad seq with the reason.
pub fn verify_export(export: &Value, key: &PublicKey) -> Result<i64, (i64, String)> {
    let list = export["epitaphs"].as_array().filter(|l| !l.is_empty()).ok_or((0, "no epitaphs".to_string()))?;
    let mut prev: Option<(i64, String)> = None;
    for e in list {
        let shown = e["seq"].as_i64().unwrap_or(0);
        let link_text = e["link"].as_str().unwrap_or("");
        let link = check_link(link_text, e["sig"].as_str().unwrap_or(""), key).map_err(|r| (shown, r))?;
        let seq = link["seq"].as_i64().unwrap_or(0);
        check_body(&link, e["body"].as_str().unwrap_or("")).map_err(|r| (seq, r))?;
        match &prev {
            Some((p, h)) => {
                if seq != p + 1 {
                    return Err((p + 1, format!("epitaph {} is missing or out of order", p + 1)));
                }
                if link["prev"].as_str() != Some(h.as_str()) {
                    return Err((seq, "does not follow the previous epitaph".into()));
                }
            }
            None if seq == 1 && link["prev"] != ZEROS => return Err((1, "the first epitaph must follow nothing".into())),
            None => {}
        }
        prev = Some((seq, hash(link_text)));
    }
    Ok(prev.map(|p| p.0).unwrap_or(0))
}

/// Whether a (verified) export still contains a mark; how many came after.
pub fn verify_mark(export: &Value, mark: &(i64, String)) -> Result<i64, String> {
    let list = export["epitaphs"].as_array().ok_or("no epitaphs")?;
    // Positions come from the signed links, never the file's own labels.
    let signed_seq = |e: &Value| -> Option<i64> {
        serde_json::from_str::<Value>(e["link"].as_str()?).ok()?["seq"].as_i64()
    };
    let last = list.last().and_then(signed_seq).unwrap_or(0);
    let at = list
        .iter()
        .find(|e| signed_seq(e) == Some(mark.0))
        .ok_or_else(|| format!("the export does not reach seq {}", mark.0))?;
    if hash(at["link"].as_str().unwrap_or("")) != mark.1 {
        return Err(format!("the chain no longer contains this mark: seq {} has changed", mark.0));
    }
    Ok(last - mark.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::generate;
    use serde_json::json;

    const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn body() -> Value {
        json!({"v": 1, "run": "s-1", "parent": null, "ritual": {"name": "r", "version": 1, "hash": "h"},
               "caller": "", "started": 1, "ended": 2, "outcome": "returned",
               "fingerprints": {"inputs": "a", "events": {"count": 0, "root": events_root(&[])}}, "children": []})
    }

    #[test]
    fn fingerprint_depends_on_salt_and_bytes() {
        let (a, b) = (new_salt(), new_salt());
        assert_ne!(a, b);
        assert_eq!(fingerprint(&a, b"x"), fingerprint(&a, b"x"));
        assert_ne!(fingerprint(&a, b"x"), fingerprint(&b, b"x"));
        assert_ne!(fingerprint(&a, b"x"), fingerprint(&a, b"y"));
        assert_eq!(fingerprint(&a, b"x").len(), 64);
    }

    #[test]
    fn event_bytes_separate_fields() {
        assert_eq!(event_bytes(3, "op", "{}"), b"3\0op\0{}".to_vec());
        assert_ne!(event_bytes(1, "a", "bc"), event_bytes(1, "ab", "c"));
    }

    #[test]
    fn events_root_is_order_sensitive() {
        let (x, y) = ("1".repeat(64), "2".repeat(64));
        assert_ne!(events_root(&[x.clone(), y.clone()]), events_root(&[y, x]));
        assert_eq!(events_root(&[]).len(), 64);
    }

    #[test]
    fn seal_round_trip() {
        let k = generate();
        let s = seal(&k, 1, ZEROS, &body());
        let link = check_link(&s.link, &s.sig, &k.public()).unwrap();
        assert_eq!(link["seq"], 1);
        assert_eq!(link["prev"], ZEROS);
        assert_eq!(link["core"], k.public().id());
        assert_eq!(check_body(&link, &s.body).unwrap()["run"], "s-1");
        assert_eq!(s.hash, hash(&s.link));
        assert_eq!(s.body, crate::util::canonical_json(&body()));
    }

    #[test]
    fn link_names_nothing() {
        let k = generate();
        let s = seal(&k, 7, ZEROS, &body());
        let link: Value = serde_json::from_str(&s.link).unwrap();
        let mut keys: Vec<&String> = link.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["body", "chain", "core", "prev", "seq", "v"]);
        assert!(!s.link.contains("s-1") && !s.link.contains("returned"));
    }

    #[test]
    fn flipped_link_byte_fails() {
        let k = generate();
        let s = seal(&k, 1, ZEROS, &body());
        let bad = s.link.replacen("\"seq\":1", "\"seq\":2", 1);
        assert!(check_link(&bad, &s.sig, &k.public()).is_err());
        assert!(check_link(&s.link, &s.sig, &generate().public()).is_err(), "only the core's key verifies");
    }

    #[test]
    fn swapped_body_fails() {
        let k = generate();
        let s = seal(&k, 1, ZEROS, &body());
        let link = check_link(&s.link, &s.sig, &k.public()).unwrap();
        let mut other = body();
        other["outcome"] = json!("failed");
        assert!(check_body(&link, &crate::util::canonical_json(&other)).is_err());
    }

    #[test]
    fn countersign_round_trip_and_wrong_key_fails() {
        let w = generate();
        let sig = countersign(&w, "ward-id", 5, "abc", 99);
        assert!(check_countersign(&w.public(), "ward-id", 5, "abc", 99, &sig));
        assert!(!check_countersign(&w.public(), "ward-id", 6, "abc", 99, &sig));
        assert!(!check_countersign(&generate().public(), "ward-id", 5, "abc", 99, &sig));
    }

    #[test]
    fn mark_round_trip() {
        let h = "ab".repeat(32);
        assert_eq!(parse_mark(&mark(42, &h)), Some((42, h)));
    }

    #[test]
    fn parse_mark_rejects_bad() {
        for bad in ["summonr-mark:v2:1:".to_string() + &"a".repeat(64), "summonr-mark:v1:1:xyz".into(), "summonr-mark:v1::abc".into(), "nope".into()] {
            assert_eq!(parse_mark(&bad), None, "{bad}");
        }
    }
}
