//! Signed request and reply envelopes (version 1). Pure: no database, no core.
//! The signature covers the exact payload bytes sent and is checked before
//! the payload is parsed.

use crate::keys::{Keypair, PublicKey, REPLY_DOMAIN, REQUEST_DOMAIN};
use crate::rpc::RpcErr;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

pub const REQUEST_PURPOSE: &str = "summonr/request/v1";
pub const REPLY_PURPOSE: &str = "summonr/response/v1";
/// How far a request's timestamp may differ from the receiver's clock.
pub const WINDOW_MS: i64 = 120_000;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// A verified request.
#[derive(Debug)]
pub struct Opened {
    pub key: String,
    pub method: String,
    pub params: Value,
    pub nonce: String,
}

/// Why a request was refused. Only the receiver's owner sees the reason.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Malformed,
    UnknownKey,
    BadSignature,
    WrongPurpose,
    KeyMismatch,
    WrongAudience,
    ClockSkew { by_ms: i64 },
    Replayed,
}

impl Refusal {
    pub fn reason(&self) -> String {
        match self {
            Refusal::Malformed => "malformed signed envelope".into(),
            Refusal::UnknownKey => "unknown or revoked key".into(),
            Refusal::BadSignature => "signature does not verify".into(),
            Refusal::WrongPurpose => "wrong purpose".into(),
            Refusal::KeyMismatch => "payload familiar does not match the signing key".into(),
            Refusal::WrongAudience => "addressed to a different core".into(),
            Refusal::ClockSkew { by_ms } => format!(
                "clock skew of {} s (window is {} s); check NTP on both devices",
                by_ms / 1000,
                WINDOW_MS / 1000
            ),
            Refusal::Replayed => "nonce already used (replay)".into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    v: u32,
    payload: String,
    key: String,
    sig: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestPayload {
    purpose: String,
    familiar: String,
    audience: String,
    ts: i64,
    nonce: String,
    method: String,
    params: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyPayload {
    purpose: String,
    core: String,
    in_reply_to: String,
    // `Some(Value::Null)` when the result is JSON null; `None` when absent.
    #[serde(default, deserialize_with = "present")]
    result: Option<Value>,
    #[serde(default)]
    error: Option<ReplyError>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyError {
    code: String,
    message: String,
    exit: i32,
    #[serde(default)]
    data: Value,
}

fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

/// 16 bytes from the OS random source, as hex.
pub fn new_nonce() -> String {
    use rand_core::RngCore;
    let mut b = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn wrap(k: &Keypair, domain: &[u8], payload: &[u8]) -> Value {
    json!({"signed": {"v": 1, "payload": B64.encode(payload), "key": k.public().id(), "sig": B64.encode(k.sign(domain, payload))}})
}

pub fn seal_request(k: &Keypair, audience: &str, method: &str, params: &Value, now_ms: i64, nonce: &str) -> Value {
    let payload = json!({
        "purpose": REQUEST_PURPOSE, "familiar": k.public().id(), "audience": audience,
        "ts": now_ms, "nonce": nonce, "method": method, "params": params,
    });
    wrap(k, REQUEST_DOMAIN, &serde_json::to_vec(&payload).expect("JSON values serialize"))
}

fn frame_of(frame: &Value) -> Option<(Frame, Vec<u8>, Vec<u8>)> {
    let f: Frame = serde_json::from_value(frame.get("signed")?.clone()).ok()?;
    if f.v != 1 {
        return None;
    }
    let payload = B64.decode(&f.payload).ok()?;
    let sig = B64.decode(&f.sig).ok()?;
    Some((f, payload, sig))
}

/// The key id a frame claims, read WITHOUT verification, for the owner's log.
pub fn claimed_key(frame: &Value) -> String {
    frame["signed"]["key"].as_str().unwrap_or("").chars().take(64).collect()
}

/// Verify, then parse, then check. `seen(key, nonce, ts)` records the nonce
/// and returns true if it was already used.
pub fn open_request(
    frame: &Value,
    lookup: &dyn Fn(&str) -> Option<PublicKey>,
    audience: &str,
    now_ms: i64,
    seen: &mut dyn FnMut(&str, &str, i64) -> bool,
) -> Result<Opened, Refusal> {
    let (f, payload, sig) = frame_of(frame).ok_or(Refusal::Malformed)?;
    let key = lookup(&f.key).ok_or(Refusal::UnknownKey)?;
    if key.id() != f.key || !key.verify(REQUEST_DOMAIN, &payload, &sig) {
        return Err(Refusal::BadSignature);
    }
    let p: RequestPayload = serde_json::from_slice(&payload).map_err(|_| Refusal::Malformed)?;
    if p.purpose != REQUEST_PURPOSE {
        return Err(Refusal::WrongPurpose);
    }
    if p.familiar != f.key {
        return Err(Refusal::KeyMismatch);
    }
    if p.audience != audience {
        return Err(Refusal::WrongAudience);
    }
    let by_ms = now_ms - p.ts;
    if by_ms.abs() > WINDOW_MS {
        return Err(Refusal::ClockSkew { by_ms });
    }
    if p.nonce.len() != 32 || !p.nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Refusal::Malformed);
    }
    if seen(&f.key, &p.nonce, p.ts) {
        return Err(Refusal::Replayed);
    }
    Ok(Opened { key: f.key, method: p.method, params: p.params, nonce: p.nonce })
}

pub fn seal_reply(core: &Keypair, in_reply_to: &str, outcome: &Result<Value, RpcErr>) -> Value {
    let mut payload = json!({"purpose": REPLY_PURPOSE, "core": core.public().id(), "in_reply_to": in_reply_to});
    match outcome {
        Ok(v) => payload["result"] = v.clone(),
        Err(e) => payload["error"] = json!({"code": e.code, "message": e.message, "exit": e.exit, "data": e.data}),
    }
    wrap(core, REPLY_DOMAIN, &serde_json::to_vec(&payload).expect("JSON values serialize"))
}

/// The core's answer to a frame it could not verify. It names no request:
/// the frame's nonce is unverified, and a copy of a frame that already ran is
/// refused too, so a refusal bound to that nonce would let anyone on the path
/// tell the sender "not authenticated" about a request that ran.
pub fn seal_refusal(core: &Keypair) -> Value {
    let generic = RpcErr::new("not_authenticated", 4, "request not authenticated");
    seal_reply(core, "", &Err(generic))
}

/// What a sender learns from a refusal: some frame was refused, not
/// necessarily this one.
pub const UNBOUND_REFUSAL: &str = "the core refused a request it could not verify (unknown or revoked key, \
clock skew, bad signature or a replayed copy); it cannot say which request, so if this one may have \
run, check its history before retrying";

/// Verify a reply against the pinned core key and the request's nonce. The
/// outer `Err` means the reply itself cannot be trusted.
pub fn open_reply(frame: &Value, core: &PublicKey, expected_nonce: &str) -> Result<Result<Value, RpcErr>, String> {
    let (f, payload, sig) = frame_of(frame).ok_or("malformed reply")?;
    if f.key != core.id() || !core.verify(REPLY_DOMAIN, &payload, &sig) {
        return Err("reply is not signed by the pinned core key".into());
    }
    let p: ReplyPayload = serde_json::from_slice(&payload).map_err(|_| "malformed reply payload")?;
    if p.purpose != REPLY_PURPOSE || p.core != core.id() {
        return Err("reply has the wrong purpose or core".into());
    }
    if p.in_reply_to.is_empty() && p.result.is_none() && p.error.is_some() {
        return Ok(Err(RpcErr::new("not_authenticated", 4, UNBOUND_REFUSAL)));
    }
    if p.in_reply_to != expected_nonce {
        return Err("reply answers a different request".into());
    }
    match (p.result, p.error) {
        (Some(v), None) => Ok(Ok(v)),
        (None, Some(e)) => Ok(Err(RpcErr { code: e.code, message: e.message, exit: e.exit, data: e.data })),
        _ => Err("reply has neither a result nor an error".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{generate, Keypair};
    use serde_json::json;

    const NOW: i64 = 1_794_733_000_000;

    struct World {
        agent: Keypair,
        core: Keypair,
    }

    fn world() -> World {
        World { agent: generate(), core: generate() }
    }

    fn request(w: &World, ts: i64, nonce: &str) -> Value {
        seal_request(&w.agent, &w.core.public().id(), "invoke", &json!({"target":"r","caller":""}), ts, nonce)
    }

    fn open(w: &World, frame: &Value, now: i64, used: &mut Vec<(String, String)>) -> Result<Opened, Refusal> {
        let agent = w.agent.public();
        let lookup = move |id: &str| (id == agent.id()).then_some(agent);
        let mut seen = |k: &str, n: &str, _ts: i64| {
            let hit = used.iter().any(|(a, b)| a == k && b == n);
            if !hit {
                used.push((k.to_string(), n.to_string()));
            }
            hit
        };
        open_request(frame, &lookup, &w.core.public().id(), now, &mut seen)
    }

    /// Re-sign a payload edited as JSON, to probe checks behind the signature.
    fn resign(w: &World, frame: &Value, edit: impl FnOnce(&mut Value)) -> Value {
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut payload: Value = serde_json::from_slice(&b64.decode(frame["signed"]["payload"].as_str().unwrap()).unwrap()).unwrap();
        edit(&mut payload);
        let bytes = serde_json::to_vec(&payload).unwrap();
        let sig = w.agent.sign(crate::keys::REQUEST_DOMAIN, &bytes);
        json!({"signed": {"v": 1, "payload": b64.encode(&bytes), "key": w.agent.public().id(), "sig": b64.encode(sig)}})
    }

    #[test]
    fn round_trip() {
        let w = world();
        let n = new_nonce();
        assert_eq!(n.len(), 32);
        let o = open(&w, &request(&w, NOW, &n), NOW, &mut vec![]).unwrap();
        assert_eq!(o.key, w.agent.public().id());
        assert_eq!(o.method, "invoke");
        assert_eq!(o.nonce, n);
        assert_eq!(o.params["target"], "r");
    }

    #[test]
    fn params_caller_is_carried_verbatim_not_trusted() {
        let w = world();
        let o = open(&w, &request(&w, NOW, &new_nonce()), NOW, &mut vec![]).unwrap();
        assert_eq!(o.params["caller"], "", "the envelope does not interpret params; the core overrides caller");
    }

    #[test]
    fn unknown_key() {
        let w = world();
        let stranger = World { agent: generate(), core: generate() };
        let frame = seal_request(&stranger.agent, &w.core.public().id(), "invoke", &json!({}), NOW, &new_nonce());
        assert_eq!(open(&w, &frame, NOW, &mut vec![]).unwrap_err(), Refusal::UnknownKey);
    }

    #[test]
    fn flipped_payload_bit() {
        let w = world();
        let mut frame = request(&w, NOW, &new_nonce());
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut bytes = b64.decode(frame["signed"]["payload"].as_str().unwrap()).unwrap();
        bytes[5] ^= 1;
        frame["signed"]["payload"] = json!(b64.encode(bytes));
        assert_eq!(open(&w, &frame, NOW, &mut vec![]).unwrap_err(), Refusal::BadSignature);
    }

    #[test]
    fn flipped_signature_bit() {
        let w = world();
        let mut frame = request(&w, NOW, &new_nonce());
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut sig = b64.decode(frame["signed"]["sig"].as_str().unwrap()).unwrap();
        sig[0] ^= 1;
        frame["signed"]["sig"] = json!(b64.encode(sig));
        assert_eq!(open(&w, &frame, NOW, &mut vec![]).unwrap_err(), Refusal::BadSignature);
    }

    #[test]
    fn malformed_frames() {
        let w = world();
        for frame in [json!({}), json!({"signed": {"v": 2}}), json!({"signed": {"v":1,"payload":"!!","key":"x","sig":"y"}})] {
            assert!(open(&w, &frame, NOW, &mut vec![]).is_err(), "{frame}");
        }
        let mut wrong_version = request(&w, NOW, &new_nonce());
        wrong_version["signed"]["v"] = json!(2);
        assert_eq!(open(&w, &wrong_version, NOW, &mut vec![]).unwrap_err(), Refusal::Malformed);
    }

    #[test]
    fn wrong_purpose() {
        let w = world();
        let frame = resign(&w, &request(&w, NOW, &new_nonce()), |p| p["purpose"] = json!("summonr/response/v1"));
        assert_eq!(open(&w, &frame, NOW, &mut vec![]).unwrap_err(), Refusal::WrongPurpose);
    }

    #[test]
    fn familiar_key_mismatch() {
        let w = world();
        let frame = resign(&w, &request(&w, NOW, &new_nonce()), |p| p["familiar"] = json!("0".repeat(64)));
        assert_eq!(open(&w, &frame, NOW, &mut vec![]).unwrap_err(), Refusal::KeyMismatch);
    }

    #[test]
    fn wrong_audience() {
        let w = world();
        let other_core = generate();
        let frame = seal_request(&w.agent, &other_core.public().id(), "invoke", &json!({}), NOW, &new_nonce());
        assert_eq!(open(&w, &frame, NOW, &mut vec![]).unwrap_err(), Refusal::WrongAudience);
    }

    #[test]
    fn stale_and_future_by_121s_are_clock_skew() {
        let w = world();
        for ts in [NOW - 121_000, NOW + 121_000] {
            let err = open(&w, &request(&w, ts, &new_nonce()), NOW, &mut vec![]).unwrap_err();
            assert!(matches!(err, Refusal::ClockSkew { .. }), "{err:?}");
            assert!(err.reason().contains("clock skew"), "{}", err.reason());
        }
    }

    #[test]
    fn accepts_at_119s() {
        let w = world();
        for ts in [NOW - 119_000, NOW + 119_000] {
            assert!(open(&w, &request(&w, ts, &new_nonce()), NOW, &mut vec![]).is_ok());
        }
    }

    #[test]
    fn replayed_nonce() {
        let w = world();
        let frame = request(&w, NOW, &new_nonce());
        let mut used = vec![];
        open(&w, &frame, NOW, &mut used).unwrap();
        assert_eq!(open(&w, &frame, NOW + 1, &mut used).unwrap_err(), Refusal::Replayed);
    }

    #[test]
    fn reply_round_trip() {
        let w = world();
        let reply = seal_reply(&w.core, "abc", &Ok(json!({"summon": "s-1"})));
        assert_eq!(open_reply(&reply, &w.core.public(), "abc").unwrap().unwrap()["summon"], "s-1");
    }

    #[test]
    fn reply_with_null_result_round_trips() {
        let w = world();
        let reply = seal_reply(&w.core, "abc", &Ok(Value::Null));
        assert_eq!(open_reply(&reply, &w.core.public(), "abc").unwrap().unwrap(), Value::Null);
    }

    #[test]
    fn reply_wrong_core_key() {
        let w = world();
        let reply = seal_reply(&generate(), "abc", &Ok(json!({})));
        assert!(open_reply(&reply, &w.core.public(), "abc").is_err());
    }

    #[test]
    fn reply_wrong_nonce() {
        let w = world();
        let reply = seal_reply(&w.core, "abc", &Ok(json!({})));
        assert!(open_reply(&reply, &w.core.public(), "abd").is_err());
    }

    #[test]
    fn reply_carries_rpc_error() {
        let w = world();
        let reply = seal_reply(&w.core, "abc", &Err(RpcErr::not_found("no summon `s-x`")));
        let e = open_reply(&reply, &w.core.public(), "abc").unwrap().unwrap_err();
        assert_eq!(e.code, "not_found");
        assert_eq!(e.exit, 3);
        assert_eq!(e.message, "no summon `s-x`");
    }

    #[test]
    fn request_signature_is_not_a_reply_signature() {
        let w = world();
        // A request sealed by the core key must not pass as a reply.
        let frame = seal_request(&w.core, &w.core.public().id(), "x", &json!({}), NOW, "abc");
        assert!(open_reply(&frame, &w.core.public(), "abc").is_err());
    }
}
