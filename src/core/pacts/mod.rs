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

pub(super) fn sql(e: rusqlite::Error) -> RpcErr {
    RpcErr::new("storage_failed", 11, e.to_string())
}

/// The stored terms of a pact.
pub(in crate::core) fn terms_of(conn: &rusqlite::Connection, id: &str) -> Result<Terms, RpcErr> {
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

pub(super) fn insert(tx: &rusqlite::Transaction, id: &str, e: &Entry, s: &Signed) -> rusqlite::Result<()> {
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
pub(super) struct Admit {
    name: String,
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Named {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Post {
    pact: String,
    entry: Signed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Sync {
    pact: String,
    after: i64,
}

pub(super) fn parse<T: serde::de::DeserializeOwned>(p: Value) -> Result<T, RpcErr> {
    serde_json::from_value(p).map_err(|e| RpcErr::invalid(e.to_string()))
}

pub(super) fn key_id_of(text: &str) -> String {
    crate::keys::PublicKey::parse(text).map(|k| k.id()).unwrap_or_default()
}


mod calls;
mod export;
mod lifecycle;
mod thread;
#[cfg(test)]
pub(in crate::core) use calls::next_poll_ms;
pub(in crate::core) use calls::{PactPart, PactTag};
use calls::used;
use lifecycle::{Row, Which};
