//! Pacts: agreed terms between two cores, and the signed thread of entries
//! both cores hold. Pure: no storage, no network, no clock.

use crate::keys::{Keypair, PublicKey};
use crate::ritual::Essence;
use crate::util::{canonical_json, parse_duration, sha256_hex};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Every pact entry is signed over this prefix and the entry's exact bytes.
pub const ENTRY_DOMAIN: &[u8] = b"summonr-pact-v1\0";

/// The terms two cores agree to. The pact id is the hash of their canonical form.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Terms {
    pub pact: String,
    pub caller: String,
    pub callee: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub witnesses: Vec<String>,
    pub ritual: Pinned,
    #[serde(default)]
    pub fixed: Map<String, Value>,
    #[serde(default)]
    pub inputs: Vec<String>,
    pub ceilings: Ceilings,
    #[serde(default)]
    pub held_by_caller: Vec<CallerHold>,
    #[serde(default)]
    pub accept: AcceptMode,
    #[serde(default)]
    pub checks: Checks,
    pub expires: String,
    #[serde(default)]
    pub revises: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Pinned {
    pub name: String,
    pub hash: String,
}

/// A callee step the caller must also approve, when `when` (true, or a
/// reference to a boolean the ritual computes) holds.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CallerHold {
    pub step: String,
    #[serde(default = "yes")]
    pub when: Value,
}

fn yes() -> Value {
    Value::Bool(true)
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AcceptMode {
    #[default]
    Owner,
    Checks,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Checks {
    #[serde(default)]
    pub schema: bool,
    #[serde(default)]
    pub ritual: Option<AcceptRitual>,
}

/// The caller's acceptance ritual, carried in full so both sides see it.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AcceptRitual {
    pub name: String,
    pub hash: String,
    pub source: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Ceilings {
    #[serde(default)]
    pub per_call: Essence,
    #[serde(default)]
    pub total: Totals,
    pub deadline: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Totals {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u64>,
}

pub fn parse_terms(yaml: &str) -> Result<Terms, String> {
    serde_yaml::from_str(yaml).map_err(|e| format!("invalid pact terms: {e}"))
}

/// The canonical bytes of the terms, as signed, stored and hashed.
pub fn terms_bytes(t: &Terms) -> String {
    canonical_json(&serde_json::to_value(t).expect("terms serialise"))
}

pub fn pact_id(t: &Terms) -> String {
    sha256_hex(terms_bytes(t).as_bytes())
}

/// The end of `date` (YYYY-MM-DD), UTC, in milliseconds.
pub fn expires_ms(date: &str) -> Result<i64, String> {
    let bad = || format!("`expires` must be a date such as 2027-11-16, found `{date}`");
    let parts: Vec<&str> = date.split('-').collect();
    let [ys, ms, ds] = parts.as_slice() else { return Err(bad()) };
    if ys.len() != 4 || ms.len() != 2 || ds.len() != 2 {
        return Err(bad());
    }
    let (y, m, d): (i64, i64, i64) = (ys.parse().map_err(|_| bad())?, ms.parse().map_err(|_| bad())?, ds.parse().map_err(|_| bad())?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days_in = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=12).contains(&m) || d < 1 || d > days_in[(m - 1) as usize] || !(1970..=9999).contains(&y) {
        return Err(bad());
    }
    // Days from the civil date (Howard Hinnant's algorithm), then the day's last millisecond.
    let (yy, mm) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Ok((days + 1) * 86_400_000 - 1)
}

/// Problems with the terms' shape, before either side looks at its rituals.
pub fn check_shape(t: &Terms) -> Vec<String> {
    let mut errors = Vec::new();
    if t.pact.is_empty() || !t.pact.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        errors.push(format!("pact name `{}` must use letters, digits, `-` or `_`", t.pact));
    }
    let keys: Vec<Option<PublicKey>> = [&t.caller, &t.callee]
        .iter()
        .map(|k| match PublicKey::parse(k) {
            Ok(p) => Some(p),
            Err(e) => {
                errors.push(format!("pact key `{k}`: {e}"));
                None
            }
        })
        .collect();
    if let [Some(a), Some(b)] = keys.as_slice() {
        if a.id() == b.id() {
            errors.push("the caller and the callee must be different cores".into());
        }
        let mut seen = std::collections::HashSet::from([a.id(), b.id()]);
        for wk in &t.witnesses {
            match PublicKey::parse(wk) {
                Ok(w) => {
                    if !seen.insert(w.id()) {
                        errors.push(format!("pact witness `{wk}` duplicates another party"));
                    }
                }
                Err(e) => errors.push(format!("pact witness `{wk}`: {e}")),
            }
        }
    }
    if t.ritual.hash.len() != 64 || !t.ritual.hash.chars().all(|c| c.is_ascii_hexdigit()) {
        errors.push("ritual hash must be 64 hex characters".into());
    }
    for name in &t.inputs {
        if t.fixed.contains_key(name) {
            errors.push(format!("input `{name}` is both fixed and given by calls"));
        }
    }
    match parse_duration(&t.ceilings.deadline) {
        Ok(ms) if ms > 365 * 86_400_000 => errors.push("ceilings.deadline must not exceed 365d".into()),
        Ok(_) => {}
        Err(e) => errors.push(format!("ceilings.deadline: {e}")),
    }
    if let Some(time) = &t.ceilings.per_call.time {
        match parse_duration(time) {
            Ok(ms) if ms > 365 * 86_400_000 => errors.push("ceilings.per_call.time must not exceed 365d".into()),
            Ok(_) => {}
            Err(e) => errors.push(format!("ceilings.per_call.time: {e}")),
        }
    }
    if let Err(e) = expires_ms(&t.expires) {
        errors.push(e);
    }
    let mut seen_steps = std::collections::BTreeSet::new();
    for h in &t.held_by_caller {
        if !seen_steps.insert(&h.step) {
            errors.push(format!("held_by_caller lists step `{}` more than once", h.step));
        }
        if !matches!(&h.when, Value::Bool(true) | Value::String(_)) {
            errors.push(format!("held_by_caller `{}`: `when` must be true or a reference to a boolean", h.step));
        }
    }
    if let Some(r) = &t.checks.ritual {
        match crate::ritual::parse_file(&r.source) {
            Ok(def) if crate::ritual::content_hash(&def) != r.hash => {
                errors.push(format!("acceptance ritual `{}`: its source does not match its hash", r.name))
            }
            Ok(def) if def.ritual != r.name => {
                errors.push(format!("acceptance ritual is named `{}` in its source, `{}` in the terms", def.ritual, r.name))
            }
            Ok(def) if def.bindings.values().any(|b| b.document.is_some()) => {
                errors.push(format!("acceptance ritual `{}` must be self-contained and cannot reference external document files", r.name))
            }
            Ok(_) => {}
            Err(e) => errors.push(format!("acceptance ritual `{}`: {e}", r.name)),
        }
    }
    errors
}

// ------------------------------------------------------------------ entries

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Offer,
    Agree,
    Decline,
    Call,
    Deliver,
    Decide,
    Settle,
    Accept,
    Reject,
    End,
}

/// An entry as it travels and is stored: exact bytes, signature, hash.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Signed {
    pub bytes: String,
    pub sig: String,
    pub hash: String,
}

/// A verified entry, parsed from its signed bytes.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub v: i64,
    pub pact: String,
    pub n: i64,
    pub prev: String,
    pub by: String,
    pub kind: Kind,
    #[serde(default)]
    pub call: Option<i64>,
    pub at: i64,
    pub data: Value,
    #[serde(skip)]
    pub hash: String,
}

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[allow(clippy::too_many_arguments)]
pub fn seal_entry(k: &Keypair, pact: &str, n: i64, prev: &str, kind: Kind, call: Option<i64>, at: i64, data: Value) -> Signed {
    use base64::Engine;
    let bytes = canonical_json(&serde_json::json!({
        "v": 1, "pact": pact, "n": n, "prev": prev, "by": k.public().id(),
        "kind": kind, "call": call, "at": at, "data": data,
    }));
    let sig = B64.encode(k.sign(ENTRY_DOMAIN, bytes.as_bytes()));
    Signed { hash: sha256_hex(bytes.as_bytes()), bytes, sig }
}

/// Verify the signature over the raw bytes, then parse; `by` must be `key`.
pub fn open_entry(s: &Signed, key: &PublicKey) -> Result<Entry, String> {
    use base64::Engine;
    let sig = B64.decode(&s.sig).map_err(|_| "entry signature is not base64".to_string())?;
    if !key.verify(ENTRY_DOMAIN, s.bytes.as_bytes(), &sig) {
        return Err("entry signature does not verify".into());
    }
    if s.hash != sha256_hex(s.bytes.as_bytes()) {
        return Err("entry hash does not match its bytes".into());
    }
    let mut e: Entry = serde_json::from_str(&s.bytes).map_err(|e| format!("entry is malformed: {e}"))?;
    if e.v != 1 {
        return Err(format!("unsupported pact entry version {}", e.v));
    }
    if e.by != key.id() {
        return Err("entry names a different signer".into());
    }
    e.hash = s.hash.clone();
    Ok(e)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Caller,
    Callee,
}

/// Which side signed `e`, by the terms' keys.
pub fn side_of(e: &Entry, t: &Terms) -> Option<Side> {
    let id = |k: &str| PublicKey::parse(k).map(|p| p.id()).ok();
    if id(&t.caller).as_deref() == Some(e.by.as_str()) {
        Some(Side::Caller)
    } else if id(&t.callee).as_deref() == Some(e.by.as_str()) {
        Some(Side::Callee)
    } else {
        None
    }
}

/// Whether `next` may follow `thread` (already checked, in order).
pub fn check_next(thread: &[Entry], next: &Entry, t: &Terms) -> Result<(), String> {
    let head = thread.last();
    let want_n = head.map(|h| h.n + 1).unwrap_or(1);
    if next.n != want_n {
        return Err(format!("entry {} is out of order; expected {want_n}", next.n));
    }
    let want_prev = head.map(|h| h.hash.as_str()).unwrap_or(crate::epitaph::ZEROS);
    if next.prev != want_prev {
        return Err(format!("entry {} does not follow entry {}", next.n, want_n - 1));
    }
    if next.pact != pact_id(t) {
        return Err("entry belongs to a different pact".into());
    }
    let side = side_of(next, t).ok_or("entry is signed by neither party")?;
    let offerer = thread.first().and_then(|o| side_of(o, t));
    let was_agreed = thread.iter().any(|e| e.kind == Kind::Agree);
    let state = thread.iter().map(|e| e.kind).fold(None, |s: Option<Kind>, k| match k {
        Kind::Offer | Kind::Agree | Kind::Decline | Kind::End => Some(k),
        _ => s,
    });
    // After an end, calls made before it may still finish; nothing else.
    if state == Some(Kind::End) {
        let last_call = thread.iter().filter(|e| e.kind == Kind::Call).filter_map(|e| e.call).max().unwrap_or(0);
        let finishing = was_agreed
            && matches!(next.kind, Kind::Deliver | Kind::Settle | Kind::Decide | Kind::Accept | Kind::Reject)
            && next.call.is_some_and(|c| c >= 1 && c <= last_call);
        if !finishing {
            return Err("the pact has ended; only calls made before the end may finish".into());
        }
    }
    let state = if state == Some(Kind::End) && was_agreed { Some(Kind::Agree) } else { state };
    let need = |want: Side, what: &str| if side == want { Ok(()) } else { Err(format!("only the {what} may sign `{:?}`", next.kind).to_lowercase()) };
    match (state, next.kind) {
        (None, Kind::Offer) => Ok(()),
        (None, _) => Err("a thread starts with an offer".into()),
        (Some(Kind::End | Kind::Decline), _) => Err("the pact has ended; nothing may follow".into()),
        (Some(Kind::Offer), Kind::Agree | Kind::Decline) => {
            if Some(side) == offerer { Err("the other party must agree to an offer".into()) } else { Ok(()) }
        }
        (Some(Kind::Offer), _) => Err("nothing but agree or decline may follow an offer".into()),
        (Some(_), Kind::Offer | Kind::Agree | Kind::Decline) => Err("the pact is already agreed".into()),
        (Some(_), Kind::Call | Kind::Decide | Kind::Accept | Kind::Reject) => need(Side::Caller, "caller"),
        (Some(_), Kind::Deliver | Kind::Settle) => need(Side::Callee, "callee"),
        (Some(_), Kind::End) => Ok(()),
    }
}

/// Two different entries by the same signer naming the same previous entry.
pub fn find_fork(entries: &[Entry]) -> Option<(Entry, Entry)> {
    for (i, a) in entries.iter().enumerate() {
        for b in &entries[i + 1..] {
            if a.pact == b.pact && a.by == b.by && a.prev == b.prev && a.hash != b.hash {
                return Some((a.clone(), b.clone()));
            }
        }
    }
    None
}

#[derive(Debug, Default)]
pub struct ThreadReport {
    pub entries: i64,
    pub calls: i64,
    pub forks: Vec<(String, String)>,
    pub epitaphs_checked: i64,
}

/// Check an exported thread with the two parties' keys (never keys from the
/// file). Returns the first bad entry number with the reason.
pub fn verify_thread(export: &Value, caller: &PublicKey, callee: &PublicKey) -> Result<ThreadReport, (i64, String)> {
    let terms: Terms = serde_json::from_value(export["terms"].clone()).map_err(|e| (0, format!("export has no valid terms: {e}")))?;
    let expected_pact = pact_id(&terms);
    let named = |k: &str| PublicKey::parse(k).map(|p| p.id()).unwrap_or_default();
    if named(&terms.caller) != caller.id() || named(&terms.callee) != callee.id() {
        return Err((0, "the terms name different cores than the keys given".into()));
    }
    let list: Vec<Signed> = serde_json::from_value(export["entries"].clone()).map_err(|e| (0, format!("export entries are malformed: {e}")))?;
    let key_for = |s: &Signed| -> PublicKey {
        let by = serde_json::from_str::<Value>(&s.bytes).ok().and_then(|v| v["by"].as_str().map(str::to_string)).unwrap_or_default();
        if by == callee.id() { *callee } else { *caller }
    };
    let mut report = ThreadReport::default();
    let mut all = Vec::new();
    let mut thread: Vec<Entry> = Vec::new();
    for s in &list {
        let shown = serde_json::from_str::<Value>(&s.bytes).ok().and_then(|v| v["n"].as_i64()).unwrap_or(0);
        let e = open_entry(s, &key_for(s)).map_err(|r| (shown, r))?;
        if e.pact != expected_pact {
            return Err((e.n, "entry belongs to a different pact".into()));
        }
        if thread.iter().any(|t| t.hash == e.hash) {
            return Err((e.n, "duplicate entry in thread".into()));
        }
        all.push(e.clone());
        // A second, different entry by the same signer on an already-used prev is a fork, reported below.
        if thread.iter().any(|t| t.by == e.by && t.prev == e.prev && t.hash != e.hash) {
            continue;
        }
        if e.n == 1 && e.data != serde_json::to_value(&terms).unwrap_or_default() {
            return Err((1, "the offer does not carry these terms".into()));
        }
        check_next(&thread, &e, &terms).map_err(|r| (e.n, r))?;
        if e.kind == Kind::Call {
            report.calls += 1;
        }
        if e.kind == Kind::Deliver && (!e.data["epitaph"].is_null() || !e.data["run"].is_null() || e.data["outcome"] == "returned") {
            check_delivery_epitaph(&e.data, callee, &terms.ritual.hash).map_err(|r| (e.n, r))?;
            report.epitaphs_checked += 1;
        }
        thread.push(e);
    }
    let mut rest = all.clone();
    while let Some((a, b)) = find_fork(&rest) {
        report.forks.push((a.hash.clone(), b.hash.clone()));
        rest.retain(|e| e.hash != b.hash);
    }
    report.entries = thread.len() as i64;
    Ok(report)
}

/// A delivery's epitaph must be signed by the callee's core and name the
/// pinned ritual.
pub fn check_delivery_epitaph(data: &Value, callee: &PublicKey, pinned_hash: &str) -> Result<(), String> {
    let ep = &data["epitaph"];
    let link = crate::epitaph::check_link(ep["link"].as_str().unwrap_or(""), ep["sig"].as_str().unwrap_or(""), callee)
        .map_err(|e| format!("delivery epitaph: {e}"))?;
    let body = crate::epitaph::check_body(&link, ep["body"].as_str().unwrap_or("")).map_err(|e| format!("delivery epitaph: {e}"))?;
    if body["ritual"]["hash"].as_str() != Some(pinned_hash) {
        return Err("delivery epitaph names a different ritual than the pact pins".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn caller() -> Keypair {
        Keypair::from_secret(&[1; 32])
    }
    pub(crate) fn callee() -> Keypair {
        Keypair::from_secret(&[2; 32])
    }

    pub(crate) fn yaml() -> String {
        format!(
            "pact: order-parts\ncaller: {}\ncallee: {}\nritual: {{name: quote-and-commit, hash: {}}}\nfixed:\n  max_unit_price_minor: 1250\ninputs: [part, quantity]\nceilings:\n  per_call: {{requests: 6, time: 10m}}\n  total: {{calls: 200}}\n  deadline: 48h\nheld_by_caller:\n  - {{step: commit, when: over_threshold}}\nchecks: {{schema: true}}\nexpires: 2027-11-16\n",
            caller().public().text(),
            callee().public().text(),
            "a".repeat(64)
        )
    }

    #[test]
    fn terms_round_trip_and_id_stable() {
        let t = parse_terms(&yaml()).unwrap();
        assert!(check_shape(&t).is_empty(), "{:?}", check_shape(&t));
        let reordered = yaml().replace("pact: order-parts\n", "") + "pact: order-parts\n";
        let t2 = parse_terms(&reordered).unwrap();
        assert_eq!(pact_id(&t), pact_id(&t2));
        assert_eq!(pact_id(&t).len(), 64);
        let again: Terms = serde_json::from_str(&terms_bytes(&t)).unwrap();
        assert_eq!(pact_id(&again), pact_id(&t));
    }

    #[test]
    fn unknown_field_refused() {
        assert!(parse_terms(&(yaml() + "bonus: 1\n")).is_err());
    }

    #[test]
    fn fixed_inputs_overlap_refused() {
        let t = parse_terms(&yaml().replace("inputs: [part, quantity]", "inputs: [part, max_unit_price_minor]")).unwrap();
        assert!(check_shape(&t).iter().any(|e| e.contains("max_unit_price_minor")), "{:?}", check_shape(&t));
    }

    #[test]
    fn accept_defaults_to_owner() {
        assert_eq!(parse_terms(&yaml()).unwrap().accept, AcceptMode::Owner);
        let t = parse_terms(&(yaml() + "accept: checks\n")).unwrap();
        assert_eq!(t.accept, AcceptMode::Checks);
    }

    #[test]
    fn acceptance_source_hash_mismatch_refused() {
        let src = "ritual: check-quote\ninputs:\n  delivery: {type: object}\nsteps: []\nreturn:\n  accept: true\n  reason: ok\n";
        let good = crate::ritual::content_hash(&crate::ritual::parse_file(src).unwrap());
        let with = |hash: &str| {
            let mut t = parse_terms(&yaml()).unwrap();
            t.checks.ritual = Some(AcceptRitual { name: "check-quote".into(), hash: hash.into(), source: src.into() });
            check_shape(&t)
        };
        assert!(with(&good).is_empty(), "{:?}", with(&good));
        assert!(with(&"b".repeat(64)).iter().any(|e| e.contains("hash")));
    }

    #[test]
    fn same_core_both_sides_refused() {
        let t = parse_terms(&yaml().replace(&callee().public().text(), &caller().public().text())).unwrap();
        assert!(!check_shape(&t).is_empty());
    }

    #[test]
    fn expires_parses_as_end_of_day_utc() {
        // 2027-11-16T23:59:59.999Z
        assert_eq!(expires_ms("2027-11-16").unwrap(), 1_826_409_599_999);
        assert!(expires_ms("2027-13-01").is_err());
    }

    // ------------------------------------------------------------ entries

    fn offer_entry() -> (Terms, Signed) {
        let t = parse_terms(&yaml()).unwrap();
        let id = pact_id(&t);
        let s = seal_entry(&caller(), &id, 1, crate::epitaph::ZEROS, Kind::Offer, None, 1, serde_json::to_value(&t).unwrap());
        (t, s)
    }

    fn thread() -> (Terms, Vec<Signed>) {
        let (t, offer) = offer_entry();
        let id = pact_id(&t);
        let agree = seal_entry(&callee(), &id, 2, &offer.hash, Kind::Agree, None, 2, serde_json::json!({"rehearsal": {}}));
        let call = seal_entry(&caller(), &id, 3, &agree.hash, Kind::Call, Some(1), 3, serde_json::json!({"inputs": {}}));
        (t, vec![offer, agree, call])
    }

    fn opened(list: &[Signed]) -> Vec<Entry> {
        list.iter().map(|s| open_entry(s, &key_of(s)).unwrap()).collect()
    }

    fn key_of(s: &Signed) -> PublicKey {
        let by = serde_json::from_str::<Value>(&s.bytes).unwrap()["by"].as_str().unwrap().to_string();
        if by == caller().public().id() { caller().public() } else { callee().public() }
    }

    #[test]
    fn entry_round_trip() {
        let (_, s) = offer_entry();
        let e = open_entry(&s, &caller().public()).unwrap();
        assert_eq!(e.n, 1);
        assert_eq!(e.kind, Kind::Offer);
        assert_eq!(s.hash, crate::util::sha256_hex(s.bytes.as_bytes()));
    }

    #[test]
    fn flipped_byte_fails() {
        let (_, mut s) = offer_entry();
        s.bytes = s.bytes.replacen("order-parts", "order-partz", 1);
        assert!(open_entry(&s, &caller().public()).is_err());
        let (_, s) = offer_entry();
        assert!(open_entry(&s, &callee().public()).is_err(), "wrong key");
    }

    #[test]
    fn wrong_domain_fails() {
        let (_, mut s) = offer_entry();
        use base64::Engine;
        s.sig = base64::engine::general_purpose::STANDARD.encode(caller().sign(crate::epitaph::LINK_DOMAIN, s.bytes.as_bytes()));
        assert!(open_entry(&s, &caller().public()).is_err());
    }

    #[test]
    fn wrong_side_refused() {
        let (t, list) = thread();
        let mut e = opened(&list);
        let bad = seal_entry(&callee(), &pact_id(&t), 3, &list[1].hash, Kind::Call, Some(1), 3, serde_json::json!({}));
        e[2] = open_entry(&bad, &callee().public()).unwrap();
        let err = check_next(&e[..2], &e[2], &t).unwrap_err();
        assert!(err.contains("caller"), "{err}");
    }

    #[test]
    fn gap_and_stale_prev_refused() {
        let (t, list) = thread();
        let e = opened(&list);
        assert!(check_next(&e[..1], &e[2], &t).is_err(), "gap");
        let stale = seal_entry(&caller(), &pact_id(&t), 3, &list[0].hash, Kind::Call, Some(1), 3, serde_json::json!({}));
        assert!(check_next(&e[..2], &open_entry(&stale, &caller().public()).unwrap(), &t).is_err());
        assert!(check_next(&e[..2], &e[2], &t).is_ok());
    }

    #[test]
    fn call_before_agree_refused() {
        let (t, list) = thread();
        let e = opened(&list);
        let early = seal_entry(&caller(), &pact_id(&t), 2, &list[0].hash, Kind::Call, Some(1), 2, serde_json::json!({}));
        let err = check_next(&e[..1], &open_entry(&early, &caller().public()).unwrap(), &t).unwrap_err();
        assert!(err.contains("agree"), "{err}");
    }

    #[test]
    fn agree_by_offerer_refused() {
        let (t, list) = thread();
        let e = opened(&list);
        let self_agree = seal_entry(&caller(), &pact_id(&t), 2, &list[0].hash, Kind::Agree, None, 2, serde_json::json!({}));
        assert!(check_next(&e[..1], &open_entry(&self_agree, &caller().public()).unwrap(), &t).is_err());
    }

    #[test]
    fn entry_after_end_refused() {
        let (t, list) = thread();
        let mut e = opened(&list);
        let end = seal_entry(&callee(), &pact_id(&t), 4, &list[2].hash, Kind::End, None, 4, serde_json::json!({}));
        e.push(open_entry(&end, &callee().public()).unwrap());
        let late = seal_entry(&caller(), &pact_id(&t), 5, &end.hash, Kind::Call, Some(2), 5, serde_json::json!({}));
        assert!(check_next(&e, &open_entry(&late, &caller().public()).unwrap(), &t).is_err());
    }

    #[test]
    fn fork_found() {
        let (t, list) = thread();
        let mut e = opened(&list);
        let other = seal_entry(&caller(), &pact_id(&t), 3, &list[1].hash, Kind::Call, Some(1), 9, serde_json::json!({"inputs": {"x": 1}}));
        e.push(open_entry(&other, &caller().public()).unwrap());
        let (a, b) = find_fork(&e).expect("fork");
        assert_eq!(a.prev, b.prev);
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn retry_identical_bytes_is_not_a_fork() {
        let (_, list) = thread();
        let mut e = opened(&list);
        e.push(open_entry(&list[2], &caller().public()).unwrap());
        assert!(find_fork(&e).is_none());
    }

    #[test]
    fn verify_thread_flags_fork_and_bad_epitaph() {
        let (t, list) = thread();
        let export = |entries: &[Signed]| serde_json::json!({"v": 1, "kind": "pact-thread", "terms": t, "entries": entries});
        let report = verify_thread(&export(&list), &caller().public(), &callee().public()).unwrap();
        assert_eq!(report.entries, 3);
        assert!(report.forks.is_empty());
        let mut forked = list.clone();
        forked.push(seal_entry(&caller(), &pact_id(&t), 3, &list[1].hash, Kind::Call, Some(1), 9, serde_json::json!({"inputs": {"x": 1}})));
        let report = verify_thread(&export(&forked), &caller().public(), &callee().public()).unwrap();
        assert_eq!(report.forks.len(), 1);
        // A delivery whose epitaph is not signed by the callee.
        let sealed = crate::epitaph::seal(&caller(), 1, crate::epitaph::ZEROS, &serde_json::json!({"v": 1, "ritual": {"hash": t.ritual.hash}}));
        let mut bad = list.clone();
        bad.push(seal_entry(&callee(), &pact_id(&t), 4, &list[2].hash, Kind::Deliver, Some(1), 4,
            serde_json::json!({"outcome": "returned", "epitaph": {"link": sealed.link, "body": sealed.body, "sig": sealed.sig}})));
        let (n, why) = verify_thread(&export(&bad), &caller().public(), &callee().public()).unwrap_err();
        assert_eq!(n, 4, "{why}");
        // Keys that do not match the terms are refused outright.
        assert!(verify_thread(&export(&list), &callee().public(), &caller().public()).is_err());
    }
}
