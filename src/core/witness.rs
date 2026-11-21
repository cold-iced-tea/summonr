//! witness service. As a witness: admit wards, countersign chains that
//! extend what was witnessed, and record forks. As a ward: submit this
//! core's new links to a pinned peer and keep its countersignatures.
//! Only signed links cross; bodies, salts and contents never do.
use super::*;
use crate::epitaph;
use crate::keys::PublicKey;
use crate::link::{CoreLink, SignedLink};
use serde::Serialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Accept {
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
struct Peer {
    peer: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Head {
    seq: i64,
    #[serde(default)]
    hash: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OfferedLink {
    seq: i64,
    link: String,
    sig: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Submit {
    from: Head,
    links: Vec<OfferedLink>,
}

fn parse<T: serde::de::DeserializeOwned>(p: Value) -> Result<T, RpcErr> {
    serde_json::from_value(p).map_err(|e| RpcErr::invalid(e.to_string()))
}

fn sql(e: rusqlite::Error) -> RpcErr {
    RpcErr::new("storage_failed", 11, e.to_string())
}

impl Core {
    // ------------------------------------------------------------ witness side

    pub(super) fn witness_accept(&self, p: Value) -> Result<Value, RpcErr> {
        let Accept { name, key } = parse(p)?;
        super::familiars::valid_name(&name)?;
        let key = PublicKey::parse(&key).map_err(RpcErr::invalid)?;
        let db = self.db.lock().unwrap();
        let familiar: i64 = db.query_row("SELECT COUNT(*) FROM familiars WHERE key_id=?1", params![key.id()], |r| r.get(0)).map_err(sql)?;
        if familiar > 0 {
            return Err(RpcErr::conflict("that key is a familiar here; one key cannot be both a familiar and a ward"));
        }
        let n = db
            .execute(
                "INSERT OR IGNORE INTO wards (name, key, key_id, created) VALUES (?1, ?2, ?3, ?4)",
                params![name, key.text(), key.id(), now_ms()],
            )
            .map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::conflict(format!("a ward named `{name}` or with this key already exists")));
        }
        Ok(json!({"name": name, "id": key.id()}))
    }

    pub(super) fn witness_list(&self) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut q = db
            .prepare(
                "SELECT w.name, w.key_id, h.seq, h.hash, h.time, (SELECT COUNT(*) FROM forks f WHERE f.ward_key = w.key_id)
                 FROM wards w LEFT JOIN witnessed h ON h.ward = w.name ORDER BY w.name",
            )
            .map_err(sql)?;
        let rows = q
            .query_map([], |r| {
                Ok(json!({"name": r.get::<_, String>(0)?, "id": r.get::<_, String>(1)?, "seq": r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                          "hash": r.get::<_, Option<String>>(3)?, "time": r.get::<_, Option<i64>>(4)?, "forks": r.get::<_, i64>(5)?}))
            })
            .map_err(sql)?;
        rows.collect::<Result<Vec<_>, _>>().map(Value::Array).map_err(sql)
    }

    pub(super) fn witness_drop(&self, p: Value) -> Result<Value, RpcErr> {
        let Named { name } = parse(p)?;
        let db = self.db.lock().unwrap();
        let n = db.execute("DELETE FROM wards WHERE name=?1", params![name]).map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::not_found(format!("no ward `{name}`")));
        }
        // Forget its witnessed head; its forks stay as evidence, under its key.
        db.execute("DELETE FROM witnessed WHERE ward=?1", params![name]).map_err(sql)?;
        Ok(json!({"name": name, "dropped": true}))
    }

    /// An active ward by key id: its name and key.
    pub(crate) fn ward_by_key(&self, key_id: &str) -> Option<(String, PublicKey)> {
        let (name, key): (String, String) = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT name, key FROM wards WHERE key_id=?1", params![key_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .ok()
            .flatten()?;
        Some((name, PublicKey::parse(&key).ok()?))
    }

    /// What a ward may ask: its witnessed head, and to extend it.
    pub(crate) fn dispatch_ward(&self, name: &str, key: &PublicKey, method: &str, params: Value) -> Result<Value, RpcErr> {
        match method {
            "witness_head" => {
                let held: Option<(i64, String)> = self
                    .db
                    .lock()
                    .unwrap()
                    .query_row("SELECT seq, hash FROM witnessed WHERE ward=?1", params![name], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()
                    .map_err(sql)?;
                Ok(match held {
                    Some((seq, hash)) => json!({"seq": seq, "hash": hash}),
                    None => json!({"seq": 0}),
                })
            }
            "witness_submit" => self.witness_submit(name, key, parse(params)?),
            other => Err(RpcErr::new("not_authorized", 4, format!("a ward may only submit to its witness, not call `{other}`"))),
        }
    }

    fn witness_submit(&self, ward: &str, key: &PublicKey, s: Submit) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let held: Option<(i64, String, i64, String)> = db
            .query_row("SELECT seq, hash, time, sig FROM witnessed WHERE ward=?1", params![ward], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()
            .map_err(sql)?;
        let (held_seq, held_hash) = held.as_ref().map(|h| (h.0, h.1.clone())).unwrap_or((0, String::new()));
        let offered = json!(s.links.iter().map(|l| json!({"seq": l.seq, "link": l.link, "sig": l.sig})).collect::<Vec<_>>());
        // A fork keeps the ward's own signed links, so it proves itself.
        let fork = |why: &str| -> RpcErr {
            let _ = db.execute(
                "INSERT INTO forks (ward, ward_key, held_seq, held_hash, offered_from_seq, offered_from_hash, offered, time) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![ward, key.id(), held_seq, held_hash, s.from.seq, s.from.hash, offered.to_string(), now_ms()],
            );
            eprintln!("witness: ward `{ward}` offered a history that does not extend what was witnessed: {why}");
            RpcErr::conflict(format!("offered history does not extend the witnessed head (#{held_seq}): {why}"))
        };
        let stored = |h: &(i64, String, i64, String)| json!({"seq": h.0, "hash": h.1, "time": h.2, "sig": h.3});
        // First verify every offered link's signature under the ward key.
        // Unsigned or malformed links are not evidence of equivocation and
        // must never be recorded as forks.
        let mut parsed_links = Vec::with_capacity(s.links.len());
        for l in &s.links {
            let link = epitaph::check_link(&l.link, &l.sig, key).map_err(RpcErr::invalid)?;
            parsed_links.push((l, link));
        }
        if held.is_none() {
            // First visit: the witness vouches from the head it is first shown.
            if s.links.len() != 1 {
                return Err(RpcErr::invalid("a first visit sends only the current head"));
            }
        } else if s.from.seq > held_seq || (s.from.seq == held_seq && s.from.hash != held_hash) {
            if parsed_links.is_empty() {
                return Err(RpcErr::invalid("the stated starting point is not part of the witnessed history"));
            }
            return Err(fork("the stated starting point is not part of the witnessed history"));
        }
        // Check the offered links in order from the stated starting point.
        let (mut prev_seq, mut prev_hash) = match &held {
            None => (s.links[0].seq - 1, None),
            Some(_) => (s.from.seq, Some(s.from.hash.clone())),
        };
        let mut passed_held = held.is_none() || s.from.seq == held_seq;
        for (l, link) in parsed_links {
            let seq = link["seq"].as_i64().unwrap_or(-1);
            if seq != prev_seq + 1 || seq != l.seq {
                return Err(RpcErr::invalid(format!("epitaph {} is missing or out of order", prev_seq + 1)));
            }
            if let Some(h) = &prev_hash {
                if link["prev"].as_str() != Some(h.as_str()) {
                    return Err(fork(&format!("epitaph {seq} does not follow the previous one")));
                }
            }
            prev_seq = seq;
            prev_hash = Some(epitaph::hash(&l.link));
            // A late submitter starting before the held head must pass through it exactly.
            if seq == held_seq {
                if prev_hash.as_deref() != Some(held_hash.as_str()) {
                    return Err(fork(&format!("epitaph {seq} differs from the one witnessed")));
                }
                passed_held = true;
            }
        }
        if !passed_held {
            return Err(RpcErr::invalid("the offered links stop before the witnessed head"));
        }
        // Nothing beyond what is already witnessed: repeat its countersignature.
        if let Some(h) = &held {
            if prev_seq <= h.0 {
                return Ok(stored(h));
            }
        }
        let hash = prev_hash.ok_or_else(|| RpcErr::invalid("nothing to witness"))?;
        let time = now_ms();
        let sig = epitaph::countersign(&self.key, &key.id(), prev_seq, &hash, time);
        db.execute(
            "INSERT OR REPLACE INTO witnessed (ward, seq, hash, time, sig) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![ward, prev_seq, hash, time, sig],
        )
        .map_err(sql)?;
        Ok(json!({"seq": prev_seq, "hash": hash, "time": time, "sig": sig}))
    }

    // --------------------------------------------------------------- ward side

    /// Submit this core's new links to pinned peer `peer` and keep its
    /// countersignature.
    pub(super) fn witness_to(&self, p: Value) -> Result<Value, RpcErr> {
        let Peer { peer } = parse(p)?;
        let _one_at_a_time = self.witnessing.lock().unwrap();
        let (url, peer_key, mut link) = self.peer_link(&peer)?;
        let unreachable = |e: RpcErr| RpcErr::new(&e.code, e.exit, format!("witness `{peer}` ({url}): {}", e.message));
        let held = link.call("witness_head", json!({})).map_err(unreachable)?;
        let held_seq = held["seq"].as_i64().unwrap_or(0);
        let (links, from) = {
            let db = self.db.lock().unwrap();
            let local_head: i64 = db.query_row("SELECT COALESCE(MAX(seq), 0) FROM epitaphs", [], |r| r.get(0)).map_err(sql)?;
            if local_head == 0 {
                return Err(RpcErr::conflict("the chain is empty; there is nothing to witness yet"));
            }
            if held_seq > 0 {
                let ours: Option<String> = db.query_row("SELECT hash FROM epitaphs WHERE seq=?1", params![held_seq], |r| r.get(0)).optional().map_err(sql)?;
                if ours.as_deref() != held["hash"].as_str() {
                    return Err(RpcErr::conflict(format!(
                        "witness `{peer}` holds a head this chain does not contain (#{held_seq}): this core's history differs from what it witnessed"
                    )));
                }
            }
            let first = if held_seq == 0 { local_head } else { held_seq + 1 };
            let mut q = db.prepare("SELECT seq, link, sig FROM epitaphs WHERE seq >= ?1 ORDER BY seq").map_err(sql)?;
            let links: Vec<Value> = q
                .query_map(params![first], |r| Ok(json!({"seq": r.get::<_, i64>(0)?, "link": r.get::<_, String>(1)?, "sig": r.get::<_, String>(2)?})))
                .map_err(sql)?
                .filter_map(Result::ok)
                .collect();
            (links, json!({"seq": held_seq, "hash": held["hash"].as_str().unwrap_or("")}))
        };
        // The witness must countersign exactly the head offered.
        let expected = match links.last() {
            Some(l) => (l["seq"].as_i64().unwrap_or(0), epitaph::hash(l["link"].as_str().unwrap_or(""))),
            None => (held_seq, held["hash"].as_str().unwrap_or("").to_string()),
        };
        let out = link.call("witness_submit", json!({"from": from, "links": links})).map_err(unreachable)?;
        let (seq, hash, time, sig) = (out["seq"].as_i64().unwrap_or(0), out["hash"].as_str().unwrap_or(""), out["time"].as_i64().unwrap_or(0), out["sig"].as_str().unwrap_or(""));
        if (seq, hash) != (expected.0, expected.1.as_str()) {
            return Err(RpcErr::new("not_authenticated", 4, format!("witness `{peer}` countersigned #{seq}, not the head offered (#{})", expected.0)));
        }
        if !epitaph::check_countersign(&peer_key, &self.key.public().id(), seq, hash, time, sig) {
            return Err(RpcErr::new("not_authenticated", 4, format!("witness `{peer}` returned a countersignature that does not verify")));
        }
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO witnessings (peer, seq, hash, time, sig) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![peer, seq, hash, time, sig],
            )
            .map_err(sql)?;
        Ok(json!({"peer": peer, "seq": seq, "hash": hash, "time": time}))
    }

    /// Have an RFC 3161 authority timestamp the current head link's hash.
    pub(super) fn witness_tsa(&self, p: Value) -> Result<Value, RpcErr> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Url {
            url: String,
        }
        let Url { url } = parse(p)?;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(RpcErr::invalid(format!("`{url}` is not an http:// or https:// URL")));
        }
        let prog = crate::tsa::program(self.cfg.openssl.as_deref());
        if !crate::tsa::available(&prog) {
            return Err(RpcErr::new("backend_unavailable", 8, format!("openssl not found ({prog}); RFC 3161 timestamps need it")));
        }
        let head = self.chain_head()?;
        let (seq, hash) = match (head["seq"].as_i64(), head["hash"].as_str()) {
            (Some(s), Some(h)) if s > 0 => (s, h.to_string()),
            _ => return Err(RpcErr::conflict("the chain is empty; there is nothing to timestamp yet")),
        };
        let failed = |e: String| RpcErr::new("backend_unavailable", 8, e);
        let request = crate::tsa::query(&prog, &hash).map_err(failed)?;
        let response = crate::tsa::post(&url, &request).map_err(failed)?;
        crate::tsa::granted(&prog, &response, &hash).map_err(failed)?;
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO timestamps (seq, hash, url, response, time) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![seq, hash, url, response, now_ms()],
            )
            .map_err(sql)?;
        Ok(json!({"url": url, "seq": seq, "hash": hash}))
    }

    /// Each stored timestamp, checked with `openssl` when a CA file is given.
    pub(super) fn timestamp_status(&self, ca: Option<&str>) -> Vec<Value> {
        /// seq, hash timestamped, authority URL, response, this chain's hash at that seq.
        type Stamp = (i64, String, String, Vec<u8>, Option<String>);
        let rows: Vec<Stamp> = {
            let db = self.db.lock().unwrap();
            let Ok(mut q) = db.prepare(
                "SELECT t.seq, t.hash, t.url, t.response, e.hash FROM timestamps t LEFT JOIN epitaphs e ON e.seq = t.seq ORDER BY t.seq",
            ) else {
                return vec![];
            };
            q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, Option<String>>(4)?)))
                .map(|rows| rows.filter_map(Result::ok).collect())
                .unwrap_or_default()
        };
        let prog = crate::tsa::program(self.cfg.openssl.as_deref());
        let have = !rows.is_empty() && ca.is_some() && crate::tsa::available(&prog);
        rows.into_iter()
            .map(|(seq, hash, url, response, ours)| {
                let matches = ours.as_deref() == Some(hash.as_str());
                let verdict = match (ca, have) {
                    (None, _) => "unchecked (give tsa-ca FILE to check)".to_string(),
                    (Some(_), false) => format!("unchecked (openssl not found: {prog})"),
                    (Some(ca), true) => match crate::tsa::verify(&prog, &hash, &response, Path::new(ca)) {
                        Ok(_) => "verified".to_string(),
                        Err(e) => format!("failed: {e}"),
                    },
                };
                json!({"seq": seq, "url": url, "verdict": verdict, "matches": matches})
            })
            .collect()
    }

    /// The latest countersignature from each witness, checked against its pinned key.
    pub(super) fn witness_status(&self) -> Vec<Value> {
        let db = self.db.lock().unwrap();
        let Ok(mut q) = db.prepare(
            "SELECT w.peer, w.seq, w.hash, w.time, w.sig, p.key, e.hash FROM witnessings w LEFT JOIN peers p ON p.name = w.peer
             LEFT JOIN epitaphs e ON e.seq = w.seq
             WHERE w.seq = (SELECT MAX(seq) FROM witnessings x WHERE x.peer = w.peer) ORDER BY w.peer",
        ) else {
            return vec![];
        };
        let me = self.key.public().id();
        let now = now_ms();
        q.query_map([], |r| {
            let (peer, seq, hash, time, sig, key, ours): (String, i64, String, i64, String, Option<String>, Option<String>) =
                (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?);
            // A countersignature for a head this chain no longer has is evidence of a rewrite.
            let matches = ours.as_deref() == Some(hash.as_str());
            let valid = key
                .and_then(|k| PublicKey::parse(&k).ok())
                .is_some_and(|k| epitaph::check_countersign(&k, &me, seq, &hash, time, &sig));
            Ok(json!({"peer": peer, "seq": seq, "time": time, "age_ms": now - time, "valid": valid, "matches": matches}))
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    fn peer_link(&self, peer: &str) -> Result<(String, PublicKey, SignedLink), RpcErr> {
        let pinned: Option<(String, String)> = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT url, key FROM peers WHERE name=?1", params![peer], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(sql)?;
        let (url, peer_key) = pinned.ok_or_else(|| {
            RpcErr::not_found(format!("no pinned peer `{peer}`; add it with `summonr peer add {peer} url URL key K`"))
        })?;
        let peer_key = PublicKey::parse(&peer_key).map_err(RpcErr::invalid)?;
        let me = crate::keys::load(&self.paths.config.join("core.key")).map_err(|e| RpcErr::new("storage_failed", 11, e))?;
        let target = url.clone();
        let link = SignedLink::new(me, peer_key, Box::new(move |f: &Value| crate::link::http_send(&target, f)));
        Ok((url, peer_key, link))
    }

    /// Compare witnessed heads across peers and against this core's local chain.
    ///
    /// Accepts optional `{"peers": ["p1", ...], "live": bool}`. When `live` is true,
    /// queries `witness_head` from each named pinned peer; otherwise compares the
    /// latest stored `witnessings` countersignatures and verifies each one.
    pub(crate) fn witness_compare(&self, params: Value) -> Result<Value, RpcErr> {
        #[derive(Deserialize, Default)]
        #[serde(deny_unknown_fields)]
        struct WitnessCompareParams {
            #[serde(default)]
            peers: Option<Vec<String>>,
            #[serde(default)]
            live: bool,
        }

        let p: WitnessCompareParams = if params.is_null() {
            WitnessCompareParams::default()
        } else {
            parse(params)?
        };

        let explicit_peers = p.peers.is_some();
        let peer_names: Vec<String> = match p.peers {
            Some(list) => {
                let mut seen = HashSet::new();
                list.into_iter().filter(|name| seen.insert(name.clone())).collect()
            }
            None => {
                let db = self.db.lock().unwrap();
                let mut names = BTreeSet::new();
                let mut q1 = db.prepare("SELECT name FROM peers").map_err(sql)?;
                for row in q1.query_map([], |r| r.get::<_, String>(0)).map_err(sql)? {
                    names.insert(row.map_err(sql)?);
                }
                if !p.live {
                    let mut q2 = db.prepare("SELECT DISTINCT peer FROM witnessings").map_err(sql)?;
                    for row in q2.query_map([], |r| r.get::<_, String>(0)).map_err(sql)? {
                        names.insert(row.map_err(sql)?);
                    }
                }
                names.into_iter().collect()
            }
        };

        let mut records = Vec::new();
        if p.live {
            for peer in peer_names {
                let (url, _, mut link) = self.peer_link(&peer)?;
                let head = link
                    .call("witness_head", json!({}))
                    .map_err(|e| RpcErr::new(&e.code, e.exit, format!("witness `{peer}` ({url}): {}", e.message)))?;
                let seq = head["seq"].as_i64().unwrap_or(0);
                if seq > 0 {
                    let hash = head["hash"].as_str().unwrap_or("").to_string();
                    let valid = !hash.is_empty();
                    records.push(WitnessRecord {
                        label: peer,
                        seq,
                        hash,
                        merkle_root: head["merkle_root"].as_str().map(str::to_string),
                        valid,
                    });
                }
            }
        } else {
            let db = self.db.lock().unwrap();
            let me = self.key.public().id();
            for peer in peer_names {
                if explicit_peers {
                    let known: i64 = db
                        .query_row(
                            "SELECT (SELECT COUNT(*) FROM peers WHERE name=?1) + (SELECT COUNT(*) FROM witnessings WHERE peer=?1)",
                            params![peer],
                            |r| r.get(0),
                        )
                        .map_err(sql)?;
                    if known == 0 {
                        return Err(RpcErr::not_found(format!("no pinned peer or witnessing for `{peer}`")));
                    }
                }
                let row: Option<(i64, String, i64, String, Option<String>)> = db
                    .query_row(
                        "SELECT w.seq, w.hash, w.time, w.sig, p.key
                         FROM witnessings w LEFT JOIN peers p ON p.name = w.peer
                         WHERE w.peer = ?1 ORDER BY w.seq DESC LIMIT 1",
                        params![peer],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )
                    .optional()
                    .map_err(sql)?;
                if let Some((seq, hash, time, sig, key)) = row {
                    let valid = key
                        .and_then(|k| PublicKey::parse(&k).ok())
                        .is_some_and(|k| epitaph::check_countersign(&k, &me, seq, &hash, time, &sig));
                    records.push(WitnessRecord {
                        label: peer,
                        seq,
                        hash,
                        merkle_root: None,
                        valid,
                    });
                }
            }
        }

        let head = self.chain_head()?;
        let head_seq = head["seq"].as_i64().unwrap_or(0);
        let head_hash = head["hash"].as_str().map(str::to_string);
        let head_merkle = head["merkle_root"].as_str().map(str::to_string);

        let peer_seqs: BTreeSet<i64> = records.iter().filter(|r| r.seq > 0).map(|r| r.seq).collect();
        {
            let db = self.db.lock().unwrap();
            for seq in &peer_seqs {
                let local_hash: Option<String> = db
                    .query_row("SELECT hash FROM epitaphs WHERE seq = ?1", params![seq], |r| r.get(0))
                    .optional()
                    .map_err(sql)?;
                if let Some(hash) = local_hash {
                    records.push(WitnessRecord {
                        label: "local".to_string(),
                        seq: *seq,
                        hash,
                        merkle_root: if *seq == head_seq { head_merkle.clone() } else { None },
                        valid: true,
                    });
                }
            }
        }
        if head_seq > 0 && !peer_seqs.contains(&head_seq) {
            if let Some(hash) = head_hash {
                records.push(WitnessRecord {
                    label: "local".to_string(),
                    seq: head_seq,
                    hash,
                    merkle_root: head_merkle,
                    valid: true,
                });
            }
        }

        Ok(compare_witness_records(&records))
    }
}

/// A witness or local-chain observation at a sequence number.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WitnessRecord {
    /// Peer name or `"local"` for this core's own chain.
    pub label: String,
    /// Sequence number observed.
    pub seq: i64,
    /// Link hash at `seq`.
    pub hash: String,
    /// Optional Merkle root at `seq`.
    pub merkle_root: Option<String>,
    /// Whether the observation's signature or verification succeeded.
    pub valid: bool,
}

/// Compare a set of witness and local-chain observations for split-view conflicts.
///
/// Checks every pair of observations at the same `seq` for differing `hash` or
/// differing `merkle_root` (when both are present), and notes prefix comparability
/// when observations are at different sequence numbers.
pub fn compare_witness_records(records: &[WitnessRecord]) -> Value {
    let invalid = records.iter().filter(|r| !r.valid).count();
    let has_local_at = |seq: i64| records.iter().any(|r| r.label == "local" && r.seq == seq);

    let mut conflicts = Vec::new();
    let mut prefix_notes = Vec::new();

    for i in 0..records.len() {
        for j in (i + 1)..records.len() {
            let a = &records[i];
            let b = &records[j];
            if a.seq == b.seq {
                if a.hash != b.hash {
                    conflicts.push(json!({
                        "seq": a.seq,
                        "a": a.label,
                        "b": b.label,
                        "hash_a": a.hash,
                        "hash_b": b.hash,
                        "reason": "split_view",
                    }));
                } else if let (Some(ma), Some(mb)) = (&a.merkle_root, &b.merkle_root) {
                    if ma != mb {
                        conflicts.push(json!({
                            "seq": a.seq,
                            "a": a.label,
                            "b": b.label,
                            "hash_a": a.hash,
                            "hash_b": b.hash,
                            "merkle_root_a": ma,
                            "merkle_root_b": mb,
                            "reason": "split_view",
                        }));
                    }
                }
            } else {
                let (low, high) = if a.seq < b.seq { (a, b) } else { (b, a) };
                prefix_notes.push(json!({
                    "lower": low.label,
                    "lower_seq": low.seq,
                    "higher": high.label,
                    "higher_seq": high.seq,
                    "local_bridged": has_local_at(low.seq) && has_local_at(high.seq),
                }));
            }
        }
    }

    let peer_observations = records.iter().filter(|r| r.label != "local").count();
    json!({
        "ok": peer_observations > 0 && conflicts.is_empty() && invalid == 0,
        "witnesses": peer_observations,
        "split_view": !conflicts.is_empty(),
        "invalid": invalid,
        "conflicts": conflicts,
        "prefix_notes": prefix_notes,
        "observations": records,
    })
}
