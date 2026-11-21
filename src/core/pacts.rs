//! pacts service: agreed terms between this core and a peer core, the
//! thread of signed entries both hold, and calls made under them.
use super::*;
use crate::epitaph;
use crate::link::CoreLink;
use crate::pact::{self, Entry, Kind, Signed, Terms};

/// What happened to an entry the other party sent.
#[derive(Debug)]
pub(crate) enum StoreOutcome {
    Stored,
    /// Exactly these bytes were already stored.
    Duplicate,
    /// It does not follow this core's head; the sender should sync and re-sign.
    Stale { head: i64 },
}

fn sql(e: rusqlite::Error) -> RpcErr {
    RpcErr::new("storage_failed", 11, e.to_string())
}

/// The stored terms of a pact.
pub(super) fn terms_of(conn: &rusqlite::Connection, id: &str) -> Result<Terms, RpcErr> {
    let text: String = conn
        .query_row("SELECT terms FROM pacts WHERE id=?1", params![id], |r| r.get(0))
        .optional()
        .map_err(sql)?
        .ok_or_else(|| RpcErr::not_found(format!("no pact `{id}`")))?;
    serde_json::from_str(&text).map_err(|e| RpcErr::new("storage_failed", 11, format!("stored terms are damaged: {e}")))
}

/// The thread as stored, parsed (signatures were checked when stored).
pub(super) fn thread_of(conn: &rusqlite::Connection, id: &str) -> Result<Vec<(Entry, Signed)>, RpcErr> {
    let mut q = conn.prepare("SELECT bytes, sig, hash FROM pact_entries WHERE pact=?1 ORDER BY n").map_err(sql)?;
    let rows = q
        .query_map(params![id], |r| Ok(Signed { bytes: r.get(0)?, sig: r.get(1)?, hash: r.get(2)? }))
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    rows.into_iter()
        .map(|s| {
            let mut e: Entry = serde_json::from_str(&s.bytes).map_err(|e| RpcErr::new("storage_failed", 11, format!("stored entry is damaged: {e}")))?;
            e.hash = s.hash.clone();
            Ok((e, s))
        })
        .collect()
}

fn insert(tx: &rusqlite::Transaction, id: &str, e: &Entry, s: &Signed) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO pact_entries (pact, n, bytes, sig, hash, by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, e.n, s.bytes, s.sig, s.hash, e.by],
    )?;
    Ok(())
}

/// Sign the next entry of `id` as this core, store it, and anchor it in this
/// core's epitaph chain, all in `tx`.
pub(super) fn append_own(tx: &rusqlite::Transaction, core: &crate::keys::Keypair, id: &str, kind: Kind, call: Option<i64>, data: Value) -> Result<Signed, RpcErr> {
    let terms = terms_of(tx, id)?;
    let thread: Vec<Entry> = thread_of(tx, id)?.into_iter().map(|(e, _)| e).collect();
    let (n, prev) = thread.last().map(|h| (h.n + 1, h.hash.clone())).unwrap_or((1, epitaph::ZEROS.to_string()));
    let s = pact::seal_entry(core, id, n, &prev, kind, call, now_ms(), data);
    let mut e = pact::open_entry(&s, &core.public()).map_err(RpcErr::invalid)?;
    e.hash = s.hash.clone();
    pact::check_next(&thread, &e, &terms).map_err(RpcErr::conflict)?;
    insert(tx, id, &e, &s).map_err(sql)?;
    super::epitaphs::write_pact_epitaph(tx, core, id, n, s.bytes.as_bytes()).map_err(sql)?;
    Ok(s)
}

/// Check and store an entry signed by one of the pact's parties. A second,
/// different entry from the same signer on the same previous entry is a
/// fork: both are kept, the pact is frozen, and the entry is refused.
/// A delivery carrying an invalid or mismatched epitaph also freezes the pact.
pub(super) fn store_received(tx: &rusqlite::Transaction, id: &str, s: &Signed, by: &crate::keys::PublicKey) -> Result<StoreOutcome, RpcErr> {
    let (state, side): (String, String) = tx
        .query_row("SELECT state, side FROM pacts WHERE id=?1", params![id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(sql)?
        .ok_or_else(|| RpcErr::not_found(format!("no pact `{id}`")))?;
    let terms = terms_of(tx, id)?;
    // On a witness, the callee forwards caller-signed entries too; verify the
    // entry under whichever party key (caller or callee) its `by` field names.
    let signer = if side == "witness" {
        let claimed = serde_json::from_str::<Value>(&s.bytes)
            .ok()
            .and_then(|v| v["by"].as_str().map(str::to_string))
            .unwrap_or_default();
        let caller_k = crate::keys::PublicKey::parse(&terms.caller).map_err(RpcErr::invalid)?;
        let callee_k = crate::keys::PublicKey::parse(&terms.callee).map_err(RpcErr::invalid)?;
        if claimed == caller_k.id() {
            caller_k
        } else if claimed == callee_k.id() {
            callee_k
        } else {
            return Err(RpcErr::new("not_authorized", 4, "pact entry refused: entry is signed by neither party"));
        }
    } else {
        *by
    };
    let e = pact::open_entry(s, &signer).map_err(|why| RpcErr::new("not_authorized", 4, format!("pact entry refused: {why}")))?;
    if e.pact != id {
        return Err(RpcErr::conflict("entry belongs to a different pact"));
    }
    let stored = thread_of(tx, id)?;
    if stored.iter().any(|(_, x)| x.hash == s.hash) {
        return Ok(StoreOutcome::Duplicate);
    }
    if state == "frozen" {
        return Err(RpcErr::conflict("the pact is frozen after a fork; nothing more is accepted"));
    }
    if let Some((_, twin)) = stored.iter().find(|(x, _)| x.by == e.by && x.prev == e.prev) {
        tx.execute(
            "INSERT INTO pact_forks (pact, a, b, seen) VALUES (?1, ?2, ?3, ?4)",
            params![id, serde_json::to_string(twin).unwrap_or_default(), serde_json::to_string(s).unwrap_or_default(), now_ms()],
        )
        .map_err(sql)?;
        tx.execute("UPDATE pacts SET state='frozen' WHERE id=?1", params![id]).map_err(sql)?;
        return Err(RpcErr::conflict("this entry forks the pact's thread: the same core signed two different entries on the same previous entry; both are kept as evidence and the pact is frozen"));
    }
    let thread: Vec<Entry> = stored.into_iter().map(|(e, _)| e).collect();
    if let Some(head) = thread.last() {
        if e.n != head.n + 1 || e.prev != head.hash {
            return Ok(StoreOutcome::Stale { head: head.n });
        }
    }
    pact::check_next(&thread, &e, &terms).map_err(RpcErr::conflict)?;
    // Enforce the delivery-epitaph check on every ingestion path (both pushed
    // deliveries and polled syncs), freezing the pact on mismatch.
    if e.kind == Kind::Deliver && (!e.data["epitaph"].is_null() || !e.data["run"].is_null() || e.data["outcome"] == "returned") {
        let callee_k = crate::keys::PublicKey::parse(&terms.callee).map_err(RpcErr::invalid)?;
        if let Err(why) = pact::check_delivery_epitaph(&e.data, &callee_k, &terms.ritual.hash) {
            let problem = format!("entry {}: {why}; the pact is frozen", e.n);
            tx.execute("UPDATE pacts SET state='frozen', problem=?2 WHERE id=?1", params![id, problem]).map_err(sql)?;
            return Err(RpcErr::conflict(problem));
        }
    }
    insert(tx, id, &e, s).map_err(sql)?;
    Ok(StoreOutcome::Stored)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Admit {
    name: String,
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Post {
    pact: String,
    entry: Signed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sync {
    pact: String,
    after: i64,
}

fn parse<T: serde::de::DeserializeOwned>(p: Value) -> Result<T, RpcErr> {
    serde_json::from_value(p).map_err(|e| RpcErr::invalid(e.to_string()))
}

fn key_id_of(text: &str) -> String {
    crate::keys::PublicKey::parse(text).map(|k| k.id()).unwrap_or_default()
}

impl Core {
    // ------------------------------------------------------------ admission

    pub(super) fn pact_admit(&self, p: Value) -> Result<Value, RpcErr> {
        let Admit { name, key } = parse(p)?;
        super::familiars::valid_name(&name)?;
        let key = crate::keys::PublicKey::parse(&key).map_err(RpcErr::invalid)?;
        let db = self.db.lock().unwrap();
        let familiar: i64 = db.query_row("SELECT COUNT(*) FROM familiars WHERE key_id=?1", params![key.id()], |r| r.get(0)).map_err(sql)?;
        if familiar > 0 {
            return Err(RpcErr::conflict("that key is a familiar here; one key cannot be both a familiar and a counterparty"));
        }
        let n = db
            .execute(
                "INSERT OR IGNORE INTO counterparties (name, key, key_id, created) VALUES (?1, ?2, ?3, ?4)",
                params![name, key.text(), key.id(), now_ms()],
            )
            .map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::conflict(format!("a counterparty named `{name}` or with this key already exists")));
        }
        Ok(json!({"name": name, "id": key.id()}))
    }

    pub(super) fn pact_unadmit(&self, p: Value) -> Result<Value, RpcErr> {
        let Named { name } = parse(p)?;
        let n = self.db.lock().unwrap().execute("DELETE FROM counterparties WHERE name=?1", params![name]).map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::not_found(format!("no counterparty `{name}`")));
        }
        Ok(json!({"name": name, "removed": true}))
    }

    pub(crate) fn counterparty_by_key(&self, key_id: &str) -> Option<(String, crate::keys::PublicKey)> {
        let (name, key): (String, String) = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT name, key FROM counterparties WHERE key_id=?1", params![key_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .ok()
            .flatten()?;
        Some((name, crate::keys::PublicKey::parse(&key).ok()?))
    }

    /// The pacts a counterparty (caller, callee, or witness) is a party to;
    /// anything else answers `not_found`. Also verifies the peer is pinned with this key.
    fn party_terms(&self, key: &crate::keys::PublicKey, id: &str) -> Result<Terms, RpcErr> {
        let missing = || RpcErr::not_found(format!("no pact `{id}`"));
        let row = self.pact_row(id).map_err(|_| missing())?;
        let t = terms_of(&self.db.lock().unwrap(), &row.id).map_err(|_| missing())?;
        let me = self.key.public().id();
        let kid = key.id();
        let is_witness = |k: &str| t.witnesses.iter().any(|w| key_id_of(w) == k);
        let allowed = (key_id_of(&t.caller) == kid && key_id_of(&t.callee) == me)
            || (key_id_of(&t.callee) == kid && key_id_of(&t.caller) == me)
            || (is_witness(&kid) && (key_id_of(&t.caller) == me || key_id_of(&t.callee) == me))
            || (is_witness(&me) && (key_id_of(&t.caller) == kid || key_id_of(&t.callee) == kid));
        if !allowed {
            return Err(missing());
        }
        let peer_name = self.peer_named_by_key(key);
        match peer_name.and_then(|p| self.pinned_peer(&p).ok()) {
            Some(pinned) if pinned.id() == kid => Ok(t),
            _ => Err(RpcErr::new(
                "not_authorized",
                4,
                format!("peer `{}` is no longer pinned with the key this pact names", row.peer),
            )),
        }
    }

    /// Best-effort async push of an entry to pinned witness cores only.
    fn push_to_witnesses_async(self: &Arc<Self>, pact_id: &str, entry: Signed) {
        let targets: Vec<String> = {
            let db = self.db.lock().unwrap();
            let Ok(t) = terms_of(&db, pact_id) else { return };
            drop(db);
            let mut peers = Vec::new();
            for w in &t.witnesses {
                if let Ok(wk) = crate::keys::PublicKey::parse(w) {
                    if let Some(p) = self.peer_named_by_key(&wk) {
                        if !peers.contains(&p) {
                            peers.push(p);
                        }
                    }
                }
            }
            peers
        };
        if targets.is_empty() {
            return;
        }
        let core = self.clone();
        let pact = pact_id.to_string();
        std::thread::spawn(move || {
            for peer in targets {
                let _ = core.pact_send(&peer, "pact_post", json!({"pact": pact, "entry": entry}));
            }
        });
    }

    /// Best-effort async push of a newly signed entry to reachable counterparties
    /// (callee -> caller push, and caller/callee -> witness push).
    fn push_to_peers_async(self: &Arc<Self>, pact_id: &str, entry: Signed) {
        let targets: Vec<String> = {
            let Ok(row) = self.pact_row(pact_id) else { return };
            let db = self.db.lock().unwrap();
            let Ok(t) = terms_of(&db, &row.id) else { return };
            drop(db);
            let mut peers = Vec::new();
            if row.side == "callee" && !row.peer.is_empty() {
                peers.push(row.peer.clone());
            }
            for w in &t.witnesses {
                if let Ok(wk) = crate::keys::PublicKey::parse(w) {
                    if let Some(p) = self.peer_named_by_key(&wk) {
                        if !peers.contains(&p) {
                            peers.push(p);
                        }
                    }
                }
            }
            peers
        };
        if targets.is_empty() {
            return;
        }
        let core = self.clone();
        let pact = pact_id.to_string();
        std::thread::spawn(move || {
            for peer in targets {
                let _ = core.pact_send(&peer, "pact_post", json!({"pact": pact, "entry": entry}));
            }
        });
    }

    /// Best-effort async push of an initial offer to any pinned witness cores.
    fn push_offer_to_witnesses_async(self: &Arc<Self>, t: &Terms, entry: Signed) {
        let mut peers = Vec::new();
        for w in &t.witnesses {
            if let Ok(wk) = crate::keys::PublicKey::parse(w) {
                if let Some(p) = self.peer_named_by_key(&wk) {
                    if !peers.contains(&p) {
                        peers.push(p);
                    }
                }
            }
        }
        if peers.is_empty() {
            return;
        }
        let core = self.clone();
        std::thread::spawn(move || {
            for peer in peers {
                let _ = core.pact_send(&peer, "pact_offer", json!({"entry": entry}));
            }
        });
    }

    /// What an admitted counterparty core may ask.
    pub(crate) fn dispatch_counterparty(self: &Arc<Self>, _name: &str, key: &crate::keys::PublicKey, method: &str, p: Value) -> Result<Value, RpcErr> {
        self.expire_due(now_ms());
        match method {
            "pact_post" => {
                let Post { pact, entry } = parse(p)?;
                self.party_terms(key, &pact)?;
                self.receive(&pact, &entry, key)
            }
            "pact_sync" => {
                let Sync { pact, after } = parse(p)?;
                self.party_terms(key, &pact)?;
                self.expire_due(now_ms());
                let db = self.db.lock().unwrap();
                let state: String = db.query_row("SELECT state FROM pacts WHERE id=?1", params![pact], |r| r.get(0)).map_err(sql)?;
                let thread = thread_of(&db, &pact)?;
                let head = thread.last().map(|(e, _)| e.n).unwrap_or(0);
                let entries: Vec<Signed> = thread.into_iter().filter(|(e, _)| e.n > after).map(|(_, s)| s).collect();
                Ok(json!({"entries": entries, "state": state, "head": head}))
            }
            "pact_offer" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Offered {
                    entry: Signed,
                }
                let Offered { entry } = parse(p)?;
                self.offered_to_me(key, &entry)
            }
            "pact_offers" => self.offers_for(key),
            "pact_rehearse" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Which {
                    pact: String,
                }
                let Which { pact } = parse(p)?;
                let t = self.party_terms(key, &pact)?;
                self.validate_on_callee(&t)
            }
            other => Err(RpcErr::new("not_authorized", 4, format!("a counterparty may only exchange pact entries, not call `{other}`"))),
        }
    }

    /// Store an entry from the other party and act on it. Fork evidence is
    /// committed even though the entry is refused.
    pub(super) fn receive(self: &Arc<Self>, pact: &str, entry: &Signed, by: &crate::keys::PublicKey) -> Result<Value, RpcErr> {
        let outcome = {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction().map_err(sql)?;
            let outcome = store_received(&tx, pact, entry, by);
            tx.commit().map_err(sql)?;
            outcome?
        };
        Ok(match outcome {
            StoreOutcome::Stored => {
                // Commit and reaction do not share one error channel: once the
                // entry is stored, answer `stored` so the sender does not drop
                // it and later fork.
                if let Err(e) = self.on_received(pact, entry) {
                    eprintln!("pact: reaction after storing entry on {pact} failed: {}", e.message);
                }
                // Forward stored caller entries from the callee to any pinned witnesses.
                if self.pact_row(pact).ok().is_some_and(|r| r.side == "callee") {
                    self.push_to_witnesses_async(pact, entry.clone());
                }
                json!({"stored": "stored"})
            }
            StoreOutcome::Duplicate => {
                // If a previous reaction failed or crashed after commit, retry
                // idempotent reactions on duplicate delivery.
                if let Err(e) = self.on_received(pact, entry) {
                    eprintln!("pact: reaction retry on duplicate entry for {pact} failed: {}", e.message);
                }
                json!({"stored": "duplicate"})
            }
            StoreOutcome::Stale { head } => {
                let db = self.db.lock().unwrap();
                let missing: Vec<Signed> = thread_of(&db, pact)?.into_iter().map(|(_, s)| s).collect();
                return Err(RpcErr::conflict("the entry does not follow this core's head; sync and sign again").with(json!({"head": head, "entries": missing})));
            }
        })
    }

    /// React to a newly stored entry from the other party.
    pub(super) fn on_received(self: &Arc<Self>, pact: &str, entry: &Signed) -> Result<(), RpcErr> {
        let e: Entry = serde_json::from_str(&entry.bytes).map_err(|e| RpcErr::invalid(e.to_string()))?;
        let set = |state: &str, from: &str| -> Result<(), RpcErr> {
            self.db.lock().unwrap().execute("UPDATE pacts SET state=?2 WHERE id=?1 AND state=?3", params![pact, state, from]).map_err(sql)?;
            Ok(())
        };
        match e.kind {
            Kind::Agree => {
                set("agreed", "offered")?;
                // The other side agreed a revision: its end of the old pact follows on that thread.
                Ok(())
            }
            Kind::Decline => set("declined", "offered"),
            Kind::End => {
                let to = if e.data["superseded_by"].is_string() { "superseded" } else { "ended" };
                self.db.lock().unwrap().execute("UPDATE pacts SET state=?2 WHERE id=?1 AND state IN ('offered','agreed')", params![pact, to]).map_err(sql)?;
                Ok(())
            }
            _ => self.on_received_more(pact, &e),
        }
    }

    /// Entry kinds that drive calls.
    fn on_received_more(self: &Arc<Self>, pact: &str, e: &Entry) -> Result<(), RpcErr> {
        // Witnesses record the signed thread only; they never execute calls or holds.
        if self.pact_row(pact).ok().is_some_and(|r| r.side == "witness") {
            return Ok(());
        }
        match e.kind {
            Kind::Call => self.start_call(pact, e),
            Kind::Decide => self.apply_caller_decision(pact, e),
            Kind::Deliver => self.on_delivered(pact, e),
            Kind::Settle => self.on_settled(pact, e),
            Kind::Accept | Kind::Reject => {
                let state = if e.kind == Kind::Accept { "accepted" } else { "rejected" };
                self.db.lock().unwrap().execute("UPDATE pact_calls SET state=?3 WHERE pact=?1 AND call=?2", params![pact, e.call, state]).map_err(sql)?;
                Ok(())
            }
            _ => Ok(()),
        }
    }



    // ------------------------------------------------------------ transport

    /// Send a signed request to pinned peer `peer`, signed with this core's key.
    pub(crate) fn pact_send(&self, peer: &str, method: &str, params: Value) -> Result<Value, RpcErr> {
        let pinned: Option<(String, String)> = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT url, key FROM peers WHERE name=?1", params![peer], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(sql)?;
        let (url, peer_key) = pinned.ok_or_else(|| RpcErr::not_found(format!("no pinned peer `{peer}`; add it with `summonr peer add {peer} url URL key K`")))?;
        let peer_key = crate::keys::PublicKey::parse(&peer_key).map_err(RpcErr::invalid)?;
        let me = crate::keys::load(&self.paths.config.join("core.key")).map_err(|e| RpcErr::new("storage_failed", 11, e))?;
        let target = url.clone();
        let mut link = crate::link::SignedLink::new(me, peer_key, Box::new(move |f: &Value| crate::link::http_send(&target, f)));
        link.call(method, params).map_err(|e| RpcErr::new(&e.code, e.exit, format!("peer `{peer}` ({url}): {}", e.message)).with(e.data.clone()))
    }
}

// ---------------------------------------------------------------- agreement

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    peer: String,
    file: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Peer {
    peer: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Which {
    pact: String,
    #[serde(default)]
    reason: Option<String>,
}

/// A pact row.
#[derive(Clone, Debug)]
pub(super) struct Row {
    pub id: String,
    pub name: String,
    pub peer: String,
    pub side: String,
    pub state: String,
}

/// Find a step by id anywhere in `steps`.
fn find_step<'a>(steps: &'a mut [crate::ritual::Step], id: &str) -> Option<&'a mut crate::ritual::Step> {
    for s in steps.iter_mut() {
        if s.id.as_deref() == Some(id) {
            return Some(s);
        }
        for inner in [s.steps.as_mut(), s.then.as_mut(), s.otherwise.as_mut()].into_iter().flatten() {
            if let Some(found) = find_step(inner, id) {
                return Some(found);
            }
        }
    }
    None
}

impl Core {
    pub(super) fn pact_row(&self, which: &str) -> Result<Row, RpcErr> {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, name, peer, side, state FROM pacts WHERE name=?1 OR id=?1",
                params![which],
                |r| Ok(Row { id: r.get(0)?, name: r.get(1)?, peer: r.get(2)?, side: r.get(3)?, state: r.get(4)? }),
            )
            .optional()
            .map_err(sql)?
            .ok_or_else(|| RpcErr::not_found(format!("no pact `{which}`")))
    }

    fn pinned_peer(&self, peer: &str) -> Result<crate::keys::PublicKey, RpcErr> {
        let key: String = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT key FROM peers WHERE name=?1", params![peer], |r| r.get(0))
            .optional()
            .map_err(sql)?
            .ok_or_else(|| RpcErr::not_found(format!("no pinned peer `{peer}`; add it with `summonr peer add {peer} url URL key K`")))?;
        crate::keys::PublicKey::parse(&key).map_err(RpcErr::invalid)
    }

    fn peer_named_by_key(&self, key: &crate::keys::PublicKey) -> Option<String> {
        self.db.lock().unwrap().query_row("SELECT name FROM peers WHERE key_id=?1", params![key.id()], |r| r.get(0)).optional().ok().flatten()
    }

    /// The other party's key, from the terms (never from the peer table).
    pub(super) fn other_key(&self, t: &Terms) -> Result<crate::keys::PublicKey, RpcErr> {
        let me = self.key.public().id();
        let other = if key_id_of(&t.caller) == me { &t.callee } else { &t.caller };
        crate::keys::PublicKey::parse(other).map_err(RpcErr::invalid)
    }

    /// Check the pinned ritual here, the callee: the exact hash exists, the
    /// caller holds sit on writes with boolean conditions, and the inputs are
    /// covered. Returns what both sides are shown.
    pub(super) fn validate_on_callee(&self, t: &Terms) -> Result<Value, RpcErr> {
        let row: Option<(u32, String)> = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT version, def FROM rituals WHERE name=?1 AND hash=?2", params![t.ritual.name, t.ritual.hash], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(sql)?;
        let (version, def) = row.ok_or_else(|| {
            RpcErr::invalid(format!("no ritual `{}` with hash {} in this grimoire; the pact must pin a version saved here", t.ritual.name, t.ritual.hash))
        })?;
        let def: crate::ritual::Ritual = serde_json::from_str(&def).map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        let mut problems = Vec::new();
        for (name, decl) in &def.inputs {
            let given = t.fixed.contains_key(name) || t.inputs.contains(name);
            if decl.default.is_none() && !given {
                problems.push(format!("input `{name}` is required but neither fixed nor given by calls"));
            }
            if decl.user_only && given {
                problems.push(format!("input `{name}` belongs to the owner (user_only); a pact cannot set it"));
            }
        }
        for name in t.fixed.keys().chain(t.inputs.iter()) {
            if !def.inputs.contains_key(name) {
                problems.push(format!("input `{name}` is not declared by `{}`", t.ritual.name));
            }
        }
        let mut held = def.clone();
        for h in &t.held_by_caller {
            match find_step(&mut held.steps, &h.step) {
                None => problems.push(format!("held_by_caller: no step `{}` in `{}`", h.step, t.ritual.name)),
                Some(step) => step.hold = Some(h.when.clone()),
            }
        }
        if !problems.is_empty() {
            return Err(RpcErr::invalid(problems.join("; ")));
        }
        // The same checks a `hold` gets: a write, and a boolean reference.
        let effects = crate::ritual::validate(&held, &self.resolver(), &reagent::tool, self.cfg.max_depth())
            .map_err(|errors| RpcErr::invalid(format!("held_by_caller: {}", errors.join("; "))))?;
        let grants = self.grants_for(&t.ritual.name)?;
        let missing: Vec<String> = def
            .requires
            .iter()
            .flat_map(Requirement::atoms)
            .filter(|n| !grants.iter().any(|g| g.covers(n)))
            .map(|g| g.describe())
            .collect();
        Ok(json!({
            "ritual": t.ritual.name, "version": version, "hash": t.ritual.hash, "description": def.description,
            "inputs": def.inputs, "returns": def.ret.keys().collect::<Vec<_>>(), "output_schema": def.output_schema, "essence": def.essence,
            "effects": effects.iter().map(|e| json!({"step": e.step, "what": e.what, "effect": e.effect, "determinism": e.determinism, "notes": e.notes})).collect::<Vec<_>>(),
            "model_sites": effects.iter().filter(|e| e.kind == "model").map(|e| json!({"step": e.step, "model": e.what})).collect::<Vec<_>>(),
            "authority": if missing.is_empty() { json!("granted") } else { json!({"missing": missing}) },
        }))
    }

    /// The caller's acceptance ritual, compiled from the source in the terms.
    pub(super) fn validate_acceptance(&self, t: &Terms) -> Result<Value, RpcErr> {
        let Some(r) = &t.checks.ritual else { return Ok(Value::Null) };
        let def = crate::ritual::parse_file(&r.source).map_err(|e| RpcErr::invalid(format!("acceptance ritual: {e}")))?;
        let effects = crate::ritual::validate(&def, &self.resolver(), &reagent::tool, self.cfg.max_depth())
            .map_err(|errors| RpcErr::invalid(format!("acceptance ritual: {}", errors.join("; "))))?;
        if !def.requires.is_empty() || effects.iter().any(|e| e.effect == "write") {
            return Err(RpcErr::invalid("acceptance ritual must not require external grants or perform writes"));
        }
        let inputs: Vec<&String> = def.inputs.keys().collect();
        if inputs != vec!["delivery"] || def.inputs["delivery"].ty != "object" {
            return Err(RpcErr::invalid("acceptance ritual must declare exactly one input, `delivery`, of type object"));
        }
        if !def.ret.contains_key("accept") || !def.ret.contains_key("reason") {
            return Err(RpcErr::invalid("acceptance ritual must return `accept` and `reason`"));
        }
        Ok(json!({
            "ritual": r.name, "hash": r.hash, "source": r.source,
            "effects": effects.iter().map(|e| json!({"step": e.step, "what": e.what, "effect": e.effect})).collect::<Vec<_>>(),
        }))
    }

    fn unique_name(&self, wanted: &str, id: &str) -> String {
        let taken = |n: &str| self.db.lock().unwrap().query_row("SELECT COUNT(*) FROM pacts WHERE name=?1", params![n], |r| r.get::<_, i64>(0)).unwrap_or(1) > 0;
        if taken(wanted) { format!("{wanted}-{}", &id[..8]) } else { wanted.to_string() }
    }

    fn insert_pact(&self, id: &str, name: &str, peer: &str, side: &str, t: &Terms) -> Result<(), RpcErr> {
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO pacts (id, name, peer, side, terms, state, created) VALUES (?1, ?2, ?3, ?4, ?5, 'offered', ?6)",
                params![id, name, peer, side, pact::terms_bytes(t), now_ms()],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => RpcErr::conflict("this pact or its name already exists here"),
                e => sql(e),
            })?;
        Ok(())
    }

    pub(super) fn pact_offer(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let Offer { peer, file, name } = parse(p)?;
        let text = std::fs::read_to_string(&file).map_err(|e| RpcErr::invalid(format!("cannot read {file}: {e}")))?;
        let t = pact::parse_terms(&text).map_err(RpcErr::invalid)?;
        let errors = pact::check_shape(&t);
        if !errors.is_empty() {
            return Err(RpcErr::invalid(errors.join("; ")));
        }
        let me = self.key.public().id();
        let side = if key_id_of(&t.caller) == me {
            "caller"
        } else if key_id_of(&t.callee) == me {
            "callee"
        } else {
            return Err(RpcErr::invalid("these terms do not name this core as caller or callee"));
        };
        if self.pinned_peer(&peer)?.id() != self.other_key(&t)?.id() {
            return Err(RpcErr::invalid(format!("peer `{peer}` is not the other core these terms name")));
        }
        if side == "callee" {
            self.validate_on_callee(&t)?;
        }
        self.validate_acceptance(&t)?;
        self.check_revision(&t)?;
        let id = pact::pact_id(&t);
        let name = self.unique_name(name.as_deref().unwrap_or(&t.pact), &id);
        self.insert_pact(&id, &name, &peer, side, &t)?;
        let data = serde_json::to_value(&t).expect("terms serialise");
        let entry = if side == "callee" {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction().map_err(sql)?;
            let s = append_own(&tx, &self.key, &id, Kind::Offer, None, data)?;
            tx.commit().map_err(sql)?;
            s
        } else {
            let s = pact::seal_entry(&self.key, &id, 1, epitaph::ZEROS, Kind::Offer, None, now_ms(), data);
            if let Err(e) = self.pact_send(&peer, "pact_offer", json!({"entry": s})) {
                self.db.lock().unwrap().execute("DELETE FROM pacts WHERE id=?1", params![id]).map_err(sql)?;
                return Err(e);
            }
            self.confirm_own(&id, &s)?;
            s
        };
        self.push_offer_to_witnesses_async(&t, entry.clone());
        Ok(json!({"pact": name, "id": id, "side": side, "state": "offered", "entry": entry.hash}))
    }

    /// A caller (or callee) core offers terms to this core (as callee or witness).
    fn offered_to_me(self: &Arc<Self>, key: &crate::keys::PublicKey, entry: &Signed) -> Result<Value, RpcErr> {
        let e = pact::open_entry(entry, key).map_err(|w| RpcErr::new("not_authorized", 4, format!("offer refused: {w}")))?;
        let t: Terms = serde_json::from_value(e.data.clone()).map_err(|err| RpcErr::invalid(format!("offer carries no valid terms: {err}")))?;
        let errors = pact::check_shape(&t);
        if !errors.is_empty() {
            return Err(RpcErr::invalid(errors.join("; ")));
        }
        let id = pact::pact_id(&t);
        if e.kind != Kind::Offer || e.n != 1 || e.pact != id {
            return Err(RpcErr::invalid("not an offer of these terms"));
        }
        let me = self.key.public().id();
        let is_witness = t.witnesses.iter().any(|w| key_id_of(w) == me);
        let side = if key_id_of(&t.caller) == key.id() && key_id_of(&t.callee) == me {
            "callee"
        } else if is_witness && (key_id_of(&t.caller) == key.id() || key_id_of(&t.callee) == key.id()) {
            "witness"
        } else {
            return Err(RpcErr::invalid("an offer must name its sender as caller/callee and this core as callee or witness"));
        };
        let peer = self.peer_named_by_key(key).ok_or_else(|| RpcErr::invalid("this core has not pinned the offering core as a peer"))?;
        if side == "callee" {
            self.validate_on_callee(&t)?;
        }
        self.validate_acceptance(&t)?;
        self.check_revision(&t)?;
        if self.pact_row(&id).is_err() {
            let name = self.unique_name(&t.pact, &id);
            self.insert_pact(&id, &name, &peer, side, &t)?;
        }
        self.receive(&id, entry, key)
    }

    /// Offers this core made to `key`'s core that are still open.
    fn offers_for(&self, key: &crate::keys::PublicKey) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare("SELECT p.id, p.terms, e.bytes, e.sig, e.hash FROM pacts p JOIN pact_entries e ON e.pact=p.id AND e.n=1 WHERE p.side='callee' AND p.state='offered'").map_err(sql)?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, String>(1)?, Signed { bytes: r.get(2)?, sig: r.get(3)?, hash: r.get(4)? })))
            .map_err(sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        let mine = self.key.public().id();
        let offers: Vec<Signed> = rows
            .into_iter()
            .filter(|(terms, s)| {
                let t: Option<Terms> = serde_json::from_str(terms).ok();
                let by_me = serde_json::from_str::<Value>(&s.bytes).ok().is_some_and(|v| v["by"] == mine);
                by_me && t.is_some_and(|t| key_id_of(&t.caller) == key.id())
            })
            .map(|(_, s)| s)
            .collect();
        Ok(json!({"offers": offers}))
    }

    pub(super) fn pact_fetch(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let Peer { peer } = parse(p)?;
        let peer_key = self.pinned_peer(&peer)?;
        let got = self.pact_send(&peer, "pact_offers", json!({}))?;
        let offers: Vec<Signed> = serde_json::from_value(got["offers"].clone()).map_err(|e| RpcErr::invalid(e.to_string()))?;
        let mut fetched = 0;
        for s in offers {
            let e = pact::open_entry(&s, &peer_key).map_err(RpcErr::invalid)?;
            let Ok(t) = serde_json::from_value::<Terms>(e.data.clone()) else { continue };
            let id = pact::pact_id(&t);
            if e.pact != id || key_id_of(&t.caller) != self.key.public().id() || key_id_of(&t.callee) != peer_key.id() || !pact::check_shape(&t).is_empty() {
                continue;
            }
            if self.pact_row(&id).is_ok() {
                continue;
            }
            let name = self.unique_name(&t.pact, &id);
            self.insert_pact(&id, &name, &peer, "caller", &t)?;
            self.receive(&id, &s, &peer_key)?;
            fetched += 1;
        }
        Ok(json!({"fetched": fetched}))
    }

    pub(super) fn pact_agree(self: &Arc<Self>, p: Value, agree: bool) -> Result<Value, RpcErr> {
        let Which { pact, reason } = parse(p)?;
        let row = self.pact_row(&pact)?;
        if row.state != "offered" {
            return Err(RpcErr::conflict(format!("pact `{}` is {}, not offered", row.name, row.state)));
        }
        let offer_by: String = {
            let db = self.db.lock().unwrap();
            thread_of(&db, &row.id)?.first().map(|(e, _)| e.by.clone()).unwrap_or_default()
        };
        if offer_by == self.key.public().id() {
            return Err(RpcErr::new("not_authorized", 4, "this core made the offer; the other party must agree or decline it"));
        }
        let t = terms_of(&self.db.lock().unwrap(), &row.id)?;
        let (kind, data) = match (agree, row.side.as_str()) {
            (true, "callee") => {
                self.validate_acceptance(&t)?;
                (Kind::Agree, json!({"rehearsal": self.validate_on_callee(&t)?}))
            }
            (true, _) => {
                self.validate_acceptance(&t)?;
                // What the callee showed, kept on the thread for schema checks.
                let seen = self.pact_send(&row.peer, "pact_rehearse", json!({"pact": row.id}))?;
                (Kind::Agree, json!({"rehearsal": seen}))
            }
            (false, _) => (Kind::Decline, json!({"reason": reason})),
        };
        if row.side == "callee" {
            let s = {
                let mut db = self.db.lock().unwrap();
                let tx = db.transaction().map_err(sql)?;
                let s = append_own(&tx, &self.key, &row.id, kind, None, data)?;
                tx.commit().map_err(sql)?;
                s
            };
            self.push_to_peers_async(&row.id, s);
        } else {
            self.caller_append(&row, kind, None, data)?;
        }
        let state = if agree { "agreed" } else { "declined" };
        self.db.lock().unwrap().execute("UPDATE pacts SET state=?2 WHERE id=?1 AND state='offered'", params![row.id, state]).map_err(sql)?;
        if let (true, Some(old)) = (agree, &t.revises) {
            let old = self.pact_row(old)?;
            self.end_pact(&old, json!({"superseded_by": row.id, "reason": "superseded by a revised pact"}))?;
        }
        Ok(json!({"pact": row.name, "id": row.id, "state": state}))
    }

    pub(super) fn pact_list(&self) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut q = db
            .prepare("SELECT p.name, p.id, p.peer, p.side, p.state, (SELECT COUNT(*) FROM pact_calls c WHERE c.pact=p.id AND c.state NOT IN ('accepted','rejected','closed')) FROM pacts p ORDER BY p.created, p.name")
            .map_err(sql)?;
        let rows = q
            .query_map([], |r| {
                Ok(json!({"name": r.get::<_, String>(0)?, "id": r.get::<_, String>(1)?, "peer": r.get::<_, String>(2)?,
                    "side": r.get::<_, String>(3)?, "state": r.get::<_, String>(4)?, "calls_open": r.get::<_, i64>(5)?}))
            })
            .map_err(sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        Ok(Value::Array(rows))
    }

    pub(super) fn pact_show(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let Which { pact, .. } = parse(p)?;
        let row = self.pact_row(&pact)?;
        let (t, thread, forks) = {
            let db = self.db.lock().unwrap();
            let forks: i64 = db.query_row("SELECT COUNT(*) FROM pact_forks WHERE pact=?1", params![row.id], |r| r.get(0)).map_err(sql)?;
            (terms_of(&db, &row.id)?, thread_of(&db, &row.id)?, forks)
        };
        let mut warnings = Vec::new();
        let problem: Option<String> = self.db.lock().unwrap().query_row("SELECT problem FROM pacts WHERE id=?1", params![row.id], |r| r.get(0)).map_err(sql)?;
        warnings.extend(problem);
        match self.pinned_peer(&row.peer) {
            Ok(k) if k.id() == self.other_key(&t)?.id() => {}
            Ok(_) => warnings.push(format!("peer `{}` is now pinned with a different key than this pact names; its entries will be refused", row.peer)),
            Err(e) => warnings.push(e.message),
        }
        let agreed = thread.iter().find(|(e, _)| e.kind == Kind::Agree && !e.data["rehearsal"].is_null()).map(|(e, _)| e.data["rehearsal"].clone());
        let ritual = if row.side == "callee" {
            self.validate_on_callee(&t).unwrap_or_else(|e| json!({"ritual": t.ritual.name, "error": e.message}))
        } else if let Some(r) = agreed {
            r
        } else {
            self.pact_send(&row.peer, "pact_rehearse", json!({"pact": row.id})).unwrap_or_else(|e| json!({"ritual": t.ritual.name, "error": e.message}))
        };
        let acceptance = self.validate_acceptance(&t).unwrap_or_else(|e| json!({"error": e.message}));
        let (calls, used_totals): (Vec<Value>, (u64, u64, u64, u64)) = {
            let db = self.db.lock().unwrap();
            let mut q = db.prepare("SELECT call, state, run, asked_by, deadline FROM pact_calls WHERE pact=?1 ORDER BY call").map_err(sql)?;
            let rows = q
                .query_map(params![row.id], |r| {
                    Ok(json!({"call": r.get::<_, i64>(0)?, "state": r.get::<_, String>(1)?, "run": r.get::<_, Option<String>>(2)?, "asked_by": r.get::<_, Option<String>>(3)?, "deadline": r.get::<_, i64>(4)?}))
                })
                .map_err(sql)?;
            (rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?, used(&db, &row.id)?)
        };
        Ok(json!({
            "name": row.name, "id": row.id, "peer": row.peer, "side": row.side, "state": row.state,
            "terms": t, "entries": thread.len(), "forks": forks, "ritual": ritual, "acceptance": acceptance,
            "used": {"calls": used_totals.0, "requests": used_totals.1, "tokens": used_totals.2, "tool_calls": used_totals.3},
            "calls": calls, "warnings": warnings,
        }))
    }

    // ------------------------------------------------------------ caller side

    /// Move an own entry the callee has stored into the thread, anchored.
    fn confirm_own(&self, pact: &str, s: &Signed) -> Result<(), RpcErr> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction().map_err(sql)?;
        let known: i64 = tx.query_row("SELECT COUNT(*) FROM pact_entries WHERE pact=?1 AND hash=?2", params![pact, s.hash], |r| r.get(0)).map_err(sql)?;
        if known == 0 {
            let mut e = pact::open_entry(s, &self.key.public()).map_err(RpcErr::invalid)?;
            e.hash = s.hash.clone();
            let terms = terms_of(&tx, pact)?;
            let thread: Vec<Entry> = thread_of(&tx, pact)?.into_iter().map(|(x, _)| x).collect();
            pact::check_next(&thread, &e, &terms).map_err(RpcErr::conflict)?;
            insert(&tx, pact, &e, s).map_err(sql)?;
            super::epitaphs::write_pact_epitaph(&tx, &self.key, pact, e.n, s.bytes.as_bytes()).map_err(sql)?;
        }
        tx.execute("DELETE FROM pact_outbox WHERE pact=?1 AND hash=?2", params![pact, s.hash]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(())
    }

    fn local_head(&self, pact: &str) -> Result<(i64, String), RpcErr> {
        let db = self.db.lock().unwrap();
        Ok(thread_of(&db, pact)?.last().map(|(e, _)| (e.n, e.hash.clone())).unwrap_or((0, epitaph::ZEROS.to_string())))
    }

    /// Sign an entry as the caller and hand it to the callee, who orders the
    /// thread. It waits in the outbox until the callee has stored it; if the
    /// callee's head moved, it is signed again on top.
    pub(super) fn caller_append(self: &Arc<Self>, row: &Row, kind: Kind, call: Option<i64>, data: Value) -> Result<Signed, RpcErr> {
        static OUTBOX: Mutex<()> = Mutex::new(());
        let _guard = OUTBOX.lock().unwrap();
        let waiting: i64 = self.db.lock().unwrap().query_row("SELECT COUNT(*) FROM pact_outbox WHERE pact=?1", params![row.id], |r| r.get(0)).map_err(sql)?;
        let initial = if waiting == 0 {
            let (n, prev) = self.local_head(&row.id)?;
            pact::seal_entry(&self.key, &row.id, n + 1, &prev, kind, call, now_ms(), data.clone())
        } else {
            Signed { bytes: String::new(), sig: String::new(), hash: String::new() }
        };
        let ord = {
            let db = self.db.lock().unwrap();
            db.execute(
                "INSERT INTO pact_outbox (pact, kind, call, data, bytes, sig, hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![row.id, serde_json::to_string(&kind).unwrap(), call, data.to_string(), initial.bytes, initial.sig, initial.hash],
            )
            .map_err(sql)?;
            db.last_insert_rowid()
        };
        self.flush(row)?;
        let still: i64 = self.db.lock().unwrap().query_row("SELECT COUNT(*) FROM pact_outbox WHERE pact=?1 AND ord=?2", params![row.id, ord], |r| r.get(0)).map_err(sql)?;
        if still > 0 {
            return Err(RpcErr::new("unavailable", 8, format!("the entry is signed and waiting to reach peer `{}`; it will be sent on the next poll", row.peer)));
        }
        let db = self.db.lock().unwrap();
        thread_of(&db, &row.id)?.last().map(|(_, s)| s.clone()).ok_or_else(|| RpcErr::conflict("entry was not stored"))
    }

    /// Send waiting own entries in order.
    fn flush(self: &Arc<Self>, row: &Row) -> Result<(), RpcErr> {
        for _ in 0..8 {
            let next: Option<(i64, String, Option<i64>, String, Signed)> = self
                .db
                .lock()
                .unwrap()
                .query_row("SELECT ord, kind, call, data, bytes, sig, hash FROM pact_outbox WHERE pact=?1 ORDER BY ord LIMIT 1", params![row.id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, Signed { bytes: r.get(4)?, sig: r.get(5)?, hash: r.get(6)? }))
                })
                .optional()
                .map_err(sql)?;
            let Some((ord, kind, call, data, mut s)) = next else { return Ok(()) };
            let (head_n, head_prev) = self.local_head(&row.id)?;
            let matches_head = !s.bytes.is_empty()
                && serde_json::from_str::<Entry>(&s.bytes)
                    .ok()
                    .is_some_and(|e| e.n == head_n + 1 && e.prev == head_prev);
            if !matches_head {
                let kind_val: Kind = serde_json::from_str(&kind).map_err(|e| RpcErr::invalid(e.to_string()))?;
                let data_val: Value = serde_json::from_str(&data).map_err(|e| RpcErr::invalid(e.to_string()))?;
                s = pact::seal_entry(&self.key, &row.id, head_n + 1, &head_prev, kind_val, call, now_ms(), data_val);
                self.db
                    .lock()
                    .unwrap()
                    .execute("UPDATE pact_outbox SET bytes=?2, sig=?3, hash=?4 WHERE ord=?1", params![ord, s.bytes, s.sig, s.hash])
                    .map_err(sql)?;
            }
            match self.pact_send(&row.peer, "pact_post", json!({"pact": row.id, "entry": s})) {
                Ok(_) => self.confirm_own(&row.id, &s)?,
                Err(e) if e.code == "state_conflict" && !e.data["entries"].is_null() => {
                    // Take the callee's newer entries, then sign again on top on the next loop iteration.
                    self.take_entries(row, &e.data["entries"])?;
                }
                Err(e) if e.exit == 8 => return Err(e),
                Err(e) => {
                    // The callee refused it outright; it will never be stored.
                    self.db.lock().unwrap().execute("DELETE FROM pact_outbox WHERE ord=?1", params![ord]).map_err(sql)?;
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Store entries the callee holds that this core lacks.
    fn take_entries(self: &Arc<Self>, row: &Row, entries: &Value) -> Result<(), RpcErr> {
        let list: Vec<Signed> = serde_json::from_value(entries.clone()).map_err(|e| RpcErr::invalid(e.to_string()))?;
        let t = terms_of(&self.db.lock().unwrap(), &row.id)?;
        let other = self.other_key(&t)?;
        let me = self.key.public().id();
        let (head, _) = self.local_head(&row.id)?;
        for s in list {
            let parsed: Value = serde_json::from_str(&s.bytes).unwrap_or_default();
            let n = parsed["n"].as_i64().unwrap_or(0);
            if parsed["by"] == me {
                if n > head {
                    self.confirm_own(&row.id, &s)?;
                }
                continue;
            }
            let _ = pact::open_entry(&s, &other).map_err(|why| RpcErr::new("not_authorized", 4, format!("pact entry refused: {why}")))?;
            self.receive(&row.id, &s, &other)?;
        }
        Ok(())
    }

    /// Bring one caller-side pact up to date with the callee.
    pub(super) fn sync_pact(self: &Arc<Self>, row: &Row) -> Result<(), RpcErr> {
        let (after, _) = self.local_head(&row.id)?;
        let got = self.pact_send(&row.peer, "pact_sync", json!({"pact": row.id, "after": after}))?;
        self.take_entries(row, &got["entries"])?;
        self.flush(row)
    }

    /// Sync every open caller-side pact. Errors are reported per pact.
    pub(super) fn pact_poll(self: &Arc<Self>) -> Result<Value, RpcErr> {
        let rows: Vec<Row> = {
            let db = self.db.lock().unwrap();
            let mut q = db.prepare("SELECT id, name, peer, side, state FROM pacts WHERE side='caller' AND state IN ('offered','agreed','superseded')").map_err(sql)?;
            let rows = q
                .query_map([], |r| Ok(Row { id: r.get(0)?, name: r.get(1)?, peer: r.get(2)?, side: r.get(3)?, state: r.get(4)? }))
                .map_err(sql)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?
        };
        let mut errors = Vec::new();
        for row in &rows {
            if let Err(e) = self.sync_pact(row) {
                errors.push(json!({"pact": row.name, "error": e.message}));
            }
        }
        Ok(json!({"synced": rows.len() - errors.len(), "errors": errors}))
    }
}

// --------------------------------------------------------------------- calls

/// One call at a time picks its number on the caller side.
static CALLING: Mutex<()> = Mutex::new(());

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallArgs {
    pact: String,
    #[serde(default)]
    args: Map<String, Value>,
    #[serde(default)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallRef {
    pact: String,
    call: i64,
    #[serde(default)]
    wait_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Limit {
    pact: String,
    limits: crate::ritual::Essence,
}

fn lower_u(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// The stricter of two sets of limits, field by field.
pub(super) fn stricter(a: &crate::ritual::Essence, b: &crate::ritual::Essence) -> crate::ritual::Essence {
    let ms = |t: &Option<String>| t.as_deref().and_then(|t| crate::util::parse_duration(t).ok());
    let time = match (ms(&a.time), ms(&b.time)) {
        (Some(x), Some(y)) => if x <= y { a.time.clone() } else { b.time.clone() },
        (Some(_), None) => a.time.clone(),
        _ => b.time.clone(),
    };
    crate::ritual::Essence {
        tool_calls: lower_u(a.tool_calls, b.tool_calls),
        items: lower_u(a.items, b.items),
        requests: lower_u(a.requests, b.requests),
        tokens: lower_u(a.tokens, b.tokens),
        time,
    }
}

/// Usage counted so far under a pact, from recorded calls.
fn used(conn: &rusqlite::Connection, pact: &str) -> Result<(u64, u64, u64, u64), RpcErr> {
    let mut q = conn.prepare("SELECT usage FROM pact_calls WHERE pact=?1").map_err(sql)?;
    let rows = q.query_map(params![pact], |r| r.get::<_, Option<String>>(0)).map_err(sql)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?;
    let (mut calls, mut requests, mut tokens, mut tool_calls) = (0u64, 0u64, 0u64, 0u64);
    for u in rows {
        calls = calls.saturating_add(1);
        let u: Value = u.and_then(|u| serde_json::from_str(&u).ok()).unwrap_or_default();
        requests = requests.saturating_add(u["requests"].as_u64().unwrap_or(0));
        tokens = tokens.saturating_add(u["tokens"].as_u64().unwrap_or(0));
        tool_calls = tool_calls.saturating_add(u["tool_calls"].as_u64().unwrap_or(0));
    }
    Ok((calls, requests, tokens, tool_calls))
}

/// Why another call would exceed the pact's totals, if it would.
fn over_totals(t: &Terms, (calls, requests, tokens, tool_calls): (u64, u64, u64, u64)) -> Option<String> {
    let total = &t.ceilings.total;
    if total.calls.is_some_and(|c| calls >= c) {
        return Some(format!("the pact's ceiling of {} calls is reached", total.calls.unwrap()));
    }
    if total.requests.is_some_and(|c| requests >= c) {
        return Some(format!("the pact's ceiling of {} model requests is reached", total.requests.unwrap()));
    }
    if total.tokens.is_some_and(|c| tokens >= c) {
        return Some(format!("the pact's ceiling of {} tokens is reached", total.tokens.unwrap()));
    }
    if total.tool_calls.is_some_and(|c| tool_calls >= c) {
        return Some(format!("the pact's ceiling of {} tool calls is reached", total.tool_calls.unwrap()));
    }
    None
}

fn expired(t: &Terms) -> bool {
    pact::expires_ms(&t.expires).map(|e| now_ms() > e).unwrap_or(true)
}

impl Core {
    /// Make a call under a pact this core is the caller of, as `caller`
    /// (empty for the owner, or `familiar:NAME`).
    pub(super) fn pact_call_as(self: &Arc<Self>, caller: &str, p: Value) -> Result<Value, RpcErr> {
        let CallArgs { pact, args, idempotency_key } = parse(p)?;
        let row = self.pact_row(&pact)?;
        if row.side != "caller" {
            return Err(RpcErr::invalid(format!("this core is the callee of `{}`; only the caller makes calls", row.name)));
        }
        if row.state != "agreed" {
            return Err(RpcErr::conflict(format!("pact `{}` is {}, not agreed", row.name, row.state)));
        }
        let t = terms_of(&self.db.lock().unwrap(), &row.id)?;
        if expired(&t) {
            return Err(RpcErr::conflict(format!("pact `{}` expired on {}", row.name, t.expires)));
        }
        for k in args.keys() {
            if t.fixed.contains_key(k) {
                return Err(RpcErr::invalid(format!("input `{k}` is fixed by the pact; a call cannot change it")));
            }
            if !t.inputs.contains(k) {
                return Err(RpcErr::invalid(format!("input `{k}` is not one this pact lets a call give (it allows {:?})", t.inputs)));
            }
        }
        let _one = CALLING.lock().unwrap();
        if let Some(key) = &idempotency_key {
            let prior: Option<i64> = self
                .db
                .lock()
                .unwrap()
                .query_row("SELECT call FROM pact_calls WHERE pact=?1 AND asked_by=?2 AND idem=?3", params![row.id, caller, key], |r| r.get(0))
                .optional()
                .map_err(sql)?;
            if let Some(call) = prior {
                return Ok(json!({"pact": row.name, "call": call, "reused": true}));
            }
        }
        let call = {
            let db = self.db.lock().unwrap();
            if let Some(why) = over_totals(&t, used(&db, &row.id)?) {
                return Err(RpcErr::new("exhausted", 10, why));
            }
            let call: i64 = db.query_row("SELECT COALESCE(MAX(call), 0) + 1 FROM pact_calls WHERE pact=?1", params![row.id], |r| r.get(0)).map_err(sql)?;
            let deadline = now_ms().saturating_add(crate::util::parse_duration(&t.ceilings.deadline).unwrap_or(0).min(i64::MAX as u64) as i64);
            db.execute(
                "INSERT INTO pact_calls (pact, call, state, asked_by, deadline, idem) VALUES (?1, ?2, 'sent', ?3, ?4, ?5)",
                params![row.id, call, caller, deadline, idempotency_key],
            )
            .map_err(sql)?;
            call
        };
        let data = json!({"inputs": args, "idempotency_key": idempotency_key, "asked_by": caller});
        match self.caller_append(&row, Kind::Call, Some(call), data) {
            Ok(_) => Ok(json!({"pact": row.name, "call": call, "state": "sent"})),
            Err(e) if e.exit == 8 => Ok(json!({"pact": row.name, "call": call, "state": "queued", "note": e.message})),
            Err(e) => {
                self.db.lock().unwrap().execute("DELETE FROM pact_calls WHERE pact=?1 AND call=?2", params![row.id, call]).map_err(sql)?;
                Err(e)
            }
        }
    }

    /// A call's state and its entries; a familiar sees only its own calls.
    pub(super) fn pact_call_status(self: &Arc<Self>, caller: &str, p: Value) -> Result<Value, RpcErr> {
        let CallRef { pact, call, wait_ms } = parse(p)?;
        let row = self.pact_row(&pact)?;
        let deadline = Instant::now() + Duration::from_millis(wait_ms.unwrap_or(0).min(300_000));
        loop {
            if row.side == "caller" && wait_ms.is_some() {
                let _ = self.sync_pact(&row);
            }
            let status: Option<(String, Option<String>, Option<String>)> = self
                .db
                .lock()
                .unwrap()
                .query_row("SELECT state, asked_by, run FROM pact_calls WHERE pact=?1 AND call=?2", params![row.id, call], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()
                .map_err(sql)?;
            let Some((state, asked_by, run)) = status.filter(|(_, by, _)| caller.is_empty() || by.as_deref() == Some(caller)) else {
                return Err(RpcErr::not_found(format!("no call {call} under `{}`", row.name)));
            };
            let final_state = matches!(state.as_str(), "accepted" | "rejected" | "closed");
            if final_state || Instant::now() >= deadline {
                let entries: Vec<Value> = thread_of(&self.db.lock().unwrap(), &row.id)?
                    .into_iter()
                    .filter(|(e, _)| e.call == Some(call))
                    .map(|(e, _)| json!({"n": e.n, "kind": e.kind, "by": e.by, "at": e.at, "data": e.data}))
                    .collect();
                return Ok(json!({"pact": row.name, "call": call, "state": state, "asked_by": asked_by, "run": run, "entries": entries}));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Tighten this side's per-call limits for a pact it is the callee of.
    pub(super) fn pact_limit(&self, p: Value) -> Result<Value, RpcErr> {
        let Limit { pact, limits } = parse(p)?;
        let row = self.pact_row(&pact)?;
        if row.side != "callee" {
            return Err(RpcErr::invalid("only the callee sets local limits; the caller's limits are the pact's"));
        }
        let t = terms_of(&self.db.lock().unwrap(), &row.id)?;
        let per = &t.ceilings.per_call;
        let ms = |t: &Option<String>| t.as_deref().and_then(|t| crate::util::parse_duration(t).ok());
        let looser = |mine: Option<u64>, theirs: Option<u64>| matches!((mine, theirs), (Some(m), Some(p)) if m > p);
        if looser(limits.tool_calls, per.tool_calls)
            || looser(limits.items, per.items)
            || looser(limits.requests, per.requests)
            || looser(limits.tokens, per.tokens)
            || looser(ms(&limits.time), ms(&per.time))
        {
            return Err(RpcErr::invalid("local limits may only tighten the pact's per-call ceilings; loosening needs a revised pact"));
        }
        self.db.lock().unwrap().execute("UPDATE pacts SET local_limits=?2 WHERE id=?1", params![row.id, serde_json::to_string(&limits).unwrap()]).map_err(sql)?;
        Ok(json!({"pact": row.name, "limits": limits}))
    }

    /// Sign a delivery for `call` and record the call's new state.
    pub(super) fn deliver(self: &Arc<Self>, pact: &str, call: i64, data: Value) -> Result<(), RpcErr> {
        let closed = data["outcome"] != "returned" && data["held"].as_array().is_none_or(|h| h.is_empty());
        let s = {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction().map_err(sql)?;
            let s = append_own(&tx, &self.key, pact, Kind::Deliver, Some(call), data)?;
            tx.execute("UPDATE pact_calls SET state=?3 WHERE pact=?1 AND call=?2", params![pact, call, if closed { "closed" } else { "delivered" }]).map_err(sql)?;
            tx.commit().map_err(sql)?;
            s
        };
        self.push_to_peers_async(pact, s);
        Ok(())
    }

    /// The callee starts the pinned ritual for a call it has just stored.
    fn start_call(self: &Arc<Self>, pact: &str, e: &Entry) -> Result<(), RpcErr> {
        let call = e.call.ok_or_else(|| RpcErr::invalid("a call entry needs a call number"))?;
        let row = self.pact_row(pact)?;
        let t = terms_of(&self.db.lock().unwrap(), pact)?;
        let deadline = now_ms().saturating_add(crate::util::parse_duration(&t.ceilings.deadline).unwrap_or(0).min(i64::MAX as u64) as i64);
        let refused = {
            let db = self.db.lock().unwrap();
            let so_far = used(&db, pact)?;
            db.execute(
                "INSERT OR IGNORE INTO pact_calls (pact, call, state, asked_by, deadline) VALUES (?1, ?2, 'running', ?3, ?4)",
                params![pact, call, e.data["asked_by"].as_str(), deadline],
            )
            .map_err(sql)?;
            let inputs = e.data["inputs"].as_object().cloned().unwrap_or_default();
            if row.state != "agreed" {
                Some(format!("the pact is {}, not agreed", row.state))
            } else if expired(&t) {
                Some(format!("the pact expired on {}", t.expires))
            } else if let Some(why) = over_totals(&t, so_far) {
                Some(why)
            } else {
                inputs.keys().find(|k| !t.inputs.contains(k)).map(|k| format!("input `{k}` is not one this pact lets a call give"))
            }
        };
        if let Some(why) = refused {
            return self.deliver(pact, call, json!({"outcome": "refused", "error": why}));
        }
        let local: crate::ritual::Essence = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT local_limits FROM pacts WHERE id=?1", params![pact], |r| r.get::<_, Option<String>>(0))
            .map_err(sql)?
            .and_then(|l| serde_json::from_str(&l).ok())
            .unwrap_or_default();
        let version: Option<u32> = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT version FROM rituals WHERE name=?1 AND hash=?2", params![t.ritual.name, t.ritual.hash], |r| r.get(0))
            .optional()
            .map_err(sql)?;
        let Some(version) = version else {
            return self.deliver(pact, call, json!({"outcome": "refused", "error": "the pinned ritual version is no longer saved here"}));
        };
        let mut args = t.fixed.clone();
        args.extend(e.data["inputs"].as_object().cloned().unwrap_or_default());
        let spec = json!({
            "target": t.ritual.name, "version": version, "args": args, "caller": format!("pact:{}", row.name),
            "expected_hash": t.ritual.hash, "limits": stricter(&t.ceilings.per_call, &local), "cwd": "/",
            // Ties the run to this call before it starts, so a hold inside it
            // can find its pact; also makes a repeated call entry start nothing new.
            "idempotency_key": format!("pact:{pact}:{call}"),
        });
        match self.invoke(spec) {
            Ok(out) => {
                self.db
                    .lock()
                    .unwrap()
                    .execute("UPDATE pact_calls SET run=?3 WHERE pact=?1 AND call=?2", params![pact, call, out["summon"].as_str()])
                    .map_err(sql)?;
                Ok(())
            }
            Err(err) => self.deliver(pact, call, json!({"outcome": "refused", "error": err.message})),
        }
    }
}

// --------------------------------------------------------------------- holds

/// What a petition filed under a pact call carries.
#[derive(Clone, Debug)]
pub(crate) struct PactTag {
    pub pact: String,
    pub call: i64,
    pub expires: i64,
}

/// Who must decide a pact petition.
#[derive(Clone, Debug)]
pub(crate) struct PactPart {
    pub pact: String,
    pub call: i64,
    pub needs_callee: bool,
    pub needs_caller: bool,
}

impl Core {
    /// For a step of a run started under a pact call: the call's tag, and
    /// (for the pinned ritual's own steps) the caller hold's condition.
    pub(crate) fn pact_hold_for(&self, run: &str, depth: u32, step: &str) -> Option<(PactTag, Option<Value>)> {
        let db = self.db.lock().unwrap();
        let mut root = run.to_string();
        for _ in 0..64 {
            let parent: Option<String> = db.query_row("SELECT parent FROM summons WHERE id=?1", params![root], |r| r.get(0)).optional().ok().flatten()?;
            match parent {
                Some(p) => root = p,
                None => break,
            }
        }
        let key: String = db
            .query_row("SELECT key FROM invocations WHERE summon=?1 AND caller LIKE 'pact:%' AND key LIKE 'pact:%'", params![root], |r| r.get(0))
            .optional()
            .ok()
            .flatten()?;
        let mut parts = key.splitn(3, ':').skip(1);
        let (pact, call) = (parts.next()?.to_string(), parts.next()?.parse::<i64>().ok()?);
        let expires: i64 = db.query_row("SELECT deadline FROM pact_calls WHERE pact=?1 AND call=?2", params![pact, call], |r| r.get(0)).ok()?;
        let terms = terms_of(&db, &pact).ok()?;
        let when = if depth == 0 { terms.held_by_caller.iter().find(|h| h.step == step).map(|h| h.when.clone()) } else { None };
        Some((PactTag { pact, call, expires }, when))
    }

    pub(crate) fn pact_part(&self, petition: &str) -> Result<Option<PactPart>, RpcErr> {
        #[allow(clippy::type_complexity)]
        let row: Option<(Option<String>, Option<i64>, bool, bool)> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT pact, pact_call, needs_callee, needs_caller FROM petitions WHERE id=?1",
                params![petition],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(sql)?;
        Ok(match row {
            Some((Some(pact), Some(call), needs_callee, needs_caller)) => {
                Some(PactPart { pact, call, needs_callee, needs_caller })
            }
            _ => None,
        })
    }

    /// Callee: the caller's signed decision on a held action.
    fn apply_caller_decision(self: &Arc<Self>, pact: &str, e: &Entry) -> Result<(), RpcErr> {
        self.expire_due(now_ms());
        let id = e.data["petition"].as_str().unwrap_or("").to_string();
        let permit = e.data["permit"].as_bool().unwrap_or(false);
        let part = self.pact_part(&id)?;
        let ours = part.as_ref().is_some_and(|p| p.pact == pact && Some(p.call) == e.call && p.needs_caller);
        let status: Option<String> = self.db.lock().unwrap().query_row("SELECT status FROM petitions WHERE id=?1", params![id], |r| r.get(0)).optional().map_err(sql)?;
        if !ours || status.as_deref() != Some("pending") {
            // Too late (expired or already decided) or not this pact's: the
            // decision stays on the thread, and nothing is done.
            return Ok(());
        }
        let decide = requests::Decide { permit, all: false, ids: vec![id], ritual: None };
        self.petition_decide_as(decide, true).map(|_| ())
    }

    /// Caller: sign the owner's decision on a callee's held action and send it.
    pub(super) fn relay_decision(self: &Arc<Self>, pet: &Petition, permit: bool) -> Result<Value, RpcErr> {
        let pact = pet.arguments["pact"].as_str().unwrap_or("").to_string();
        let call = pet.arguments["call"].as_i64();
        let row = self.pact_row(&pact)?;
        let n = self
            .db
            .lock()
            .unwrap()
            .execute("UPDATE petitions SET status='executing' WHERE id=?1 AND status='pending'", params![pet.id])
            .map_err(sql)?;
        if n != 1 {
            return Err(RpcErr::conflict("petition already claimed; nothing was decided"));
        }
        let data = json!({"petition": pet.arguments["petition"], "permit": permit, "decided_by": "owner"});
        let sent = match self.caller_append(&row, Kind::Decide, call, data) {
            Ok(s) => json!({"sent": s.hash}),
            Err(e) if e.exit == 8 => json!({"queued": e.message}),
            Err(e) => {
                self.db.lock().unwrap().execute("UPDATE petitions SET status='pending' WHERE id=?1", params![pet.id]).map_err(sql)?;
                return Err(e);
            }
        };
        let status = if permit { "applied" } else { "denied" };
        self.db
            .lock()
            .unwrap()
            .execute("UPDATE petitions SET status=?2, decided=?3, result=?4 WHERE id=?1", params![pet.id, status, now_ms(), sent.to_string()])
            .map_err(sql)?;
        Ok(json!({"id": pet.id, "status": status, "operation": pet.operation, "arguments": pet.arguments, "result": sent}))
    }

    /// Callee: once a pact petition is final, sign a settle entry for it.
    pub(super) fn settle_if_pact(&self, petition: &str) -> Result<(), RpcErr> {
        let Some(part) = self.pact_part(petition)? else { return Ok(()) };
        let (status, result): (String, Option<String>) =
            self.db.lock().unwrap().query_row("SELECT status, result FROM petitions WHERE id=?1", params![petition], |r| Ok((r.get(0)?, r.get(1)?))).map_err(sql)?;
        if matches!(status.as_str(), "pending" | "executing") {
            return Ok(());
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction().map_err(sql)?;
        let delivered: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM pact_entries WHERE pact=?1 AND json_extract(bytes, '$.kind')='deliver' AND json_extract(bytes, '$.call')=?2",
                params![part.pact, part.call],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if delivered == 0 {
            return Ok(());
        }
        let already: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM pact_entries WHERE pact=?1 AND json_extract(bytes, '$.kind')='settle' AND json_extract(bytes, '$.call')=?2 AND json_extract(bytes, '$.data.petition')=?3",
                params![part.pact, part.call, petition],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if already > 0 {
            return Ok(());
        }
        let data = json!({"petition": petition, "state": status, "result_fp": sha256_hex(result.unwrap_or_default().as_bytes())});
        append_own(&tx, &self.key, &part.pact, Kind::Settle, Some(part.call), data)?;
        let open: i64 = tx
            .query_row("SELECT COUNT(*) FROM petitions WHERE pact=?1 AND pact_call=?2 AND status IN ('pending','executing')", params![part.pact, part.call], |r| r.get(0))
            .map_err(sql)?;
        if open == 0 {
            let unapplied: i64 = tx
                .query_row("SELECT COUNT(*) FROM petitions WHERE pact=?1 AND pact_call=?2 AND status != 'applied'", params![part.pact, part.call], |r| r.get(0))
                .map_err(sql)?;
            let returned: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM pact_entries WHERE pact=?1 AND json_extract(bytes, '$.kind')='deliver' AND json_extract(bytes, '$.call')=?2 AND json_extract(bytes, '$.data.outcome')='returned'",
                    params![part.pact, part.call],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            let next_state = if unapplied == 0 && returned > 0 { "settled" } else { "closed" };
            tx.execute("UPDATE pact_calls SET state=?3 WHERE pact=?1 AND call=?2 AND state='delivered'", params![part.pact, part.call, next_state]).map_err(sql)?;
        }
        tx.commit().map_err(sql)
    }

    /// Callee: any pending pact petition past its call deadline expires; nothing is performed.
    pub(crate) fn expire_due(&self, now: i64) {
        let due: Vec<String> = {
            let db = self.db.lock().unwrap();
            let Ok(mut q) = db.prepare(
                "SELECT id FROM petitions WHERE pact IS NOT NULL AND status='pending' AND expires IS NOT NULL AND expires < ?1",
            ) else {
                return;
            };
            let Ok(rows) = q.query_map(params![now], |r| r.get(0)) else { return };
            rows.filter_map(Result::ok).collect()
        };
        for id in due {
            let n = self.db.lock().unwrap().execute(
                "UPDATE petitions SET status='expired', decided=?2, result=?3 WHERE id=?1 AND status='pending'",
                params![id, now, json!({"error": "the pact's deadline passed before the caller decided; nothing was done"}).to_string()],
            );
            if n == Ok(1) {
                if let Err(e) = self.settle_if_pact(&id) {
                    eprintln!("pact: cannot settle expired petition {id}: {}", e.message);
                }
            }
        }
    }

    /// Callee: a run started for a call has ended; deliver it.
    pub(crate) fn on_run_end(self: &Arc<Self>, run: &str) {
        let call: Option<(String, i64)> =
            self.db.lock().unwrap().query_row("SELECT pact, call FROM pact_calls WHERE run=?1", params![run], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok().flatten();
        // A run can end before its call row learns the run id; the
        // idempotency key ties them in that case.
        let call = call.or_else(|| {
            let key: String = self.db.lock().unwrap().query_row("SELECT key FROM invocations WHERE summon=?1 AND key LIKE 'pact:%'", params![run], |r| r.get(0)).ok()?;
            let mut parts = key.splitn(3, ':').skip(1);
            Some((parts.next()?.to_string(), parts.next()?.parse().ok()?))
        });
        let Some((pact, call)) = call else { return };
        if let Err(e) = self.deliver_run(&pact, call, run) {
            eprintln!("pact: cannot deliver call {call} of {pact}: {}", e.message);
        }
    }

    fn deliver_run(self: &Arc<Self>, pact: &str, call: i64, run: &str) -> Result<(), RpcErr> {
        let delivered: i64 = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM pact_entries WHERE pact=?1 AND by=?2 AND json_extract(bytes, '$.kind')='deliver' AND json_extract(bytes, '$.call')=?3",
                params![pact, self.key.public().id(), call],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if delivered > 0 {
            return Ok(());
        }
        let (outcome, result, error, requests, tokens, tool_calls): (Option<String>, Option<String>, Option<String>, i64, i64, i64) = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT outcome, result, error, requests, tokens, (SELECT COUNT(*) FROM events WHERE summon=?1 AND kind LIKE 'tool.%') FROM summons WHERE id=?1",
                params![run],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .map_err(sql)?;
        let epitaph: Option<(String, String, String)> =
            self.db.lock().unwrap().query_row("SELECT link, body, sig FROM epitaphs WHERE run=?1", params![run], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(sql)?;
        let held: Vec<Value> = {
            let db = self.db.lock().unwrap();
            let mut q = db
                .prepare("SELECT id, step, operation, arguments, needs_callee, needs_caller FROM petitions WHERE pact=?1 AND pact_call=?2 ORDER BY created")
                .map_err(sql)?;
            let rows = q
                .query_map(params![pact, call], |r| {
                    Ok(json!({"petition": r.get::<_, String>(0)?, "step": r.get::<_, String>(1)?, "operation": r.get::<_, String>(2)?,
                        "arguments": serde_json::from_str::<Value>(&r.get::<_, String>(3)?).unwrap_or_default(),
                        "needs_callee": r.get::<_, bool>(4)?, "needs_caller": r.get::<_, bool>(5)?}))
                })
                .map_err(sql)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?
        };
        let outcome = match outcome.as_deref() {
            Some("returned" | "abstained") => "returned",
            Some("exhausted") => "exhausted",
            _ => "failed",
        };
        let usage = json!({"requests": requests, "tokens": tokens, "tool_calls": tool_calls});
        self.db.lock().unwrap().execute("UPDATE pact_calls SET usage=?3 WHERE pact=?1 AND call=?2", params![pact, call, usage.to_string()]).map_err(sql)?;
        let parse = |t: Option<String>| t.and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(Value::Null);
        self.deliver(pact, call, json!({
            "outcome": outcome, "result": parse(result), "error": parse(error), "run": run, "usage": usage, "held": held,
            "epitaph": epitaph.map(|(link, body, sig)| json!({"link": link, "body": body, "sig": sig})),
        }))?;
        // Holds decided or expired while the run was still going settle now.
        let ids: Vec<String> = held.iter().filter_map(|h| h["petition"].as_str().map(str::to_string)).collect();
        for id in ids {
            self.settle_if_pact(&id)?;
        }
        Ok(())
    }

    /// Caller: a delivery arrived. Record usage, and raise a local petition
    /// for every held action this side must decide.
    fn on_delivered(self: &Arc<Self>, pact: &str, e: &Entry) -> Result<(), RpcErr> {
        let call = e.call.unwrap_or(0);
        let row = self.pact_row(pact)?;
        if row.side != "caller" {
            return Ok(());
        }
        let held = e.data["held"].as_array().cloned().unwrap_or_default();
        let closed = e.data["outcome"] != "returned" && held.is_empty();
        let state = if closed {
            "closed"
        } else if held.iter().any(|h| h["needs_caller"] == true) {
            "awaiting_decision"
        } else if held.is_empty() {
            "settled"
        } else {
            "delivered"
        };
        let db = self.db.lock().unwrap();
        db.execute("UPDATE pact_calls SET state=?3, usage=?4 WHERE pact=?1 AND call=?2", params![pact, call, state, e.data["usage"].to_string()]).map_err(sql)?;
        for h in held.iter().filter(|h| h["needs_caller"] == true) {
            let args = json!({"pact": pact, "call": call, "petition": h["petition"], "step": h["step"], "operation": h["operation"], "arguments": h["arguments"]});
            db.execute(
                "INSERT INTO petitions (id, summon, ritual, step, operation, effect, arguments, selection, because, status, created)
                 VALUES (?1, ?2, ?3, ?4, 'pact.decide', 'write', ?5, '[]', ?6, 'pending', ?7)",
                params![new_id("p"), format!("pact:{pact}:{call}"), row.name, h["step"].as_str().unwrap_or(""), args.to_string(),
                    json!({"pact": row.name, "call": call, "result": e.data["result"]}).to_string(), now_ms()],
            )
            .map_err(sql)?;
        }
        drop(db);
        if state == "settled" {
            self.maybe_accept(pact, call)?;
        }
        Ok(())
    }

    /// Caller: a held action settled on the callee.
    fn on_settled(self: &Arc<Self>, pact: &str, e: &Entry) -> Result<(), RpcErr> {
        let row = self.pact_row(pact)?;
        if row.side != "caller" {
            return Ok(());
        }
        let theirs = e.data["petition"].as_str().unwrap_or("");
        let state = e.data["state"].as_str().unwrap_or("");
        let db = self.db.lock().unwrap();
        if !state.is_empty() {
            db.execute(
                "UPDATE petitions SET status=?3, decided=?4 WHERE operation='pact.decide' AND status='pending' AND json_extract(arguments, '$.petition')=?1 AND json_extract(arguments, '$.pact')=?2",
                params![theirs, pact, state, now_ms()],
            )
            .map_err(sql)?;
        }
        // Settled once every held action in the delivery has a settle entry.
        let call = e.call.unwrap_or(0);
        let thread = thread_of(&db, pact)?;
        let held = thread.iter().find(|(x, _)| x.kind == Kind::Deliver && x.call == Some(call)).map(|(x, _)| x.data["held"].as_array().map(|h| h.len()).unwrap_or(0)).unwrap_or(0);
        let settled = thread.iter().filter(|(x, _)| x.kind == Kind::Settle && x.call == Some(call)).count();
        if settled >= held {
            let returned = thread.iter().any(|(x, _)| x.kind == Kind::Deliver && x.call == Some(call) && x.data["outcome"] == "returned");
            let all_applied = thread
                .iter()
                .filter(|(x, _)| x.kind == Kind::Settle && x.call == Some(call))
                .all(|(x, _)| x.data["state"] == "applied");
            let next = if returned && all_applied { "settled" } else { "closed" };
            db.execute("UPDATE pact_calls SET state=?3 WHERE pact=?1 AND call=?2 AND state IN ('delivered','awaiting_decision')", params![pact, call, next]).map_err(sql)?;
            drop(db);
            if returned && all_applied {
                self.maybe_accept(pact, call)?;
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ polling

/// The next wait between polls: back to 15 s after a change, doubling to at
/// most 5 min while nothing changes.
pub(crate) fn next_poll_ms(current: u64, changed: bool) -> u64 {
    if changed {
        15_000
    } else {
        (current * 2).clamp(15_000, 300_000)
    }
}

impl Core {
    /// While serving: sync caller-side pacts with open calls or waiting
    /// entries, and expire caller holds that are past their deadline.
    pub(super) fn pact_poller(self: Arc<Self>) {
        let mut wait = 15_000u64;
        while !self.stop.load(Ordering::SeqCst) {
            let until = Instant::now() + Duration::from_millis(wait);
            while Instant::now() < until && !self.stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(200));
            }
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            self.expire_due(now_ms());
            let count = |db: &rusqlite::Connection| db.query_row("SELECT COUNT(*) FROM pact_entries", [], |r| r.get::<_, i64>(0)).unwrap_or(0);
            let busy: i64 = self
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM pact_outbox) + (SELECT COUNT(*) FROM pacts p WHERE p.side='caller' AND (p.state='offered'
                       OR EXISTS (SELECT 1 FROM pact_calls c WHERE c.pact=p.id AND c.state NOT IN ('accepted','rejected','closed'))))",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if busy == 0 {
                wait = next_poll_ms(wait, false);
                continue;
            }
            let before = count(&self.db.lock().unwrap());
            let _ = self.pact_poll();
            let after = count(&self.db.lock().unwrap());
            wait = next_poll_ms(wait, after != before);
        }
    }
}

// --------------------------------------------------------------- acceptance

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideDelivery {
    pact: String,
    call: i64,
    #[serde(default)]
    reason: Option<String>,
}

fn short(text: &str) -> String {
    if text.chars().count() > 500 { format!("{}…", text.chars().take(500).collect::<String>()) } else { text.to_string() }
}

impl Core {
    /// The pact's checks on a delivered result: the ritual's return schema,
    /// then the acceptance ritual. Never fails; a problem is a "no".
    pub(crate) fn acceptance_checks(self: &Arc<Self>, t: &Terms, schema: Option<&Value>, result: &Value) -> Value {
        let mut out = json!({"accept": true, "reason": "no checks are set", "schema": null, "ritual": null});
        if t.checks.schema {
            let checked = match schema {
                Some(s) => crate::ritual::check_schema(s, result, "result"),
                None => Err("the ritual declares no return schema to check against".into()),
            };
            match checked {
                Ok(()) => {
                    out["schema"] = json!({"ok": true});
                    out["reason"] = json!("the result matches the ritual's return schema");
                }
                Err(e) => {
                    out["schema"] = json!({"ok": false, "error": short(&e)});
                    out["accept"] = json!(false);
                    out["reason"] = json!(format!("the result does not match the ritual's return schema: {}", short(&e)));
                    return out;
                }
            }
        }
        if t.checks.ritual.is_some() {
            let (accept, reason) = match self.run_acceptance(t, result) {
                Ok(v) => v,
                Err(e) => (false, format!("the acceptance ritual could not judge this delivery: {}", short(&e))),
            };
            out["ritual"] = json!({"accept": accept, "reason": reason});
            out["accept"] = json!(accept);
            out["reason"] = json!(reason);
        }
        out
    }

    /// Run the caller's acceptance ritual from the source in the terms.
    fn run_acceptance(self: &Arc<Self>, t: &Terms, result: &Value) -> Result<(bool, String), String> {
        let r = t.checks.ritual.as_ref().ok_or("no acceptance ritual")?;
        let def = crate::ritual::parse_file(&r.source)?;
        if crate::ritual::content_hash(&def) != r.hash {
            return Err("its source does not match the pinned hash".into());
        }
        if !result.is_object() {
            return Err("the delivered result is not an object".into());
        }
        let dir = self.paths.state.join("pacts").join(pact::pact_id(t));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join("accept.yaml");
        std::fs::write(&file, &r.source).map_err(|e| e.to_string())?;
        let started = self
            .invoke(json!({"target": file.to_str().unwrap_or(""), "args": {"delivery": result}, "caller": format!("pact:{}", t.pact), "cwd": dir.to_str().unwrap_or("/"), "expected_hash": r.hash}))
            .map_err(|e| e.message)?;
        let done = self.dispatch("wait", json!({"summon": started["summon"], "timeout_ms": 120_000})).map_err(|e| e.message)?;
        if done["outcome"] != "returned" {
            return Err(format!("it ended {}: {}", done["outcome"], done["error"]));
        }
        let accept = done["result"]["accept"].as_bool().ok_or("it did not return a boolean `accept`")?;
        let reason = done["result"]["reason"].as_str().unwrap_or("").to_string();
        Ok((accept, short(&reason)))
    }

    /// Caller: a delivery is settled; judge it, or ask the owner to.
    fn maybe_accept(self: &Arc<Self>, pact: &str, call: i64) -> Result<(), RpcErr> {
        let row = self.pact_row(pact)?;
        let (t, thread) = {
            let db = self.db.lock().unwrap();
            (terms_of(&db, pact)?, thread_of(&db, pact)?)
        };
        if thread.iter().any(|(e, _)| e.call == Some(call) && matches!(e.kind, Kind::Accept | Kind::Reject)) {
            return Ok(());
        }
        let asked: i64 = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM petitions WHERE operation='pact.accept' AND summon=?1", params![format!("pact:{pact}:{call}")], |r| r.get(0))
            .map_err(sql)?;
        if asked > 0 {
            return Ok(());
        }
        let Some((delivery, _)) = thread.iter().find(|(e, _)| e.kind == Kind::Deliver && e.call == Some(call)) else { return Ok(()) };
        let schema = thread.iter().find_map(|(e, _)| e.data["rehearsal"]["output_schema"].as_object().map(|o| Value::Object(o.clone())));
        let result = delivery.data["result"].clone();
        let checks = self.acceptance_checks(&t, schema.as_ref(), &result);
        match t.accept {
            pact::AcceptMode::Checks => {
                let accept = checks["accept"] == true;
                let data = json!({"decided_by": "checks", "reason": checks["reason"], "checks": checks});
                let kind = if accept { Kind::Accept } else { Kind::Reject };
                let state = if accept { "accepted" } else { "rejected" };
                match self.caller_append(&row, kind, Some(call), data) {
                    Ok(_) => {
                        self.db.lock().unwrap().execute("UPDATE pact_calls SET state=?3 WHERE pact=?1 AND call=?2", params![pact, call, state]).map_err(sql)?;
                        Ok(())
                    }
                    Err(e) if e.exit == 8 => Ok(()),
                    Err(e) => Err(e),
                }
            }
            pact::AcceptMode::Owner => {
                self.db
                    .lock()
                    .unwrap()
                    .execute(
                        "INSERT INTO petitions (id, summon, ritual, step, operation, effect, arguments, selection, because, status, created)
                         VALUES (?1, ?2, ?3, 'accept', 'pact.accept', 'write', ?4, '[]', ?5, 'pending', ?6)",
                        params![new_id("p"), format!("pact:{pact}:{call}"), row.name, json!({"pact": pact, "call": call}).to_string(),
                            json!({"pact": row.name, "call": call, "result": result, "checks": checks}).to_string(), now_ms()],
                    )
                    .map_err(sql)?;
                Ok(())
            }
        }
    }

    /// Owner: accept or reject a delivered call (`pact accept` / `pact reject`).
    pub(super) fn pact_decide_delivery(self: &Arc<Self>, p: Value, accept: bool) -> Result<Value, RpcErr> {
        let DecideDelivery { pact, call, reason } = parse(p)?;
        let row = self.pact_row(&pact)?;
        let pet = self
            .petitions(&requests::Petitions::default())?
            .into_iter()
            .find(|x| x.operation == "pact.accept" && x.summon == format!("pact:{}:{call}", row.id))
            .ok_or_else(|| RpcErr::conflict(format!("call {call} under `{}` is not waiting for acceptance", row.name)))?;
        self.decide_delivery(&pet, accept, reason)
    }

    /// Sign the owner's acceptance or rejection of a delivery and send it.
    pub(super) fn decide_delivery(self: &Arc<Self>, pet: &Petition, accept: bool, reason: Option<String>) -> Result<Value, RpcErr> {
        let pact = pet.arguments["pact"].as_str().unwrap_or("").to_string();
        let call = pet.arguments["call"].as_i64().unwrap_or(0);
        let row = self.pact_row(&pact)?;
        let n = self.db.lock().unwrap().execute("UPDATE petitions SET status='executing' WHERE id=?1 AND status='pending'", params![pet.id]).map_err(sql)?;
        if n != 1 {
            return Err(RpcErr::conflict("this delivery was already decided"));
        }
        let reason = reason.unwrap_or_else(|| if accept { "accepted by the owner".into() } else { "rejected by the owner".into() });
        let data = json!({"decided_by": "owner", "reason": reason, "checks": pet.because["checks"]});
        let sent = match self.caller_append(&row, if accept { Kind::Accept } else { Kind::Reject }, Some(call), data) {
            Ok(s) => json!({"sent": s.hash}),
            Err(e) if e.exit == 8 => json!({"queued": e.message}),
            Err(e) => {
                self.db.lock().unwrap().execute("UPDATE petitions SET status='pending' WHERE id=?1", params![pet.id]).map_err(sql)?;
                return Err(e);
            }
        };
        let (status, state) = if accept { ("applied", "accepted") } else { ("denied", "rejected") };
        let db = self.db.lock().unwrap();
        db.execute("UPDATE petitions SET status=?2, decided=?3, result=?4 WHERE id=?1", params![pet.id, status, now_ms(), sent.to_string()]).map_err(sql)?;
        db.execute("UPDATE pact_calls SET state=?3 WHERE pact=?1 AND call=?2", params![pact, call, state]).map_err(sql)?;
        Ok(json!({"id": pet.id, "pact": row.name, "call": call, "status": status, "state": state, "result": sent}))
    }
}

// ------------------------------------------------------- revise, end, verify

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Export {
    pact: String,
    file: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyFile {
    file: String,
    #[serde(default)]
    key: Option<String>,
}

impl Core {
    /// A revision must replace an agreed pact between the same two cores.
    fn check_revision(&self, t: &Terms) -> Result<(), RpcErr> {
        let Some(old) = &t.revises else { return Ok(()) };
        let bad = |why: &str| RpcErr::invalid(format!("revises {old}: {why}"));
        let row = self.pact_row(old).map_err(|_| bad("no such pact here"))?;
        if row.state != "agreed" {
            return Err(bad(&format!("that pact is {}, not agreed", row.state)));
        }
        let prior = terms_of(&self.db.lock().unwrap(), &row.id)?;
        if key_id_of(&prior.caller) != key_id_of(&t.caller) || key_id_of(&prior.callee) != key_id_of(&t.callee) {
            return Err(bad("a revision must be between the same caller and callee"));
        }
        Ok(())
    }

    /// Sign an end entry on `row`; open calls still finish.
    fn end_pact(self: &Arc<Self>, row: &Row, data: Value) -> Result<(), RpcErr> {
        let to = if data["superseded_by"].is_string() { "superseded" } else { "ended" };
        if row.side == "callee" {
            let s = {
                let mut db = self.db.lock().unwrap();
                let tx = db.transaction().map_err(sql)?;
                let s = append_own(&tx, &self.key, &row.id, Kind::End, None, data)?;
                tx.commit().map_err(sql)?;
                s
            };
            self.push_to_peers_async(&row.id, s);
        } else {
            match self.caller_append(row, Kind::End, None, data) {
                Ok(_) => {}
                Err(e) if e.exit == 8 => {}
                Err(e) => return Err(e),
            }
        }
        self.db.lock().unwrap().execute("UPDATE pacts SET state=?2 WHERE id=?1 AND state IN ('offered','agreed')", params![row.id, to]).map_err(sql)?;
        Ok(())
    }

    pub(super) fn pact_end(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let Which { pact, reason } = parse(p)?;
        let row = self.pact_row(&pact)?;
        if !matches!(row.state.as_str(), "agreed" | "offered") {
            return Err(RpcErr::conflict(format!("pact `{}` is already {}", row.name, row.state)));
        }
        self.end_pact(&row, json!({"reason": reason}))?;
        Ok(json!({"pact": row.name, "id": row.id, "state": "ended"}))
    }

    /// The thread entry by entry, each re-verified, with any forks.
    pub(super) fn pact_history(&self, p: Value) -> Result<Value, RpcErr> {
        let Which { pact, .. } = parse(p)?;
        let row = self.pact_row(&pact)?;
        let db = self.db.lock().unwrap();
        let t = terms_of(&db, &row.id)?;
        let other = self.other_key(&t)?;
        let me = self.key.public();
        let entries: Vec<Value> = thread_of(&db, &row.id)?
            .into_iter()
            .map(|(e, s)| {
                let key = if e.by == me.id() { &me } else { &other };
                let verified = pact::open_entry(&s, key).is_ok();
                json!({"n": e.n, "kind": e.kind, "call": e.call, "by": if e.by == me.id() { "this core" } else { row.peer.as_str() }, "at": e.at, "data": e.data, "hash": e.hash, "verified": verified})
            })
            .collect();
        let forks = Self::forks_of(&db, &row.id)?;
        Ok(json!({"pact": row.name, "id": row.id, "state": row.state, "entries": entries, "forks": forks}))
    }

    fn forks_of(db: &rusqlite::Connection, id: &str) -> Result<Vec<Value>, RpcErr> {
        let mut q = db.prepare("SELECT a, b, seen FROM pact_forks WHERE pact=?1").map_err(sql)?;
        let rows = q
            .query_map(params![id], |r| {
                Ok(json!({"a": serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap_or_default(), "b": serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or_default(), "seen": r.get::<_, i64>(2)?}))
            })
            .map_err(sql)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)
    }

    /// Write the thread for offline checking by anyone holding both keys.
    pub(super) fn pact_export(&self, p: Value) -> Result<Value, RpcErr> {
        let Export { pact, file } = parse(p)?;
        let row = self.pact_row(&pact)?;
        let export = {
            let db = self.db.lock().unwrap();
            let t = terms_of(&db, &row.id)?;
            let mut entries: Vec<Signed> = thread_of(&db, &row.id)?.into_iter().map(|(_, s)| s).collect();
            // A fork's second entry is evidence; it travels with the thread.
            for f in Self::forks_of(&db, &row.id)? {
                if let Ok(s) = serde_json::from_value::<Signed>(f["b"].clone()) {
                    entries.push(s);
                }
            }
            json!({"v": 1, "kind": "pact-thread", "terms": t, "caller_key": t.caller, "callee_key": t.callee, "entries": entries})
        };
        std::fs::write(&file, serde_json::to_string_pretty(&export).unwrap()).map_err(|e| RpcErr::invalid(format!("cannot write {file}: {e}")))?;
        Ok(json!({"pact": row.name, "file": file, "entries": export["entries"].as_array().map(|e| e.len())}))
    }

    /// Check an exported thread with keys this core trusts: its own, pinned
    /// peers', or one given. Keys written in the file are never used alone.
    pub(super) fn pact_verify(&self, p: Value) -> Result<Value, RpcErr> {
        let VerifyFile { file, key } = parse(p)?;
        let export: Value = std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
            .map_err(|e| RpcErr::invalid(format!("{file}: {e}")))?;
        if export["kind"] != "pact-thread" {
            return Err(RpcErr::invalid(format!("{file} is not a pact thread export")));
        }
        let given = key.as_deref().map(crate::keys::PublicKey::parse).transpose().map_err(RpcErr::invalid)?;
        let trusted = |text: &str| -> Result<crate::keys::PublicKey, RpcErr> {
            let id = key_id_of(text);
            if id == self.key.public().id() {
                return Ok(self.key.public());
            }
            if let Some(k) = given.as_ref().filter(|k| k.id() == id) {
                return Ok(*k);
            }
            let pinned: Option<String> = self.db.lock().unwrap().query_row("SELECT key FROM peers WHERE key_id=?1", params![id], |r| r.get(0)).optional().map_err(sql)?;
            let pinned = pinned.ok_or_else(|| RpcErr::not_found(format!("the core {} is not pinned here; pin it with `peer add` after comparing fingerprints, or give its key", &id[..16.min(id.len())])))?;
            crate::keys::PublicKey::parse(&pinned).map_err(RpcErr::invalid)
        };
        let caller = trusted(export["terms"]["caller"].as_str().unwrap_or(""))?;
        let callee = trusted(export["terms"]["callee"].as_str().unwrap_or(""))?;
        match pact::verify_thread(&export, &caller, &callee) {
            Ok(r) => Ok(json!({
                "ok": r.forks.is_empty(), "entries": r.entries, "calls": r.calls, "epitaphs_checked": r.epitaphs_checked,
                "forks": r.forks.iter().map(|(a, b)| json!({"a": a, "b": b})).collect::<Vec<_>>(),
                "problem": if r.forks.is_empty() { Value::Null } else { json!("a core signed two different entries on the same previous entry: signed evidence that it equivocated") },
            })),
            Err((n, why)) => Ok(json!({"ok": false, "problem": {"entry": n, "reason": why}, "forks": []})),
        }
    }
}
