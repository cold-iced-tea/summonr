//! signed requests: verification, the method gate, and separation between
//! familiars. Unsigned requests never reach this module.
use super::*;
use crate::envelope::{self, Refusal};

/// What a signed caller may ask the core to do. Everything else is the
/// owner's, at the terminal.
const SIGNED_METHODS: &[&str] = &[
    "rituals", "contract", "rehearse", "invoke", "get", "wait", "events", "cancel", "petitions",
    "fence_list", "compile", "drill_inline",
    "pact_list", "pact_show", "pact_call", "pact_call_status",
];

/// What a signed caller may say about a run. The model, settings, version
/// and run mode are the owner's; limits and the hash guard can only tighten.
const SIGNED_RUN_FIELDS: &[&str] = &[
    "target",
    "args",
    "expected_hash",
    "idempotency_key",
    "limits",
    "caller",
    "cwd",
    "simulate",
    "cassette_record",
    "cassette_replay",
    "petition_missing",
];

/// The longest a signed caller may block on `wait`, as over MCP.
const MAX_SIGNED_WAIT_MS: u64 = 300_000;

/// Agents, named or anonymous, may not change user-only inputs or record,
/// replay or simulate.
pub(crate) fn is_agent(caller: &str) -> bool {
    caller == "agent" || caller.starts_with("familiar:") || caller.starts_with("pact:")
}

impl Core {
    /// Verify a signed frame, act as its familiar, and sign the reply.
    pub(crate) fn handle_signed(self: &Arc<Self>, frame: &Value) -> Value {
        let audience = self.key.public().id();
        let now = now_ms();
        // One lookup: the familiar the signature was checked against is the
        // one that acts, even if it is revoked a moment later.
        let found: std::cell::RefCell<Option<(String, String, Vec<String>)>> = std::cell::RefCell::new(None);
        let ward: std::cell::RefCell<Option<(String, crate::keys::PublicKey)>> = std::cell::RefCell::new(None);
        let party: std::cell::RefCell<Option<(String, crate::keys::PublicKey)>> = std::cell::RefCell::new(None);
        let lookup = |id: &str| {
            if let Some((name, key, allowed)) = self.familiar_by_key(id) {
                *found.borrow_mut() = Some((key.id(), name, allowed));
                return Some(key);
            }
            // A core this core witnesses, or a pact counterparty, signing with
            // its core key. One core may be both; the method decides which.
            let w = self.ward_by_key(id);
            let c = self.counterparty_by_key(id);
            let key = w.as_ref().map(|(_, k)| *k).or_else(|| c.as_ref().map(|(_, k)| *k));
            *ward.borrow_mut() = w;
            *party.borrow_mut() = c;
            key
        };
        let mut seen = |key: &str, nonce: &str, _ts: i64| self.nonce_seen(key, nonce, now);
        let outcome = match envelope::open_request(frame, &lookup, &audience, now, &mut seen) {
            Ok(opened) => {
                let party = party.into_inner().filter(|(_, k)| k.id() == opened.key);
                let ward = ward.into_inner().filter(|(_, k)| k.id() == opened.key);
                if let Some((name, key)) = party.filter(|_| opened.method.starts_with("pact_") || ward.is_none()) {
                    let result = self.dispatch_counterparty(&name, &key, &opened.method, opened.params);
                    return envelope::seal_reply(&self.key, &opened.nonce, &result);
                }
                if let Some((name, key)) = ward {
                    let result = self.dispatch_ward(&name, &key, &opened.method, opened.params);
                    return envelope::seal_reply(&self.key, &opened.nonce, &result);
                }
                let Some((key_id, name, allowed)) = found.into_inner().filter(|(k, _, _)| *k == opened.key) else {
                    // Unreachable unless the lookup and the signature disagree; refuse.
                    self.refused(&Refusal::UnknownKey, &opened.key);
                    let generic = RpcErr::new("not_authenticated", 4, "request not authenticated");
                    return envelope::seal_reply(&self.key, &opened.nonce, &Err(generic));
                };
                debug_assert_eq!(key_id, opened.key);
                let caller = format!("familiar:{name}");
                let result = self.dispatch_as(&caller, &allowed, &opened.method, opened.params);
                return envelope::seal_reply(&self.key, &opened.nonce, &result);
            }
            Err(refusal) => refusal,
        };
        self.refused(&outcome, &envelope::claimed_key(frame));
        let generic = RpcErr::new("not_authenticated", 4, "request not authenticated");
        envelope::seal_reply(&self.key, &envelope::claimed_nonce(frame), &Err(generic))
    }

    fn refused(&self, r: &Refusal, claimed: &str) {
        let short: String = claimed.chars().take(16).collect();
        let reason = format!("{} (claimed key {short}…, unverified)", r.reason());
        eprintln!("signed request refused: {reason}");
        *self.last_refusal.lock().unwrap() = Some(reason);
    }

    /// The most recent refusal reason; the caller only ever sees a generic one.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn last_refusal(&self) -> Option<String> {
        self.last_refusal.lock().unwrap().clone()
    }

    /// Record a nonce; true if this familiar already used it. Nonces outlive
    /// a restart and expire well after the clock window has closed.
    fn nonce_seen(&self, key: &str, nonce: &str, now: i64) -> bool {
        let db = self.db.lock().unwrap();
        let _ = db.execute("DELETE FROM nonces WHERE expires < ?1", params![now]);
        match db.execute(
            "INSERT OR IGNORE INTO nonces (key_id, nonce, expires) VALUES (?1, ?2, ?3)",
            params![key, nonce, now + 3 * envelope::WINDOW_MS],
        ) {
            Ok(1) => false,
            // Already present, or storage failed: never accept on doubt.
            _ => true,
        }
    }

    /// The caller that started the run this summon belongs to: `None` if no
    /// such summon, `Some(None)` if the owner started it.
    pub(crate) fn owner_of(&self, summon: &str) -> Option<Option<String>> {
        let db = self.db.lock().unwrap();
        let mut id = summon.to_string();
        for _ in 0..64 {
            let (parent, familiar): (Option<String>, Option<String>) = db
                .query_row("SELECT parent, familiar FROM summons WHERE id=?1", params![id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()
                .ok()
                .flatten()?;
            match parent {
                Some(p) => id = p,
                None => return Some(familiar.filter(|p| !p.is_empty())),
            }
        }
        None
    }

    /// Keep only array items whose `summon` belongs to `caller`.
    fn filter_owned_by_summon(&self, caller: &str, items: Value) -> Value {
        let mine: Vec<Value> = items
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| {
                item["summon"]
                    .as_str()
                    .and_then(|s| self.owner_of(s))
                    == Some(Some(caller.to_string()))
            })
            .cloned()
            .collect();
        Value::Array(mine)
    }

    pub(super) fn dispatch_as(self: &Arc<Self>, caller: &str, allowed: &[String], method: &str, mut params: Value) -> Result<Value, RpcErr> {
        if !SIGNED_METHODS.contains(&method) {
            return Err(RpcErr::new("not_authorized", 4, format!("`{method}` belongs to the owner of this core")));
        }
        if !params.is_object() {
            return Err(RpcErr::invalid("params must be an object"));
        }
        let unknown_ritual = |name: &str| RpcErr::not_found(format!("no ritual `{name}` in the grimoire"));
        match method {
            "rituals" => {
                let all = self.dispatch("rituals", params)?;
                let visible: Vec<Value> = all
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|r| r["ritual"].as_str().is_some_and(|n| allowed.iter().any(|a| a == n)))
                    .cloned()
                    .collect();
                Ok(Value::Array(visible))
            }
            "contract" => {
                let name = params["name"].as_str().unwrap_or("").to_string();
                if !allowed.contains(&name) {
                    return Err(unknown_ritual(&name));
                }
                self.dispatch(method, params)
            }
            "compile" => self.dispatch(method, params),
            "fence_list" => {
                let all = self.dispatch("fence_list", params)?;
                Ok(self.filter_owned_by_summon(caller, all))
            }
            "drill_inline" => {
                let name = params["ritual"].as_str().unwrap_or("").to_string();
                if !allowed.contains(&name) {
                    return Err(unknown_ritual(&name));
                }
                self.dispatch(method, params)
            }
            "rehearse" | "invoke" => {
                let target = params["target"].as_str().unwrap_or("").to_string();
                if is_path(&target) || !allowed.contains(&target) {
                    return Err(unknown_ritual(&target));
                }
                if let Some((field, _)) = params
                    .as_object()
                    .into_iter()
                    .flatten()
                    .find(|(k, v)| !SIGNED_RUN_FIELDS.contains(&k.as_str()) && !v.is_null())
                {
                    return Err(RpcErr::new(
                        "not_authorized",
                        4,
                        format!("`{field}` is chosen by the owner of this core, not by a calling agent"),
                    ));
                }
                // Paths are refused above, so the caller's directory is irrelevant.
                params["cwd"] = json!("/");
                // The familiar is who the signature says, whatever params claim.
                params["caller"] = json!(caller);
                self.dispatch(method, params)
            }
            "get" | "wait" | "events" | "cancel" => {
                if method == "wait" && params["timeout_ms"].as_u64().is_some_and(|t| t > MAX_SIGNED_WAIT_MS) {
                    return Err(RpcErr::invalid(format!("a signed wait is at most {MAX_SIGNED_WAIT_MS} ms; wait again")));
                }
                let id = params["summon"].as_str().unwrap_or("").to_string();
                if self.owner_of(&id) != Some(Some(caller.to_string())) {
                    // The same answer as for a summon that does not exist.
                    return self.summon(&format!("{id}\u{0}missing")).map_err(|mut e| {
                        e.message = e.message.replace(&format!("{id}\u{0}missing"), &id);
                        e
                    });
                }
                self.dispatch(method, params)
            }
            "petitions" => {
                params["status"] = json!("pending");
                let all = self.dispatch("petitions", params)?;
                Ok(self.filter_owned_by_summon(caller, all))
            }
            "pact_list" => {
                let all = self.dispatch("pact_list", json!({}))?;
                let visible: Vec<Value> = all
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|r| r["name"].as_str().is_some_and(|n| allowed.contains(&format!("pact:{n}"))))
                    .cloned()
                    .collect();
                Ok(Value::Array(visible))
            }
            "pact_show" | "pact_call" | "pact_call_status" => {
                // Who is calling is what the signature says, never a param.
                if let Some(m) = params.as_object_mut() {
                    m.remove("caller");
                }
                let which = params["pact"].as_str().unwrap_or("").to_string();
                let missing = || RpcErr::not_found(format!("no pact `{which}`"));
                let row = self.pact_row(&which).map_err(|_| missing())?;
                if !allowed.contains(&format!("pact:{}", row.name)) {
                    return Err(missing());
                }
                match method {
                    "pact_call" => self.pact_call_as(caller, params),
                    "pact_call_status" => self.pact_call_status(caller, params),
                    _ => {
                        let mut shown = self.pact_show(params)?;
                        // A familiar sees only the calls it made.
                        let mine: Vec<Value> = shown["calls"].as_array().into_iter().flatten().filter(|c| c["asked_by"] == caller).cloned().collect();
                        shown["calls"] = json!(mine);
                        Ok(shown)
                    }
                }
            }
            _ => unreachable!("every signed method is handled above"),
        }
    }
}
