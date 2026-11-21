use super::*;

impl Core {
    // ------------------------------------------------------------ admission

    pub(in crate::core) fn pact_admit(&self, p: Value) -> Result<Value, RpcErr> {
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

    pub(in crate::core) fn pact_unadmit(&self, p: Value) -> Result<Value, RpcErr> {
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
    pub(super) fn party_terms(&self, key: &crate::keys::PublicKey, id: &str) -> Result<Terms, RpcErr> {
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
    pub(super) fn push_to_witnesses_async(self: &Arc<Self>, pact_id: &str, entry: Signed) {
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
    pub(super) fn push_to_peers_async(self: &Arc<Self>, pact_id: &str, entry: Signed) {
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
    pub(super) fn push_offer_to_witnesses_async(self: &Arc<Self>, t: &Terms, entry: Signed) {
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
    pub(in crate::core) fn receive(self: &Arc<Self>, pact: &str, entry: &Signed, by: &crate::keys::PublicKey) -> Result<Value, RpcErr> {
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
    pub(in crate::core) fn on_received(self: &Arc<Self>, pact: &str, entry: &Signed) -> Result<(), RpcErr> {
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
    pub(super) fn on_received_more(self: &Arc<Self>, pact: &str, e: &Entry) -> Result<(), RpcErr> {
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

