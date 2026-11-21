//! epitaphs service: each ended run's signed, chained, fingerprint-only
//! account, written in the same transaction that records the end.
use super::*;
use crate::epitaph;

pub(super) use crate::epitaph::ZEROS;
/// The summon columns an epitaph fingerprints, as stored.
pub(super) const PIECES: [&str; 5] = ["inputs", "result", "error", "essence", "cleanup"];

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Fingerprint `bytes` with a fresh salt, keeping the salt beside the elegy.
fn fingerprint_part(tx: &rusqlite::Transaction, run: &str, part: &str, bytes: &[u8]) -> rusqlite::Result<String> {
    let salt = epitaph::new_salt();
    let fp = epitaph::fingerprint(&salt, bytes);
    tx.execute(
        "INSERT INTO epitaph_salts (run, part, salt, fp) VALUES (?1, ?2, ?3, ?4)",
        params![run, part, hex(&salt), fp],
    )?;
    Ok(fp)
}

/// Who asked for the root of `run`'s tree: children act for whoever started it.
pub(super) fn root_caller(conn: &rusqlite::Connection, run: &str) -> rusqlite::Result<String> {
    let mut id = run.to_string();
    for _ in 0..64 {
        let (parent, caller): (Option<String>, Option<String>) =
            conn.query_row("SELECT parent, familiar FROM summons WHERE id=?1", params![id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        match parent {
            Some(p) => id = p,
            None => return Ok(caller.unwrap_or_default()),
        }
    }
    Ok(String::new())
}

/// Seal `run`'s epitaph onto the chain. Call inside the transaction that
/// records the run's end, after the end is written.
pub(super) fn write_epitaph(tx: &rusqlite::Transaction, core: &crate::keys::Keypair, run: &str) -> rusqlite::Result<()> {
    #[allow(clippy::type_complexity)]
    let (parent, ritual, version, hash, _caller, started, ended, outcome, pieces): (
        Option<String>, String, i64, String, Option<String>, i64, Option<i64>, Option<String>, Vec<Option<String>>,
    ) = tx.query_row(
        "SELECT parent, ritual, version, hash, familiar, started, ended, outcome, inputs, result, error, essence, cleanup FROM summons WHERE id=?1",
        params![run],
        |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?,
                (8..13).map(|i| r.get::<_, Option<String>>(i)).collect::<rusqlite::Result<Vec<_>>>()?))
        },
    )?;
    let mut fingerprints = Map::new();
    for (part, text) in PIECES.iter().zip(pieces) {
        let fp = match text {
            Some(t) => json!(fingerprint_part(tx, run, part, t.as_bytes())?),
            None => Value::Null,
        };
        fingerprints.insert(part.to_string(), fp);
    }
    let events: Vec<(i64, String, Option<String>)> = {
        let mut q = tx.prepare("SELECT seq, kind, data FROM events WHERE summon=?1 ORDER BY seq")?;
        let rows = q.query_map(params![run], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut fps = Vec::with_capacity(events.len());
    for (seq, kind, data) in &events {
        let bytes = epitaph::event_bytes(*seq, kind, data.as_deref().unwrap_or(""));
        fps.push(fingerprint_part(tx, run, &format!("event:{seq}"), &bytes)?);
    }
    fingerprints.insert("events".into(), json!({"count": fps.len(), "root": epitaph::events_root(&fps)}));
    let children: Vec<String> = {
        let mut q = tx.prepare(
            "SELECT e.hash FROM summons s JOIN epitaphs e ON e.run = s.id WHERE s.parent=?1 ORDER BY e.seq",
        )?;
        let rows = q.query_map(params![run], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    // A child acts for whoever asked for its root run.
    let root = root_caller(tx, run)?;
    let body = json!({
        "v": 1, "run": run, "parent": parent,
        "ritual": {"name": ritual, "version": version, "hash": hash},
        "caller": root, "started": started, "ended": ended, "outcome": outcome,
        "fingerprints": fingerprints, "children": children,
    });
    let (seq, prev): (i64, String) = tx
        .query_row("SELECT seq, hash FROM epitaphs ORDER BY seq DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
        .unwrap_or((0, ZEROS.to_string()));
    let sealed = epitaph::seal(core, seq + 1, &prev, &body);
    tx.execute(
        "INSERT INTO epitaphs (seq, run, link, body, sig, hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![seq + 1, run, sealed.link, sealed.body, sealed.sig, sealed.hash],
    )?;
    Ok(())
}

/// Anchor pact entry `n` of `pact`, signed by this core, in the chain: the
/// body holds only a salted fingerprint of the entry's bytes.
pub(super) fn write_pact_epitaph(tx: &rusqlite::Transaction, core: &crate::keys::Keypair, pact: &str, n: i64, bytes: &[u8]) -> rusqlite::Result<()> {
    let run = format!("pact:{pact}:{n}");
    let fp = fingerprint_part(tx, &run, "entry", bytes)?;
    let body = json!({"v": 1, "kind": "pact", "run": run, "pact": pact, "n": n, "fingerprint": fp});
    let (seq, prev): (i64, String) = tx
        .query_row("SELECT seq, hash FROM epitaphs ORDER BY seq DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
        .unwrap_or((0, ZEROS.to_string()));
    let sealed = epitaph::seal(core, seq + 1, &prev, &body);
    tx.execute(
        "INSERT INTO epitaphs (seq, run, link, body, sig, hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![seq + 1, run, sealed.link, sealed.body, sealed.sig, sealed.hash],
    )?;
    Ok(())
}

/// Seal a post-run petition decision onto the epitaph chain inside the same
/// transaction that records the decision.
pub(super) fn write_petition_epitaph(
    tx: &rusqlite::Transaction,
    core: &crate::keys::Keypair,
    petition_id: &str,
    summon: &str,
    status: &str,
    decided: i64,
    result: &Value,
) -> rusqlite::Result<()> {
    let run = format!("petition:{petition_id}");
    let payload = json!({
        "id": petition_id,
        "summon": summon,
        "status": status,
        "decided": decided,
        "result": result,
    })
    .to_string();
    let fp = fingerprint_part(tx, &run, "decision", payload.as_bytes())?;
    let body = json!({
        "v": 1,
        "kind": "petition",
        "run": run,
        "petition": petition_id,
        "summon": summon,
        "status": status,
        "decided": decided,
        "fingerprint": fp,
    });
    let (seq, prev): (i64, String) = tx
        .query_row("SELECT seq, hash FROM epitaphs ORDER BY seq DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
        .unwrap_or((0, ZEROS.to_string()));
    let sealed = epitaph::seal(core, seq + 1, &prev, &body);
    tx.execute(
        "INSERT INTO epitaphs (seq, run, link, body, sig, hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![seq + 1, run, sealed.link, sealed.body, sealed.sig, sealed.hash],
    )?;
    Ok(())
}

/// Whether a pact epitaph still matches the stored entry.
fn recheck_pact(db: &rusqlite::Connection, run: &str, body: &Value) -> Result<Option<String>, ()> {
    let (pact, n) = (body["pact"].as_str().unwrap_or(""), body["n"].as_i64().unwrap_or(0));
    let bytes: Option<String> =
        db.query_row("SELECT bytes FROM pact_entries WHERE pact=?1 AND n=?2", params![pact, n], |r| r.get(0)).optional().map_err(|_| ())?;
    let Some(bytes) = bytes else { return Ok(Some(format!("pact entry {n} was removed"))) };
    let salt: Option<String> =
        db.query_row("SELECT salt FROM epitaph_salts WHERE run=?1 AND part='entry'", params![run], |r| r.get(0)).optional().map_err(|_| ())?;
    match salt.as_deref().and_then(epitaph::unhex16) {
        Some(salt) if body["fingerprint"].as_str() == Some(epitaph::fingerprint(&salt, bytes.as_bytes()).as_str()) => Ok(None),
        _ => Ok(Some(format!("pact entry {n} differs from its fingerprint"))),
    }
}

/// Whether a petition epitaph still matches the stored decision.
fn recheck_petition(db: &rusqlite::Connection, run: &str, body: &Value) -> Result<Option<String>, ()> {
    let petition_id = body["petition"].as_str().unwrap_or("");
    let row: Option<(String, String, Option<i64>, Option<String>)> = db
        .query_row(
            "SELECT summon, status, decided, result FROM petitions WHERE id=?1",
            params![petition_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(|_| ())?;
    let Some((summon, status, decided, result_raw)) = row else {
        return Ok(Some(format!("petition {petition_id} was removed")));
    };
    let result_val: Value = result_raw
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    let payload = json!({
        "id": petition_id,
        "summon": summon,
        "status": status,
        "decided": decided,
        "result": result_val,
    })
    .to_string();
    let salt: Option<String> = db
        .query_row(
            "SELECT salt FROM epitaph_salts WHERE run=?1 AND part='decision'",
            params![run],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| ())?;
    match salt.as_deref().and_then(epitaph::unhex16) {
        Some(salt) if body["fingerprint"].as_str() == Some(epitaph::fingerprint(&salt, payload.as_bytes()).as_str()) => Ok(None),
        _ => Ok(Some(format!("petition {petition_id} differs from its fingerprint"))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
    run: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reveal {
    run: String,
    part: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Range {
    from: i64,
    to: i64,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct VerifyArgs {
    #[serde(default)]
    mark: Option<String>,
    #[serde(default)]
    tsa_ca: Option<String>,
}

fn parse<T: serde::de::DeserializeOwned>(p: Value) -> Result<T, RpcErr> {
    serde_json::from_value(p).map_err(|e| RpcErr::invalid(e.to_string()))
}

fn sql(e: rusqlite::Error) -> RpcErr {
    RpcErr::new("storage_failed", 11, e.to_string())
}

/// One stored epitaph row.
struct Row {
    seq: i64,
    run: String,
    link: String,
    body: String,
    sig: String,
    hash: String,
}

fn rows(db: &rusqlite::Connection, filter: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Row>, RpcErr> {
    let mut q = db.prepare(&format!("SELECT seq, run, link, body, sig, hash FROM epitaphs {filter} ORDER BY seq")).map_err(sql)?;
    let out = q
        .query_map(args, |r| Ok(Row { seq: r.get(0)?, run: r.get(1)?, link: r.get(2)?, body: r.get(3)?, sig: r.get(4)?, hash: r.get(5)? }))
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql);
    out
}

fn shown(r: &Row) -> Value {
    json!({"seq": r.seq, "run": r.run, "link": r.link, "body": r.body, "sig": r.sig})
}

/// Why a stored run no longer matches its epitaph, or `None` if it matches.
/// `Err(())` means the run's record is gone, so it cannot be re-checked.
fn recheck(db: &rusqlite::Connection, run: &str, body: &Value) -> Result<Option<String>, ()> {
    #[allow(clippy::type_complexity)]
    let row: Option<(Option<String>, String, i64, String, i64, Option<i64>, Option<String>, Vec<Option<String>>)> = db
        .query_row(
            "SELECT parent, ritual, version, hash, started, ended, outcome, inputs, result, error, essence, cleanup FROM summons WHERE id=?1",
            params![run],
            |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?,
                    (7..12).map(|i| r.get::<_, Option<String>>(i)).collect::<rusqlite::Result<Vec<_>>>()?))
            },
        )
        .optional()
        .ok()
        .flatten();
    let Some((parent, ritual, version, hash, started, ended, outcome, pieces)) = row else { return Err(()) };
    let differs = |what: &str| Ok(Some(format!("run {run}: its recorded {what} differs from its epitaph")));
    // What the run's record claims must be what was signed.
    if body["parent"] != json!(parent) {
        return differs("parent");
    }
    if body["ritual"]["name"] != json!(ritual) || body["ritual"]["version"] != json!(version) || body["ritual"]["hash"] != json!(hash) {
        return differs("ritual");
    }
    if body["started"] != json!(started) || body["ended"] != json!(ended) {
        return differs("times");
    }
    if body["outcome"] != json!(outcome) {
        return differs("outcome");
    }
    if root_caller(db, run).ok().as_deref() != body["caller"].as_str() {
        return differs("caller");
    }
    let salt_of = |part: &str| -> Option<Option<[u8; 16]>> {
        let hex: Option<String> =
            db.query_row("SELECT salt FROM epitaph_salts WHERE run=?1 AND part=?2", params![run, part], |r| r.get(0)).optional().ok()?;
        // Outer None: no salt row. Inner None: a malformed one.
        Some(hex.map(|h| epitaph::unhex16(&h)).unwrap_or(None))
    };
    for (part, text) in PIECES.iter().zip(pieces) {
        let want = body["fingerprints"][part].as_str();
        match (want, text) {
            (None, None) => {}
            (None, Some(_)) => return Ok(Some(format!("run {run}: its {part} was written after its epitaph"))),
            (Some(_), None) => return Ok(Some(format!("run {run}: its {part} was removed"))),
            (Some(w), Some(t)) => match salt_of(part).flatten() {
                Some(salt) if epitaph::fingerprint(&salt, t.as_bytes()) == w => {}
                _ => return Ok(Some(format!("run {run}: its {part} differs from its fingerprint"))),
            },
        }
    }
    let events: Vec<(i64, String, Option<String>)> = {
        let Ok(mut q) = db.prepare("SELECT seq, kind, data FROM events WHERE summon=?1 ORDER BY seq") else { return Err(()) };
        let Ok(rows) = q.query_map(params![run], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))) else { return Err(()) };
        rows.filter_map(Result::ok).collect()
    };
    let count = body["fingerprints"]["events"]["count"].as_u64().unwrap_or(0);
    if events.is_empty() && count > 0 {
        return Ok(Some(format!("run {run}: its events were deleted")));
    }
    let mut fps = Vec::new();
    for (seq, kind, data) in &events {
        match salt_of(&format!("event:{seq}")) {
            None => return Ok(Some(format!("run {run}: event {seq} was added after its epitaph"))),
            Some(None) => return Ok(Some(format!("run {run}: event {seq}'s salt is damaged"))),
            Some(Some(salt)) => fps.push(epitaph::fingerprint(&salt, &epitaph::event_bytes(*seq, kind, data.as_deref().unwrap_or("")))),
        }
    }
    if fps.len() as u64 != count || body["fingerprints"]["events"]["root"].as_str() != Some(epitaph::events_root(&fps).as_str()) {
        return Ok(Some(format!("run {run}: its events differ from their fingerprints")));
    }
    Ok(None)
}

impl Core {
    pub(super) fn epitaph_get(&self, p: Value) -> Result<Value, RpcErr> {
        let Run { run } = parse(p)?;
        let db = self.db.lock().unwrap();
        let row = rows(&db, "WHERE run=?1", &[&run])?
            .pop()
            .ok_or_else(|| RpcErr::not_found(format!("no epitaph for `{run}`; runs that ended before epitaphs existed have none")))?;
        let key = self.key.public();
        let verifies = epitaph::check_link(&row.link, &row.sig, &key).and_then(|l| epitaph::check_body(&l, &row.body)).is_ok();
        let mut out = shown(&row);
        out["hash"] = json!(epitaph::hash(&row.link));
        out["core_key"] = json!(key.text());
        out["verifies"] = json!(verifies);
        Ok(out)
    }

    pub(super) fn epitaph_reveal(&self, p: Value) -> Result<Value, RpcErr> {
        let Reveal { run, part } = parse(p)?;
        let epitaph = self.epitaph_get(json!({"run": run}))?;
        let db = self.db.lock().unwrap();
        let salt: String = db
            .query_row("SELECT salt FROM epitaph_salts WHERE run=?1 AND part=?2", params![run, part], |r| r.get(0))
            .optional()
            .map_err(sql)?
            .ok_or_else(|| RpcErr::not_found(format!("`{run}` has no part `{part}`; parts are {} and event:N", PIECES.join(", "))))?;
        let mut reveal = json!({"run": run, "part": part, "salt": salt});
        if let Some(n) = part.strip_prefix("event:") {
            let n: i64 = n.parse().map_err(|_| RpcErr::invalid("event parts look like event:3"))?;
            let (kind, data): (String, Option<String>) = db
                .query_row("SELECT kind, data FROM events WHERE summon=?1 AND seq=?2", params![run, n], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|_| RpcErr::not_found(format!("`{run}` has no event {n} on disk")))?;
            reveal["bytes"] = json!(String::from_utf8_lossy(&epitaph::event_bytes(n, &kind, data.as_deref().unwrap_or(""))));
            let mut q = db.prepare("SELECT part, fp FROM epitaph_salts WHERE run=?1 AND part LIKE 'event:%'").map_err(sql)?;
            let mut fps: Vec<(i64, String)> = q
                .query_map(params![run], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(sql)?
                .filter_map(Result::ok)
                .filter_map(|(p, f)| p.strip_prefix("event:").and_then(|n| n.parse().ok()).map(|n| (n, f)))
                .collect();
            fps.sort();
            reveal["fps"] = json!(fps.into_iter().map(|(seq, fp)| json!({"seq": seq, "fp": fp})).collect::<Vec<_>>());
        } else {
            let col = PIECES.iter().find(|c| **c == part).ok_or_else(|| RpcErr::invalid("unknown part"))?;
            let text: Option<String> = db
                .query_row(&format!("SELECT {col} FROM summons WHERE id=?1"), params![run], |r| r.get(0))
                .map_err(|_| RpcErr::not_found(format!("`{run}`'s elegy is no longer on disk")))?;
            reveal["bytes"] = json!(text.unwrap_or_default());
        }
        Ok(json!({"epitaph": epitaph, "reveal": reveal}))
    }

    pub(super) fn epitaph_export(&self, p: Value) -> Result<Value, RpcErr> {
        let Range { from, to } = parse(p)?;
        if from < 1 || to < from {
            return Err(RpcErr::invalid("give a range FROM TO with 1 <= FROM <= TO"));
        }
        let db = self.db.lock().unwrap();
        let list: Vec<Value> = rows(&db, "WHERE seq BETWEEN ?1 AND ?2", &[&from, &to])?.iter().map(shown).collect();
        Ok(json!({"core_key": self.key.public().text(), "epitaphs": list}))
    }

    pub(super) fn chain_head(&self) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let all = rows(&db, "", &[])?;
        let link_hashes: Vec<String> = all.iter().map(|r| epitaph::hash(&r.link)).collect();
        let merkle_root = epitaph::merkle_root(&link_hashes);
        Ok(match all.last() {
            Some(last) => {
                let hash = epitaph::hash(&last.link);
                json!({
                    "seq": last.seq,
                    "hash": hash,
                    "mark": epitaph::mark(last.seq, &hash),
                    "merkle_root": merkle_root,
                    "count": link_hashes.len(),
                })
            }
            None => json!({"seq": 0, "merkle_root": merkle_root, "count": 0}),
        })
    }

    /// Generate a short Merkle inclusion proof for `run` against the current chain's Merkle root.
    pub(super) fn epitaph_prove(&self, p: Value) -> Result<Value, RpcErr> {
        let Run { run } = parse(p)?;
        let db = self.db.lock().unwrap();
        let all = rows(&db, "", &[])?;
        let idx = all
            .iter()
            .position(|r| r.run == run)
            .ok_or_else(|| RpcErr::not_found(format!("no epitaph for `{run}`")))?;
        let link_hashes: Vec<String> = all.iter().map(|r| epitaph::hash(&r.link)).collect();
        let merkle_root = epitaph::merkle_root(&link_hashes);
        let proof = epitaph::merkle_proof(&link_hashes, idx).unwrap_or_default();
        Ok(json!({
            "run": run,
            "seq": all[idx].seq,
            "leaf_hash": link_hashes[idx],
            "merkle_root": merkle_root,
            "count": link_hashes.len(),
            "proof": proof,
        }))
    }

    /// Check this core's whole chain against its key and its elegies.
    pub(super) fn chain_verify(&self, p: Value) -> Result<Value, RpcErr> {
        let args: VerifyArgs = if p.as_object().is_some_and(|m| m.is_empty()) { VerifyArgs::default() } else { parse(p)? };
        let key = self.key.public();
        let db = self.db.lock().unwrap();
        let all = rows(&db, "", &[])?;
        let (mut prev, mut expected, mut problem, mut unchecked, mut hashes) =
            (ZEROS.to_string(), 1i64, None::<(i64, String)>, Vec::<String>::new(), HashMap::<i64, String>::new());
        for r in &all {
            let fail = |seq: i64, why: String| Some((seq, why));
            if r.seq != expected {
                problem = fail(expected, format!("epitaph {expected} is missing or out of order"));
                break;
            }
            let link = match epitaph::check_link(&r.link, &r.sig, &key) {
                Ok(l) => l,
                Err(e) => {
                    problem = fail(r.seq, e);
                    break;
                }
            };
            if link["seq"].as_i64() != Some(r.seq) {
                problem = fail(r.seq, "the stored position differs from the signed position".into());
                break;
            }
            if link["prev"].as_str() != Some(prev.as_str()) {
                problem = fail(r.seq, "does not follow the previous epitaph".into());
                break;
            }
            let body = match epitaph::check_body(&link, &r.body) {
                Ok(b) => b,
                Err(e) => {
                    problem = fail(r.seq, e);
                    break;
                }
            };
            if body["run"].as_str() != Some(r.run.as_str()) {
                problem = fail(r.seq, "the stored run differs from the signed run".into());
                break;
            }
            let again = match body["kind"].as_str() {
                Some("pact") => recheck_pact(&db, &r.run, &body),
                Some("petition") => recheck_petition(&db, &r.run, &body),
                _ => recheck(&db, &r.run, &body),
            };
            match again {
                Ok(None) => {}
                Ok(Some(why)) => {
                    problem = fail(r.seq, why);
                    break;
                }
                Err(()) => unchecked.push(r.run.clone()),
            }
            prev = epitaph::hash(&r.link);
            if r.hash != prev {
                problem = fail(r.seq, "the stored hash column differs from the signed link".into());
                break;
            }
            hashes.insert(r.seq, prev.clone());
            expected += 1;
        }
        drop(db);
        let mut out = json!({
            "ok": problem.is_none(), "checked": hashes.len(), "not_checkable": unchecked,
            "problem": problem.map(|(seq, reason)| json!({"seq": seq, "reason": reason})),
            "witnesses": self.witness_status(), "timestamps": self.timestamp_status(args.tsa_ca.as_deref()),
        });
        // Countersignatures or timestamps for heads this chain no longer has,
        // invalid countersignatures, or failed timestamp checks mean verification failed.
        let witness_bad = out["witnesses"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|x| x["matches"] == false || x["valid"] == false);
        let ts_bad = out["timestamps"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|x| {
                x["matches"] == false
                    || x["verdict"].as_str().is_some_and(|v| v.starts_with("failed"))
            });
        if witness_bad || ts_bad {
            out["ok"] = json!(false);
            out["disagreements"] = json!(true);
        }
        if let Some(m) = &args.mark {
            let (seq, hash) = epitaph::parse_mark(m).ok_or_else(|| RpcErr::invalid("not a chain mark (summonr-mark:v1:SEQ:HASH)"))?;
            match hashes.get(&seq) {
                Some(h) if *h == hash => out["mark"] = json!({"seq": seq, "added_since": expected - 1 - seq}),
                Some(_) => {
                    out["ok"] = json!(false);
                    out["mark"] = json!({"seq": seq, "error": format!("the chain no longer contains this mark: seq {seq} has changed")});
                }
                None => {
                    out["ok"] = json!(false);
                    out["mark"] = json!({"seq": seq, "error": format!("the chain does not verify as far as seq {seq}")});
                }
            }
        }
        Ok(out)
    }
}
