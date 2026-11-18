//! Durable result-work coordination. Claims fence commits; they do not fence external systems.
use crate::rpc::RpcErr;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS jobs(name TEXT PRIMARY KEY, spec TEXT NOT NULL, created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS job_items(job TEXT NOT NULL,id TEXT NOT NULL,version TEXT NOT NULL,args TEXT NOT NULL,
 ordinal INTEGER NOT NULL,state TEXT NOT NULL DEFAULT 'ready',generation INTEGER NOT NULL DEFAULT 0,
 owner TEXT,expires INTEGER,result TEXT,PRIMARY KEY(job,id));
CREATE INDEX IF NOT EXISTS job_ready ON job_items(job,state,ordinal);
CREATE TABLE IF NOT EXISTS job_attempts(job TEXT NOT NULL,item TEXT NOT NULL,generation INTEGER NOT NULL,
 owner TEXT NOT NULL,started INTEGER NOT NULL,expires INTEGER NOT NULL,state TEXT NOT NULL,
 summon TEXT,result TEXT,PRIMARY KEY(job,item,generation));";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub job: String,
    pub ritual: String,
    pub version: Option<u32>,
    pub items: Vec<Item>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub id: String,
    pub version: String,
    pub args: Map<String, Value>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Claim {
    pub job: String,
    pub item: String,
    pub version: String,
    pub owner: String,
    pub generation: i64,
    pub expires: i64,
    pub args: Map<String, Value>,
}
fn db(e: rusqlite::Error) -> RpcErr {
    RpcErr::new("storage_failed", 11, e.to_string())
}
fn name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 200 && !s.chars().any(char::is_control)
}

pub fn create(
    conn: &mut Connection,
    manifest: &Manifest,
    spec: &Value,
    now: i64,
) -> Result<Value, RpcErr> {
    if !name(&manifest.job) || manifest.items.is_empty() || manifest.items.len() > 10000 {
        return Err(RpcErr::invalid(
            "job needs a name (1..200 bytes) and 1..10000 items",
        ));
    }
    let mut ids = HashSet::new();
    for item in &manifest.items {
        if !name(&item.id) || !name(&item.version) || !ids.insert(&item.id) {
            return Err(RpcErr::invalid(
                "items need unique IDs and explicit nonempty versions (1..200 bytes)",
            ));
        }
    }
    let tx = conn.transaction().map_err(db)?;
    if tx
        .query_row("SELECT 1 FROM jobs WHERE name=?1", [&manifest.job], |_| {
            Ok(())
        })
        .optional()
        .map_err(db)?
        .is_some()
    {
        return Err(RpcErr::conflict(
            "job already exists; jobs and item inputs are immutable",
        ));
    }
    tx.execute(
        "INSERT INTO jobs VALUES (?1,?2,?3)",
        params![manifest.job, spec.to_string(), now],
    )
    .map_err(db)?;
    for (i, item) in manifest.items.iter().enumerate() {
        tx.execute(
            "INSERT INTO job_items(job,id,version,args,ordinal) VALUES (?1,?2,?3,?4,?5)",
            params![
                manifest.job,
                item.id,
                item.version,
                serde_json::to_string(&item.args).unwrap(),
                i as i64
            ],
        )
        .map_err(db)?;
    }
    tx.commit().map_err(db)?;
    Ok(json!({"job":manifest.job,"items":manifest.items.len(),"contract":spec}))
}

pub fn spec(conn: &Connection, job: &str) -> Result<Value, RpcErr> {
    let raw: String = conn
        .query_row("SELECT spec FROM jobs WHERE name=?1", [job], |r| r.get(0))
        .optional()
        .map_err(db)?
        .ok_or_else(|| RpcErr::not_found("no such job"))?;
    serde_json::from_str(&raw).map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))
}
fn expire(tx: &Transaction, job: &str, now: i64) -> Result<(), RpcErr> {
    tx.execute(
        "UPDATE job_attempts SET state='expired' WHERE job=?1 AND state='claimed' AND expires<=?2",
        params![job, now],
    )
    .map_err(db)?;
    tx.execute("UPDATE job_items SET state='ready',owner=NULL,expires=NULL WHERE job=?1 AND state='claimed' AND expires<=?2",params![job,now]).map_err(db)?;
    Ok(())
}

pub fn claim(
    conn: &mut Connection,
    job: &str,
    owner: &str,
    count: usize,
    lease_ms: i64,
    now: i64,
) -> Result<Vec<Claim>, RpcErr> {
    if !(1..=100).contains(&count) || !(1000..=3600000).contains(&lease_ms) || !name(owner) {
        return Err(RpcErr::invalid("count must be 1..100 and lease 1s..1h"));
    }
    let expires = now
        .checked_add(lease_ms)
        .ok_or_else(|| RpcErr::invalid("lease overflow"))?;
    spec(conn, job)?;
    let tx = conn.transaction().map_err(db)?;
    expire(&tx, job, now)?;
    let items = {
        let mut q=tx.prepare("SELECT id,version,args,generation FROM job_items WHERE job=?1 AND state='ready' ORDER BY ordinal LIMIT ?2").map_err(db)?;
        let rows = q
            .query_map(params![job, count as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)?
    };
    let mut claims = Vec::new();
    for (item, version, args, generation) in items {
        let generation = generation
            .checked_add(1)
            .ok_or_else(|| RpcErr::conflict("claim generation exhausted"))?;
        let args = serde_json::from_str(&args)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        tx.execute("UPDATE job_items SET state='claimed',generation=?3,owner=?4,expires=?5 WHERE job=?1 AND id=?2",params![job,item,generation,owner,expires]).map_err(db)?;
        tx.execute("INSERT INTO job_attempts(job,item,generation,owner,started,expires,state) VALUES (?1,?2,?3,?4,?5,?6,'claimed')",params![job,item,generation,owner,now,expires]).map_err(db)?;
        claims.push(Claim {
            job: job.into(),
            item,
            version,
            args,
            owner: owner.into(),
            generation,
            expires,
        });
    }
    tx.commit().map_err(db)?;
    Ok(claims)
}

pub fn check(conn: &Connection, c: &Claim, now: i64) -> Result<(), RpcErr> {
    let exists=conn.query_row("SELECT 1 FROM job_items WHERE job=?1 AND id=?2 AND version=?3 AND owner=?4 AND generation=?5 AND state='claimed' AND expires>?6",
        params![c.job,c.item,c.version,c.owner,c.generation,now], |_|Ok(())).optional().map_err(db)?.is_some();
    if exists {
        Ok(())
    } else {
        Err(RpcErr::conflict(
            "job claim expired or superseded; result cannot be committed",
        ))
    }
}

pub fn attach(conn: &Connection, c: &Claim, summon: &str, now: i64) -> Result<(), RpcErr> {
    check(conn, c, now)?;
    conn.execute(
        "UPDATE job_attempts SET summon=?4 WHERE job=?1 AND item=?2 AND generation=?3",
        params![c.job, c.item, c.generation, summon],
    )
    .map_err(db)?;
    Ok(())
}

pub fn finish(
    conn: &mut Connection,
    c: &Claim,
    outcome: &str,
    result: &Value,
    now: i64,
) -> Result<bool, RpcErr> {
    let tx = conn.transaction().map_err(db)?;
    let valid = match check(&tx, c, now) {
        Ok(()) => true,
        Err(e) if e.code == "state_conflict" => false,
        Err(e) => return Err(e),
    };
    let state = if outcome == "returned" {
        "done"
    } else {
        "failed"
    };
    let recorded=tx.execute("UPDATE job_attempts SET state=?4,result=?5 WHERE job=?1 AND item=?2 AND generation=?3 AND owner=?6 AND result IS NULL",
        params![c.job,c.item,c.generation,if valid {state} else {"stale"},result.to_string(),c.owner]).map_err(db)?;
    if recorded == 0 {
        return Err(RpcErr::conflict("attempt was already completed"));
    }
    if valid {
        tx.execute("UPDATE job_items SET state=?3,result=?4,owner=NULL,expires=NULL WHERE job=?1 AND id=?2",params![c.job,c.item,state,result.to_string()]).map_err(db)?;
    }
    tx.commit().map_err(db)?;
    Ok(valid)
}

pub fn recover(conn: &Connection) -> Result<(), RpcErr> {
    conn.execute_batch("BEGIN IMMEDIATE; UPDATE job_attempts SET state='interrupted' WHERE state='claimed'; UPDATE job_items SET state='ready',owner=NULL,expires=NULL WHERE state='claimed'; COMMIT;").map_err(db)
}

pub fn retry(conn: &Connection, job: &str, item: &str) -> Result<Value, RpcErr> {
    let n=conn.execute("UPDATE job_items SET state='ready',result=NULL WHERE job=?1 AND id=?2 AND state='failed'",params![job,item]).map_err(db)?;
    if n != 1 {
        return Err(RpcErr::conflict(
            "only a failed item can be explicitly retried",
        ));
    }
    Ok(json!({"job":job,"item":item,"state":"ready"}))
}

pub fn status(conn: &Connection, job: &str, offset: usize) -> Result<Value, RpcErr> {
    let contract = spec(conn, job)?;
    let mut counts = Map::new();
    let mut q = conn
        .prepare("SELECT state,COUNT(*) FROM job_items WHERE job=?1 GROUP BY state")
        .map_err(db)?;
    for row in q
        .query_map([job], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(db)?
    {
        let (state, count) = row.map_err(db)?;
        counts.insert(state, json!(count));
    }
    let mut q=conn.prepare("SELECT id,version,state,generation,owner,expires,result FROM job_items WHERE job=?1 ORDER BY ordinal LIMIT 100 OFFSET ?2").map_err(db)?;
    let rows=q.query_map(params![job,offset as i64],|r|Ok(json!({"id":r.get::<_,String>(0)?,"version":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"generation":r.get::<_,i64>(3)?,"owner":r.get::<_,Option<String>>(4)?,"expires":r.get::<_,Option<i64>>(5)?,"record":r.get::<_,Option<String>>(6)?}))).map_err(db)?;
    let mut items = rows.collect::<Result<Vec<_>, _>>().map_err(db)?;
    for item in &mut items {
        if let Some(raw) = item["record"].as_str() {
            item["record"] = serde_json::from_str(raw)
                .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        }
    }
    let usage=conn.query_row("SELECT COUNT(*),COALESCE(SUM(s.requests),0),COALESCE(SUM(s.tokens),0) FROM job_attempts a LEFT JOIN summons s ON s.id=a.summon WHERE a.job=?1",[job],|r|Ok(json!({"attempts":r.get::<_,i64>(0)?,"requests":r.get::<_,i64>(1)?,"reported_tokens":r.get::<_,i64>(2)?}))).map_err(db)?;
    let total: i64 = counts.values().filter_map(Value::as_i64).sum();
    Ok(
        json!({"job":job,"contract":contract,"counts":counts,"usage":usage,"items":items,"offset":offset,"total":total,"next_offset":if offset+items.len()<total as usize {Some(offset+items.len())} else {None}}),
    )
}

pub fn attempts(conn: &Connection, job: &str, item: &str) -> Result<Value, RpcErr> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM job_items WHERE job=?1 AND id=?2",
            params![job, item],
            |_| Ok(()),
        )
        .optional()
        .map_err(db)?
        .is_some();
    if !exists {
        return Err(RpcErr::not_found("no such job item"));
    }
    let mut q=conn.prepare("SELECT generation,owner,started,expires,state,summon,result FROM job_attempts WHERE job=?1 AND item=?2 ORDER BY generation DESC LIMIT 100").map_err(db)?;
    let rows=q.query_map(params![job,item],|r|Ok(json!({"generation":r.get::<_,i64>(0)?,"owner":r.get::<_,String>(1)?,"started":r.get::<_,i64>(2)?,"expires":r.get::<_,i64>(3)?,"state":r.get::<_,String>(4)?,"summon":r.get::<_,Option<String>>(5)?,"record":r.get::<_,Option<String>>(6)?}))).map_err(db)?;
    let mut attempts = rows.collect::<Result<Vec<_>, _>>().map_err(db)?;
    for attempt in &mut attempts {
        if let Some(raw) = attempt["record"].as_str() {
            attempt["record"] = serde_json::from_str(raw)
                .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        }
    }
    Ok(json!({"job":job,"item":item,"latest_attempts":attempts,"limit":100}))
}

impl crate::requests::Validate for Manifest {}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch("CREATE TABLE summons(id TEXT,requests INTEGER,tokens INTEGER)")
            .unwrap();
        let m = Manifest {
            job: "sorting".into(),
            ritual: "classify".into(),
            version: Some(1),
            items: (0..20)
                .map(|i| Item {
                    id: format!("item-{i}"),
                    version: "v1".into(),
                    args: Map::new(),
                })
                .collect(),
        };
        create(&mut c, &m, &json!({"target":"classify"}), 0).unwrap();
        c
    }
    #[test]
    fn competing_workers_claim_disjoint_batches() {
        let conn = std::sync::Arc::new(std::sync::Mutex::new(setup()));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|i| {
                let c = conn.clone();
                let b = barrier.clone();
                std::thread::spawn(move || {
                    b.wait();
                    claim(
                        &mut c.lock().unwrap(),
                        "sorting",
                        &format!("worker-{i}"),
                        10,
                        1000,
                        0,
                    )
                    .unwrap()
                })
            })
            .collect();
        barrier.wait();
        let batches: Vec<_> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        assert_eq!(batches.len(), 20);
        assert_eq!(
            batches
                .iter()
                .map(|c| &c.item)
                .collect::<HashSet<_>>()
                .len(),
            20
        );
        assert!(
            claim(&mut conn.lock().unwrap(), "sorting", "third", 1, 1000, 1)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn expired_worker_cannot_commit_over_new_owner() {
        let mut conn = setup();
        let old = claim(&mut conn, "sorting", "old", 1, 1000, 0)
            .unwrap()
            .remove(0);
        assert!(check(&conn, &old, 999).is_ok());
        assert!(check(&conn, &old, 1000).is_err());
        let next = claim(&mut conn, "sorting", "next", 1, 1000, 1000)
            .unwrap()
            .remove(0);
        assert_eq!(old.item, next.item);
        assert!(next.generation > old.generation);
        assert!(!finish(&mut conn, &old, "returned", &json!({"answer":"old"}), 1001).unwrap());
        assert!(finish(&mut conn, &next, "returned", &json!({"answer":"new"}), 1002).unwrap());
        assert!(finish(&mut conn, &next, "returned", &json!({}), 1003).is_err());
        let status = status(&conn, "sorting", 0).unwrap();
        assert_eq!(status["items"][0]["record"]["answer"], "new");
        assert_eq!(
            attempts(&conn, "sorting", &old.item).unwrap()["latest_attempts"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
    #[test]
    fn expired_lease_is_not_revived_by_late_success() {
        let mut conn = setup();
        let old = claim(&mut conn, "sorting", "old", 1, 1000, 0)
            .unwrap()
            .remove(0);
        assert!(!finish(&mut conn, &old, "returned", &json!({"answer":1}), 1000).unwrap());
        let next = claim(&mut conn, "sorting", "next", 1, 1000, 1001)
            .unwrap()
            .remove(0);
        assert_eq!(old.item, next.item);
    }
    #[test]
    fn recovery_preserves_attempts_and_invalidates_old_claims() {
        let mut conn = setup();
        let old = claim(&mut conn, "sorting", "old", 1, 1000, 0)
            .unwrap()
            .remove(0);
        attach(&conn, &old, "s-old", 0).unwrap();
        recover(&conn).unwrap();
        assert!(check(&conn, &old, 1).is_err());
        let attempts = attempts(&conn, "sorting", &old.item).unwrap();
        assert_eq!(attempts["latest_attempts"][0]["state"], "interrupted");
        assert_eq!(attempts["latest_attempts"][0]["summon"], "s-old");
        let new = claim(&mut conn, "sorting", "new", 1, 1000, 1)
            .unwrap()
            .remove(0);
        assert_eq!(new.generation, 2);
    }
    #[test]
    fn failure_requires_explicit_retry_and_history_survives() {
        let mut conn = setup();
        let c = claim(&mut conn, "sorting", "worker", 20, 1000, 0).unwrap();
        finish(
            &mut conn,
            &c[0],
            "abstained",
            &json!({"outcome":"abstained"}),
            1,
        )
        .unwrap();
        assert!(claim(&mut conn, "sorting", "other", 1, 1000, 2)
            .unwrap()
            .is_empty());
        retry(&conn, "sorting", &c[0].item).unwrap();
        assert!(retry(&conn, "sorting", &c[1].item).is_err());
        let new = claim(&mut conn, "sorting", "other", 1, 1000, 2)
            .unwrap()
            .remove(0);
        assert_eq!(new.generation, 2);
        assert_eq!(
            attempts(&conn, "sorting", &new.item).unwrap()["latest_attempts"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
    #[test]
    fn invalid_manifests_and_limits_do_not_reserve_work() {
        let mut conn = setup();
        let m:Manifest=serde_json::from_value(json!({"job":"bad","ritual":"r","items":[{"id":"same","version":"1","args":{}},{"id":"same","version":"2","args":{}}]})).unwrap();
        assert!(create(&mut conn, &m, &json!({}), 0).is_err());
        assert!(spec(&conn, "bad").is_err());
        for (count, lease) in [(0, 1000), (101, 1000), (1, 0), (1, 3600001)] {
            assert!(claim(&mut conn, "sorting", "w", count, lease, 0).is_err());
        }
        assert_eq!(
            claim(&mut conn, "sorting", "w", 100, 1000, 0)
                .unwrap()
                .len(),
            20
        );
    }
}
