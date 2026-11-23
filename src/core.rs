//! The persistent core. It owns every summon; the CLI and the observer are
//! thin clients of its socket. Closing a client never stops work.

mod assays;
pub(crate) mod audit;
mod drills;
mod experiments;
mod job_service;
mod petitions;
mod epitaphs;
mod familiars;
mod operators;
pub(crate) mod rbac;
mod signed;
mod witness;
mod pacts;
mod reliquary;
mod rites;
mod sessions;

use crate::backend;
use crate::exec::{Finished, Meter, Run};
use crate::jobs;
use crate::outcome::Outcome;
use crate::reagent;
use crate::records::{Petition, PetitionState};
use crate::requests::{self, decode};
use crate::ritual::{self, Effect, Essence, Grant, Requirement, Ritual};
use crate::rpc::{read_frame, write_frame, RpcErr};
use crate::util::{new_id, now_ms, sha256_hex, Config, Paths};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS summons (
  id TEXT PRIMARY KEY, parent TEXT, series TEXT, ritual TEXT NOT NULL, version INTEGER NOT NULL,
  hash TEXT NOT NULL, model TEXT, status TEXT NOT NULL, outcome TEXT, started INTEGER NOT NULL,
  ended INTEGER, done INTEGER NOT NULL DEFAULT 0, total INTEGER NOT NULL DEFAULT 0,
  requests INTEGER NOT NULL DEFAULT 0, tokens INTEGER NOT NULL DEFAULT 0,
  inputs TEXT, essence TEXT, result TEXT, error TEXT, cleanup TEXT);
CREATE TABLE IF NOT EXISTS events (
  summon TEXT NOT NULL, seq INTEGER NOT NULL, ts INTEGER NOT NULL, kind TEXT NOT NULL,
  text TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY (summon, seq));
CREATE TABLE IF NOT EXISTS rituals (
  name TEXT NOT NULL, version INTEGER NOT NULL, hash TEXT NOT NULL, def TEXT NOT NULL,
  created INTEGER NOT NULL, PRIMARY KEY (name, version));
CREATE TABLE IF NOT EXISTS grants (
  ritual TEXT NOT NULL, grant_json TEXT NOT NULL, created INTEGER NOT NULL,
  PRIMARY KEY (ritual, grant_json));
CREATE TABLE IF NOT EXISTS epitaphs (
  seq INTEGER PRIMARY KEY, run TEXT NOT NULL UNIQUE, link TEXT NOT NULL, body TEXT NOT NULL,
  sig TEXT NOT NULL, hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS epitaph_salts (
  run TEXT NOT NULL, part TEXT NOT NULL, salt TEXT NOT NULL, fp TEXT NOT NULL, PRIMARY KEY (run, part));
CREATE TABLE IF NOT EXISTS wards (
  name TEXT PRIMARY KEY, key TEXT NOT NULL, key_id TEXT NOT NULL UNIQUE, created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS witnessed (
  ward TEXT PRIMARY KEY, seq INTEGER NOT NULL, hash TEXT NOT NULL, time INTEGER NOT NULL, sig TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS forks (
  ward TEXT NOT NULL, ward_key TEXT NOT NULL, held_seq INTEGER, held_hash TEXT, offered_from_seq INTEGER,
  offered_from_hash TEXT, offered TEXT NOT NULL, time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS witnessings (
  peer TEXT NOT NULL, seq INTEGER NOT NULL, hash TEXT NOT NULL, time INTEGER NOT NULL, sig TEXT NOT NULL, PRIMARY KEY (peer, seq));
CREATE TABLE IF NOT EXISTS timestamps (
  seq INTEGER NOT NULL, hash TEXT NOT NULL, url TEXT NOT NULL, response BLOB NOT NULL, time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS familiars (
  name TEXT PRIMARY KEY, kind TEXT NOT NULL, key TEXT NOT NULL, key_id TEXT NOT NULL UNIQUE,
  created INTEGER NOT NULL, revoked INTEGER, allowed TEXT NOT NULL DEFAULT '[]',
  expires_at INTEGER);
CREATE TABLE IF NOT EXISTS peers (
  name TEXT PRIMARY KEY, url TEXT NOT NULL, key TEXT NOT NULL, key_id TEXT NOT NULL, created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS nonces (
  key_id TEXT NOT NULL, nonce TEXT NOT NULL, expires INTEGER NOT NULL, PRIMARY KEY (key_id, nonce));
CREATE TABLE IF NOT EXISTS petitions (
  id TEXT PRIMARY KEY, summon TEXT NOT NULL, ritual TEXT NOT NULL, step TEXT NOT NULL,
  operation TEXT NOT NULL, effect TEXT NOT NULL, arguments TEXT NOT NULL, verify TEXT,
  selection TEXT NOT NULL, because TEXT NOT NULL, status TEXT NOT NULL, created INTEGER NOT NULL,
  authority TEXT,
  decided INTEGER, result TEXT);
CREATE TABLE IF NOT EXISTS pacts (
  id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, peer TEXT NOT NULL, side TEXT NOT NULL,
  terms TEXT NOT NULL, state TEXT NOT NULL, local_limits TEXT, created INTEGER NOT NULL, problem TEXT);
CREATE TABLE IF NOT EXISTS pact_entries (
  pact TEXT NOT NULL, n INTEGER NOT NULL, bytes TEXT NOT NULL, sig TEXT NOT NULL, hash TEXT NOT NULL,
  by TEXT NOT NULL, PRIMARY KEY (pact, n));
CREATE TABLE IF NOT EXISTS pact_forks (
  pact TEXT NOT NULL, a TEXT NOT NULL, b TEXT NOT NULL, seen INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pact_calls (
  pact TEXT NOT NULL, call INTEGER NOT NULL, state TEXT NOT NULL, run TEXT, asked_by TEXT,
  deadline INTEGER NOT NULL, usage TEXT, idem TEXT, PRIMARY KEY (pact, call));
CREATE TABLE IF NOT EXISTS pact_outbox (
  ord INTEGER PRIMARY KEY AUTOINCREMENT, pact TEXT NOT NULL, kind TEXT NOT NULL, call INTEGER,
  data TEXT NOT NULL, bytes TEXT NOT NULL, sig TEXT NOT NULL, hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS counterparties (
  name TEXT PRIMARY KEY, key TEXT NOT NULL, key_id TEXT NOT NULL UNIQUE, created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS invocations (
  caller TEXT NOT NULL, key TEXT NOT NULL, request_hash TEXT NOT NULL, summon TEXT NOT NULL,
  PRIMARY KEY(caller,key));
CREATE TABLE IF NOT EXISTS baselines (
  ritual TEXT NOT NULL, version INTEGER NOT NULL, model TEXT, set_name TEXT NOT NULL,
  set_hash TEXT NOT NULL, set_json TEXT NOT NULL, fingerprint TEXT NOT NULL, account TEXT NOT NULL,
  created INTEGER NOT NULL, PRIMARY KEY (ritual, version));
CREATE TABLE IF NOT EXISTS checkpoints (
  summon TEXT NOT NULL, path TEXT NOT NULL, data TEXT NOT NULL,
  PRIMARY KEY (summon, path));
CREATE TABLE IF NOT EXISTS effect_fence (
  fence_key TEXT PRIMARY KEY,
  summon TEXT NOT NULL,
  step TEXT NOT NULL,
  operation TEXT NOT NULL,
  effect TEXT NOT NULL,
  args_hash TEXT NOT NULL,
  state TEXT NOT NULL,
  output TEXT,
  evidence TEXT,
  error TEXT,
  updated INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS assay_trials (
  series TEXT PRIMARY KEY, ritual TEXT NOT NULL, trials TEXT NOT NULL,
  created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  name TEXT,
  model TEXT,
  system_prompt TEXT NOT NULL DEFAULT '',
  settings TEXT NOT NULL DEFAULT '{}',
  essence TEXT NOT NULL DEFAULT '{}',
  bindings TEXT NOT NULL DEFAULT '{}',
  turns TEXT NOT NULL DEFAULT '[]',
  used_requests INTEGER NOT NULL DEFAULT 0,
  used_tokens INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'open',
  created INTEGER NOT NULL,
  ended INTEGER);
CREATE TABLE IF NOT EXISTS experiments (
  id TEXT PRIMARY KEY,
  ritual TEXT NOT NULL,
  spec TEXT NOT NULL,
  trials TEXT NOT NULL,
  judged TEXT,
  created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS reliquary_packs (
  name TEXT NOT NULL,
  version INTEGER NOT NULL,
  hash TEXT NOT NULL UNIQUE,
  author_key TEXT NOT NULL,
  pack_json TEXT NOT NULL,
  created INTEGER NOT NULL,
  retracted TEXT,
  PRIMARY KEY (name, version));
CREATE TABLE IF NOT EXISTS reliquary_peers (
  peer TEXT PRIMARY KEY,
  added INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS reliquary_evidence (
  ritual TEXT NOT NULL,
  version INTEGER NOT NULL,
  author_key TEXT NOT NULL,
  pack_hash TEXT NOT NULL,
  evidence_json TEXT NOT NULL,
  imported INTEGER NOT NULL,
  PRIMARY KEY (ritual, version, pack_hash));
CREATE TABLE IF NOT EXISTS rites (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  hash TEXT NOT NULL,
  def_json TEXT NOT NULL,
  inputs_json TEXT NOT NULL,
  status TEXT NOT NULL,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS rite_stages (
  rite TEXT NOT NULL,
  stage TEXT NOT NULL,
  ritual TEXT NOT NULL,
  version INTEGER NOT NULL,
  hash TEXT NOT NULL,
  status TEXT NOT NULL,
  summon_id TEXT,
  output_json TEXT,
  gate_status TEXT NOT NULL,
  petition_id TEXT,
  accepted INTEGER NOT NULL DEFAULT 0,
  reopened_reason TEXT,
  PRIMARY KEY (rite, stage));
CREATE TABLE IF NOT EXISTS rite_assumptions (
  rite TEXT NOT NULL,
  id TEXT NOT NULL,
  statement TEXT NOT NULL,
  status TEXT NOT NULL,
  falsified_reason TEXT,
  updated INTEGER NOT NULL,
  PRIMARY KEY (rite, id));
CREATE TABLE IF NOT EXISTS rite_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  rite TEXT NOT NULL,
  ts INTEGER NOT NULL,
  kind TEXT NOT NULL,
  detail_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS audit_log (
  seq INTEGER PRIMARY KEY,
  ts INTEGER NOT NULL,
  actor TEXT NOT NULL,
  action TEXT NOT NULL,
  target TEXT NOT NULL,
  outcome TEXT NOT NULL,
  detail TEXT NOT NULL,
  prev_hash TEXT NOT NULL,
  hash TEXT NOT NULL,
  sig TEXT NOT NULL);
CREATE TRIGGER IF NOT EXISTS audit_log_no_update
  BEFORE UPDATE ON audit_log
  BEGIN SELECT RAISE(ABORT, 'audit_log is append-only'); END;
CREATE TRIGGER IF NOT EXISTS audit_log_no_delete
  BEFORE DELETE ON audit_log
  BEGIN SELECT RAISE(ABORT, 'audit_log is append-only'); END;
CREATE TABLE IF NOT EXISTS operators (
  name TEXT PRIMARY KEY,
  role TEXT NOT NULL,
  key TEXT NOT NULL,
  key_id TEXT NOT NULL UNIQUE,
  created INTEGER NOT NULL,
  revoked INTEGER);
CREATE TABLE IF NOT EXISTS rbac_roles (
  name TEXT PRIMARY KEY,
  description TEXT NOT NULL,
  builtin INTEGER NOT NULL DEFAULT 0,
  created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS rbac_role_permissions (
  role_name TEXT NOT NULL REFERENCES rbac_roles(name) ON DELETE CASCADE,
  resource_type TEXT NOT NULL,
  resource_pattern TEXT NOT NULL,
  action TEXT NOT NULL,
  PRIMARY KEY (role_name, resource_type, resource_pattern, action));
CREATE TABLE IF NOT EXISTS rbac_actor_roles (
  actor_id TEXT NOT NULL,
  role_name TEXT NOT NULL REFERENCES rbac_roles(name) ON DELETE RESTRICT,
  granted_by TEXT NOT NULL,
  granted_at INTEGER NOT NULL,
  revoked_at INTEGER,
  PRIMARY KEY (actor_id, role_name));
CREATE INDEX IF NOT EXISTS idx_rbac_actor_roles_active
  ON rbac_actor_roles(actor_id) WHERE revoked_at IS NULL;
-- A run's tree (record, brief, children, warnings) walks parent links.
CREATE INDEX IF NOT EXISTS idx_summons_parent ON summons(parent, started);
CREATE INDEX IF NOT EXISTS idx_petitions_summon ON petitions(summon);
CREATE INDEX IF NOT EXISTS idx_effect_fence_summon ON effect_fence(summon);
";


// Queries over a run's tree of summons. The tree is walked first (by
// idx_summons_parent), then each summon's rows by their own index; a
// CROSS JOIN fixes that order, or SQLite may scan every event instead.
pub(crate) const TREE_EFFECTS_SQL: &str = "WITH RECURSIVE tree(id) AS (SELECT id FROM summons WHERE id=?1 UNION ALL SELECT s.id FROM summons s JOIN tree t ON s.parent=t.id) SELECT e.kind,e.data FROM tree t CROSS JOIN events e ON e.summon=t.id WHERE e.kind IN ('op','held','check.failed') AND NOT (e.kind='op' AND json_extract(e.data,'$.effect')='pure') ORDER BY e.ts,e.seq";
pub(crate) const TREE_PETITIONS_SQL: &str = "WITH RECURSIVE tree(id) AS (SELECT id FROM summons WHERE id=?1 UNION ALL SELECT s.id FROM summons s JOIN tree t ON s.parent=t.id) SELECT p.id,p.status,p.operation FROM tree t CROSS JOIN petitions p ON p.summon=t.id ORDER BY p.created";
pub(crate) const TREE_EACH_FAILED_SQL: &str = "WITH RECURSIVE tree(id) AS (SELECT id FROM summons WHERE id=?1 UNION ALL SELECT s.id FROM summons s JOIN tree t ON s.parent=t.id) SELECT s.id, s.ritual, e.text FROM tree t CROSS JOIN events e ON e.summon=t.id JOIN summons s ON s.id=e.summon WHERE e.kind='each.failed' ORDER BY e.ts, e.seq";

pub(crate) enum FenceAcquire {
    Proceed { fence_key: String },
    Cached { output: Value, evidence: crate::records::Evidence },
    Blocked { reason: String },
}

struct EventSub {
    summon: Option<String>,
    slim: bool,
    tx: std::sync::mpsc::Sender<Value>,
}

pub struct Core {
    /// This core's identity. It signs replies now and, later, receipts.
    pub(crate) key: crate::keys::Keypair,
    /// Why the last signed request was refused (the caller sees a generic error).
    pub(crate) last_refusal: Mutex<Option<String>>,
    /// One witness submission at a time from this core, so two never race.
    pub(crate) witnessing: Mutex<()>,
    pub db: crate::store::Store,
    pub paths: Paths,
    pub cfg: Config,
    /// Top-level summon id -> cancellation flag shared with its descendants.
    pub active: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub stop: AtomicBool,
    _ownership: std::fs::File,
    storage_error: Mutex<Option<String>>,
    admission: Mutex<()>,
    event_subs: Mutex<Vec<EventSub>>,
}

pub use requests::Invoke as Spec;

pub struct Resolved {
    /// Who asked: empty for the owner, `agent`, or `familiar:NAME`.
    pub caller: String,
    pub def: Ritual,
    pub version: u32,
    pub hash: String,
    pub inputs: Value,
    pub model: Option<String>,
    pub settings: BTreeMap<String, Value>,
    pub grants: Vec<Grant>,
    pub missing: Vec<Grant>,
    pub effects: Vec<Effect>,
    pub warnings: Vec<String>,
    pub refuse: Option<String>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct AssaySet {
    pub assay_set: String,
    #[serde(default)]
    pub min_cases: Option<usize>,
    #[serde(default)]
    pub tolerances: Tolerances,
    /// A saved pure ritual that computes each case's expected result.
    #[serde(default)]
    pub touchstone: Option<String>,
    pub cases: Vec<Case>,
}

#[derive(Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct Tolerances {
    pub accuracy_drop: Option<f64>,
    pub abstention_rise: Option<f64>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub inputs: Map<String, Value>,
    #[serde(default)]
    pub expect: Map<String, Value>,
    /// What the ritual answered when this case was gleaned from a real run.
    /// Informational only; it is never used for scoring.
    #[serde(default)]
    #[allow(dead_code)]
    pub observed: Option<Value>,
    #[serde(default)]
    #[allow(dead_code)]
    pub source: Option<Value>,
}

fn is_path(target: &str) -> bool {
    target.contains('/')
        || target.ends_with(".yaml")
        || target.ends_with(".yml")
        || target.ends_with(".json")
        || target.ends_with(".py")
}

/// A missing or unreadable cassette is not found (3); one that does not
/// parse is invalid (2).
fn cassette_error(e: crate::cassette::LoadError) -> RpcErr {
    match e {
        crate::cassette::LoadError::Unreadable(m) => RpcErr::not_found(m),
        crate::cassette::LoadError::Invalid(m) => RpcErr::invalid(m),
    }
}

fn setting_value(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}

use crate::store::{error as storage_error, json_value as parse_json};

/// Wilson score interval for a proportion, as (low, high).
fn wilson(correct: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let (p, n, z) = (correct as f64 / n as f64, n as f64, 1.96);
    let denom = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / denom;
    let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / denom;
    ((center - half).max(0.0), (center + half).min(1.0))
}

impl Core {
    pub fn open(paths: Paths) -> Result<Arc<Core>, String> {
        for d in [&paths.runtime, &paths.state, &paths.config] {
            std::fs::create_dir_all(d)
                .map_err(|e| format!("cannot create {}: {e}", d.display()))?;
        }
        // Ownership precedes database recovery. A second core must never mutate live work.
        let ownership = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(paths.state.join("core.lock"))
            .map_err(|e| e.to_string())?;
        // A child process started by another thread holds a copy of a closed
        // core's lock until it execs, so a reopen may briefly find it taken.
        // A live core holds it for good; wait a moment before deciding.
        let mut locked = false;
        for _ in 0..25 {
            if unsafe { libc::flock(ownership.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                locked = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !locked {
            return Err("another core owns this state directory".into());
        }
        let cfg = Config::load(&paths)?;
        let db = Connection::open(paths.db())
            .map_err(|e| format!("cannot open the state database: {e}"))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .map_err(|e| e.to_string())?;
        db.execute_batch(SCHEMA)
            .map_err(|e| format!("cannot prepare the state database: {e}"))?;
        db.execute_batch(jobs::SCHEMA).map_err(|e| e.to_string())?;
        jobs::recover(&db).map_err(|e| e.message)?;
        add_column(&db, "petitions", "authority", "TEXT")?;
        add_column(&db, "petitions", "digest", "TEXT")?;
        add_column(&db, "summons", "familiar", "TEXT")?;
        // Petitions filed by a run under a pact; the caller may also decide them.
        for (column, decl) in [
            ("pact", "TEXT"),
            ("pact_call", "INTEGER"),
            ("needs_caller", "INTEGER NOT NULL DEFAULT 0"),
            ("needs_callee", "INTEGER NOT NULL DEFAULT 1"),
            ("caller_decision", "TEXT"),
            ("callee_decision", "TEXT"),
            ("expires", "INTEGER"),
        ] {
            add_column(&db, "petitions", column, decl)?;
        }
        migrate_principals(&db, &paths)?;
        add_column(&db, "familiars", "expires_at", "INTEGER")?;
        // Baselines recorded before assays were isolated ran live.
        add_column(&db, "baselines", "mode", "TEXT NOT NULL DEFAULT 'live'")?;
        // The touchstone {name, version, hash} a baseline's expectations came from.
        add_column(&db, "baselines", "touchstone", "TEXT")?;
        add_column(&db, "summons", "cost_microusd", "INTEGER")?;
        add_column(&db, "sessions", "used_cost_microusd", "INTEGER NOT NULL DEFAULT 0")?;
        add_column(&db, "petitions", "decided_by", "TEXT")?;
        // Work that was running when a previous core died cannot be shown as
        // done; its completion is unknown.
        let lost: Vec<String> = {
            // Deepest first, so a child is sealed before the parent that lists it.
            let mut st = db
                .prepare(
                    "WITH RECURSIVE up(id, anc, depth) AS (
                       SELECT id, parent, 0 FROM summons WHERE status != 'ended'
                       UNION ALL SELECT up.id, s.parent, up.depth + 1 FROM up JOIN summons s ON s.id = up.anc
                         WHERE up.anc IS NOT NULL AND up.depth < 64)
                     SELECT id FROM up GROUP BY id ORDER BY MAX(depth) DESC, id",
                )
                .map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        let key = load_or_create_core_key(&paths)?;
        let core = Arc::new(Core {
            key,
            last_refusal: Mutex::new(None),
            witnessing: Mutex::new(()),
            db: crate::store::Store::new(db),
            paths,
            cfg,
            active: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            _ownership: ownership,
            storage_error: Mutex::new(None),
            admission: Mutex::new(()),
            event_subs: Mutex::new(Vec::new()),
        });
        for id in lost {
            // The end event, the ended row and the epitaph commit together, so
            // a second crash cannot leave an ended run without its epitaph.
            let mut db = core.db.lock().unwrap();
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT INTO events (summon, seq, ts, kind, text, data) VALUES (?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE summon = ?1), ?2, 'summon.end', ?3, ?4)",
                params![id, now_ms(), "interrupted: the core stopped while this summon was running", json!({"outcome": "interrupted"}).to_string()],
            ).map_err(|e| e.to_string())?;
            tx.execute(
                "UPDATE summons SET status='ended', outcome='interrupted', ended=?2, error='the core stopped while this summon was running; completion cannot be established' WHERE id=?1",
                params![id, now_ms()],
            ).map_err(|e| e.to_string())?;
            epitaphs::write_epitaph(&tx, &core.key, &id).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            drop(db);
            // An interrupted run started for a pact call is delivered as failed.
            core.on_run_end(&id);
        }
        let unk_pets: Vec<String> = {
            let db = core.db.lock().unwrap();
            let mut q = db.prepare("SELECT id FROM petitions WHERE status='executing' AND pact IS NOT NULL").map_err(|e| e.to_string())?;
            let rows = q.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
            rows.filter_map(Result::ok).collect()
        };
        core.db.lock().unwrap().execute(
            "UPDATE petitions SET status='unknown', result=?1 WHERE status='executing'",
            params![json!({"error":"core interrupted after claim; effect outcome unknown; reconcile before retry"}).to_string()],
        ).map_err(|e| e.to_string())?;
        let ended_pact_runs: Vec<String> = {
            let db = core.db.lock().unwrap();
            let mut q = db
                .prepare("SELECT c.run FROM pact_calls c JOIN summons s ON s.id=c.run WHERE c.state='running' AND s.status='ended' AND c.run IS NOT NULL")
                .map_err(|e| e.to_string())?;
            let rows = q.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
            rows.filter_map(Result::ok).collect()
        };
        for run in ended_pact_runs {
            core.on_run_end(&run);
        }
        for id in unk_pets {
            let _ = core.settle_if_pact(&id);
        }
        core.storage_health().map_err(|e| e.message)?;
        core.seed_rbac()?;
        core.verify_integrity_on_startup();
        if core.storage_health().is_ok() {
            core.anchor_config_on_boot();
        }
        Ok(core)
    }

    pub(super) fn note_storage_error(&self, msg: String) {
        let mut fault = self.storage_error.lock().unwrap();
        if fault.is_none() {
            *fault = Some(msg);
        }
    }

    fn persisted(&self, result: rusqlite::Result<usize>) {
        if let Err(e) = result {
            let mut fault = self.storage_error.lock().unwrap();
            if fault.is_none() {
                *fault = Some(format!(
                    "durable storage failed: {e}; restart after repairing storage"
                ));
            }
        }
    }

    pub fn storage_health(&self) -> Result<(), RpcErr> {
        match self.storage_error.lock().unwrap().as_ref() {
            Some(e) => Err(RpcErr::new("storage_failed", 11, e.clone())),
            None => Ok(()),
        }
    }

    // ------------------------------------------------------------- storage

    pub fn emit(&self, summon: &str, kind: &str, text: &str, data: Value) {
        self.emit_with(summon, kind, text, data, true)
    }

    /// `durable: false` is for records of recomputable work (pure steps):
    /// the commit skips its fsync. Under WAL the next durable commit syncs
    /// it along with everything before it, in order, so a power loss can
    /// drop only a tail of such records, never reorder or tear them.
    pub fn emit_with(&self, summon: &str, kind: &str, text: &str, data: Value, durable: bool) {
        let ts = now_ms();
        let seq = {
            let db = self.db.lock().unwrap();
            // Events are stored before anyone can observe them.
            let res = relaxed(&db, durable, |db| {
                db.execute(
                    "INSERT INTO events (summon, seq, ts, kind, text, data)
                     VALUES (?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE summon = ?1), ?2, ?3, ?4, ?5)",
                    params![summon, ts, kind, text, data.to_string()],
                )
            });
            let ok = res.is_ok();
            self.persisted(res);
            if ok {
                db.query_row(
                    "SELECT seq FROM events WHERE rowid = last_insert_rowid()",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .ok()
            } else {
                None
            }
        };
        if let Some(seq) = seq {
            let mut subs = self.event_subs.lock().unwrap();
            subs.retain(|sub| {
                if sub.summon.as_deref().is_some_and(|s| s != summon) {
                    return true;
                }
                let mut ev = json!({"summon": summon, "seq": seq, "ts": ts, "kind": kind, "text": text});
                if !sub.slim {
                    ev["data"] = data.clone();
                }
                sub.tx.send(ev).is_ok()
            });
        }
    }

    pub fn progress(&self, id: &str, done: u64, total: u64) {
        self.progress_with(id, done, total, true)
    }

    pub fn progress_with(&self, id: &str, done: u64, total: u64, durable: bool) {
        let db = self.db.lock().unwrap();
        self.persisted(relaxed(&db, durable, |db| {
            db.execute(
                "UPDATE summons SET done=?2, total=?3 WHERE id=?1",
                params![id, done as i64, total as i64],
            )
        }));
    }

    pub fn usage(&self, id: &str, requests: u64, tokens: u64, cost_microusd: Option<u64>) {
        self.usage_with(id, requests, tokens, cost_microusd, true)
    }

    pub fn usage_with(&self, id: &str, requests: u64, tokens: u64, cost_microusd: Option<u64>, durable: bool) {
        let db = self.db.lock().unwrap();
        self.persisted(relaxed(&db, durable, |db| {
            db.execute(
                "UPDATE summons SET requests=?2, tokens=?3, cost_microusd=?4 WHERE id=?1",
                params![id, requests as i64, tokens as i64, cost_microusd.map(|c| c as i64)],
            )
        }));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_summon(
        &self,
        id: &str,
        parent: Option<&str>,
        ritual: &str,
        version: u32,
        hash: &str,
        model: Option<&str>,
        inputs: &Value,
        essence: &Essence,
        series: Option<&str>,
    ) {
        self.insert_summon_with(id, parent, ritual, version, hash, model, inputs, essence, series, true)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_summon_with(
        &self,
        id: &str,
        parent: Option<&str>,
        ritual: &str,
        version: u32,
        hash: &str,
        model: Option<&str>,
        inputs: &Value,
        essence: &Essence,
        series: Option<&str>,
        durable: bool,
    ) {
        let db = self.db.lock().unwrap();
        self.persisted(relaxed(&db, durable, |db| {
            db.execute(
                "INSERT INTO summons (id, parent, series, ritual, version, hash, model, status, started, inputs, essence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', ?8, ?9, ?10)",
                params![id, parent, series, ritual, version, hash, model, now_ms(), inputs.to_string(), serde_json::to_string(essence).unwrap_or_default()],
            )
        }));
    }

    pub fn finish_summon(&self, id: &str, fin: &Finished, cleanup: Value) {
        let text = match &fin.error {
            Some(e) => format!("{}: {e}", fin.outcome),
            None => fin.outcome.to_string(),
        };
        let result = (|| -> rusqlite::Result<usize> {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction()?;
            tx.execute("INSERT INTO events (summon, seq, ts, kind, text, data) VALUES (?1, (SELECT COALESCE(MAX(seq),0)+1 FROM events WHERE summon=?1), ?2, 'summon.end', ?3, ?4)",
                params![id, now_ms(), text, json!({"outcome":fin.outcome,"cleanup":cleanup}).to_string()])?;
            let n = tx.execute("UPDATE summons SET status='ended', outcome=?2, ended=?3, result=?4, error=?5, cleanup=?6 WHERE id=?1",
                params![id, fin.outcome.as_str(), now_ms(), fin.result.to_string(), fin.error, cleanup.to_string()])?;
            // No run ends without its epitaph: both commit together or not at all.
            epitaphs::write_epitaph(&tx, &self.key, id)?;
            // Cross-anchor every completed execution record into the hash-chained audit_log.
            let (ritual_name, familiar): (String, Option<String>) = tx
                .query_row(
                    "SELECT ritual, familiar FROM summons WHERE id=?1",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap_or_else(|_| (String::new(), None));
            let (ep_seq, ep_hash): (i64, String) = tx
                .query_row(
                    "SELECT seq, hash FROM epitaphs WHERE run=?1",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap_or_else(|_| (0, String::new()));
            let actor = familiar.unwrap_or_else(audit::current_actor);
            audit::append_audit_tx(
                &tx,
                &self.key,
                &actor,
                "summon.finished",
                id,
                fin.outcome.as_str(),
                &json!({
                    "ritual": ritual_name,
                    "epitaph_seq": ep_seq,
                    "epitaph_hash": ep_hash,
                    "error": fin.error,
                }),
            )?;
            tx.commit()?;
            Ok(n)
        })();
        self.persisted(result);
    }

    pub fn load_ritual(
        &self,
        name: &str,
        version: Option<u32>,
    ) -> Result<Option<ritual::Resolved>, RpcErr> {
        self.db.ritual(name, version)
    }
    fn grants_for(&self, name: &str) -> Result<Vec<Grant>, RpcErr> {
        self.db.grants(name)
    }
    fn baseline(&self, name: &str, version: u32) -> Result<Option<Value>, RpcErr> {
        self.db.baseline(name, version)
    }
    fn summons(
        &self,
        filter: &str,
        arg: &str,
        limit: i64,
        full: bool,
    ) -> Result<Vec<Value>, RpcErr> {
        self.db.summons(filter, arg, limit, full)
    }

    fn summon(&self, id: &str) -> Result<Value, RpcErr> {
        let mut value = self
            .summons("id = ?1", id, 1, true)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                RpcErr::not_found(format!(
                    "no summon `{id}`; `summonr graveyard` lists ended summons"
                ))
            })?;
        let root = self.root_of(id)?;
        value["cancellation_requested"] = json!(
            value["status"] != "ended"
                && self
                    .active
                    .lock()
                    .unwrap()
                    .get(&root)
                    .is_some_and(|flag| flag.load(Ordering::SeqCst))
        );
        let db = self.db.lock().unwrap();
        let mut query = db.prepare(TREE_EFFECTS_SQL)
            .map_err(|e| RpcErr::new("storage_failed",11,e.to_string()))?;
        let rows = query
            .query_map(params![id], |r| {
                let kind: String = r.get(0)?;
                let raw: String = r.get(1)?;
                Ok(json!({"kind":kind,"data":parse_json(Some(raw))?}))
            })
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        // Pure steps change nothing outside the run; their records stay in
        // the event log (`logs`) but would swamp a big run's record.
        value["effects"] = Value::Array(
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?,
        );
        let mut query = db.prepare(TREE_PETITIONS_SQL)
            .map_err(|e|RpcErr::new("storage_failed",11,e.to_string()))?;
        let rows=query.query_map(params![id],|r| Ok(json!({"id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"operation":r.get::<_,String>(2)?})))
            .map_err(|e|RpcErr::new("storage_failed",11,e.to_string()))?;
        value["petitions"] = Value::Array(
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?,
        );
        let mut warnings = Self::admission_warnings_from(&db, id);
        if let Some(list) = warnings.as_array_mut() {
            list.extend(Self::failed_item_warnings(&db, id)?);
        }
        if warnings.as_array().is_some_and(|a| !a.is_empty()) {
            value["warnings"] = warnings;
        }
        let fences = Self::fences_for_conn(&db, id)?;
        if !fences.is_empty() {
            value["fences"] = Value::Array(fences);
        }
        Ok(value)
    }

    /// One warning per `each` in the run (children included) that kept
    /// going past failed items, with the first failure. Without it, a loop
    /// with `on_failure: continue` can fail most of its items unseen.
    fn failed_item_warnings(db: &rusqlite::Connection, id: &str) -> Result<Vec<Value>, RpcErr> {
        let err = |e: rusqlite::Error| RpcErr::new("storage_failed", 11, e.to_string());
        let mut q = db
            .prepare(TREE_EACH_FAILED_SQL)
            .map_err(err)?;
        let rows = q
            .query_map(params![id], |r| {
                let (summon, ritual, text): (String, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
                Ok(if summon == id {
                    json!(format!("each {text}"))
                } else {
                    json!(format!("each {text} (in {ritual} {summon})"))
                })
            })
            .map_err(err)?;
        let mut all = rows.collect::<Result<Vec<_>, _>>().map_err(err)?;
        // A lenient loop nested in another can fail once per outer item.
        const SHOWN: usize = 10;
        if all.len() > SHOWN {
            let more = all.len() - SHOWN;
            all.truncate(SHOWN);
            all.push(json!(format!("and {more} more loops with failed items (events of kind each.failed)")));
        }
        Ok(all)
    }

    fn admission_warnings(&self, id: &str) -> Value {
        let db = self.db.lock().unwrap();
        Self::admission_warnings_from(&db, id)
    }

    fn admission_warnings_from(db: &rusqlite::Connection, id: &str) -> Value {
        db.query_row(
            "SELECT data FROM events WHERE summon=?1 AND kind='summon.start' LIMIT 1",
            params![id],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|d| d.get("warnings").cloned())
        .filter(|w| w.is_array())
        .unwrap_or_else(|| json!([]))
    }

    // ---------------------------------------------------------- resolution

    fn resolver(&self) -> impl Fn(&str, Option<u32>) -> Option<ritual::Resolved> + '_ {
        move |n, v| match self.load_ritual(n, v) {
            Ok(value) => value,
            Err(e) => {
                *self.storage_error.lock().unwrap() = Some(e.message);
                None
            }
        }
    }

    fn load_file(&self, path: &Path) -> Result<Ritual, RpcErr> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| RpcErr::not_found(format!("cannot read {}: {e}", path.display())))?;
        let mut def = ritual::parse_definition(path, &text)
            .map_err(|e| RpcErr::invalid(format!("{}: {e}", path.display())))?;
        ritual::prepare(
            &mut def,
            path.parent().unwrap_or(Path::new(".")),
            &self.resolver(),
        )
        .map_err(RpcErr::invalid)?;
        Ok(def)
    }

    fn compile(&self, p: Value) -> Result<Value, RpcErr> {
        self.compile_as(p, None)
    }

    /// Compile an inline definition. With `visible`, `ritual:` steps resolve
    /// only against those rituals, and any other name answers as missing.
    pub(crate) fn compile_as(&self, p: Value, visible: Option<&[String]>) -> Result<Value, RpcErr> {
        let p: requests::Compile = decode(p)?;
        let all = self.resolver();
        let resolver = |n: &str, v: Option<u32>| match visible {
            Some(names) if !names.iter().any(|a| a == n) => None,
            _ => all(n, v),
        };
        let mut def: Ritual = match p.definition {
            Value::String(s) => ritual::parse_file(&s).map_err(RpcErr::invalid)?,
            other => serde_json::from_value(other).map_err(|e| RpcErr::invalid(e.to_string()))?,
        };
        if !def.bindings.is_empty() {
            return Err(RpcErr::invalid(
                "inline compile_ritual cannot read host binding files; use `summonr compile FILE` locally",
            ));
        }
        ritual::prepare(&mut def, Path::new("."), &resolver).map_err(RpcErr::invalid)?;
        let (effects, return_types) = ritual::validate_with_return_types_cfg(
            &def,
            &resolver,
            &self.cfg,
            self.cfg.max_depth(),
        )
        .map_err(|errors| {
            RpcErr::invalid(ritual::contract_message(&errors)).with(json!({
                "diagnostics": errors.into_iter().map(|message| json!({
                    "code": "invalid_definition",
                    "message": message,
                })).collect::<Vec<_>>()
            }))
        })?;
        Ok(json!({
            "valid": true,
            "language_version": def.language_version,
            "hash": ritual::content_hash(&def),
            "effects": effects,
            "warnings": ritual::lint(&def),
            "return_types": return_types,
            "output_schema": def.output_schema,
            "definition": def,
        }))
    }

    /// Where a caller's named cassettes live. The owner's are in
    /// `cassettes/`; every agent, familiar and pact has its own directory,
    /// so none can replay or overwrite another's recordings.
    fn cassette_dir(&self, caller: &str) -> PathBuf {
        let dir = self.paths.state.join("cassettes");
        if caller.is_empty() {
            return dir;
        }
        let safe: String = caller
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        dir.join("callers").join(format!("{safe}-{}", &crate::util::sha256_hex(caller.as_bytes())[..8]))
    }

    /// Everything that can be decided before any work starts: the definition,
    /// typed inputs, effective model, authority, effects, and drift state.
    pub fn resolve(&self, spec: &Spec) -> Result<Resolved, RpcErr> {
        let (def, version, hash) = if is_path(&spec.target) {
            let def = self.load_file(&PathBuf::from(&spec.cwd).join(&spec.target))?;
            let hash = ritual::content_hash(&def);
            (def, 0, hash)
        } else {
            let (v, h, d) =
                self.load_ritual(&spec.target, spec.version)?
                    .ok_or_else(|| match spec.version {
                        Some(v) => RpcErr::not_found(format!(
                            "ritual `{}` has no version {v}",
                            spec.target
                        )),
                        None => RpcErr::not_found(format!(
                            "no ritual `{}` in the grimoire; `summonr grimoire` lists them",
                            spec.target
                        )),
                    })?;
            (d, v, h)
        };
        if spec.expected_hash.as_ref().is_some_and(|h| h != &hash) {
            return Err(RpcErr::conflict(
                "ritual contract changed; describe it again before running",
            ));
        }
        if let Some(c) = &spec.limits.cost {
            crate::util::parse_cost_microusd(c).map_err(RpcErr::invalid)?;
        }
        if let Some(t) = &spec.limits.time {
            crate::util::parse_duration(t).map_err(RpcErr::invalid)?;
        }
        let mut def = def;
        fn lower(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        def.essence.requests = lower(def.essence.requests, spec.limits.requests);
        def.essence.tokens = lower(def.essence.tokens, spec.limits.tokens);
        def.essence.tool_calls = lower(def.essence.tool_calls, spec.limits.tool_calls);
        def.essence.items = lower(def.essence.items, spec.limits.items);
        if let Some(c) = &spec.limits.cost {
            let incoming = crate::util::parse_cost_microusd(c).map_err(RpcErr::invalid)?;
            let existing = def
                .essence
                .cost
                .as_deref()
                .map(crate::util::parse_cost_microusd)
                .transpose()
                .map_err(RpcErr::invalid)?;
            def.essence.cost = Some(crate::util::fmt_cost_microusd(
                lower(existing, Some(incoming)).unwrap(),
            ));
        }
        if let Some(t) = &spec.limits.time {
            let incoming = crate::util::parse_duration(t).map_err(RpcErr::invalid)?;
            let existing = def
                .essence
                .time
                .as_deref()
                .map(crate::util::parse_duration)
                .transpose()
                .map_err(RpcErr::invalid)?;
            def.essence.time = Some(format!("{}ms", lower(existing, Some(incoming)).unwrap()));
        }
        let effects =
            ritual::validate_with_cfg(&def, &self.resolver(), &self.cfg, self.cfg.max_depth())
                .map_err(|errs| {
                    RpcErr::invalid(format!(
                        "ritual `{}` is not valid:\n  {}",
                        def.ritual,
                        errs.join("\n  ")
                    ))
                })?;
        let inputs = ritual::resolve_inputs(&def, &spec.args).map_err(RpcErr::invalid)?;
        if signed::is_agent(&spec.caller) {
            let changed = ritual::user_only_changes(&def, &spec.args);
            if !changed.is_empty() {
                return Err(RpcErr::new(
                    "not_authorized",
                    4,
                    format!(
                        "only the user can change {} on `{}`; a calling agent runs with the default. Leave it out, and tell the user if they want it changed.",
                        changed.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", "),
                        def.ritual
                    ),
                ));
            }
        }
        if let Some(m) = &spec.model {
            backend::split_model(m).map_err(RpcErr::invalid)?;
        }
        let model = spec.model.clone().or_else(|| def.model.clone());
        let settings = spec
            .settings
            .iter()
            .map(|(k, v)| (k.clone(), setting_value(v)))
            .collect();
        backend::validate_settings(&settings).map_err(RpcErr::invalid)?;
        let grants = self.grants_for(&def.ritual)?;
        let rendered_requires = Self::render_requires(&def.requires, &inputs)?;
        let missing: Vec<Grant> = rendered_requires
            .iter()
            .flat_map(Requirement::atoms)
            .filter(|need| !grants.iter().any(|g| g.covers(need)))
            .collect();

        let mut warnings = ritual::lint(&def);
        let mut refuse = None;
        let replaying = spec.replay.is_some();
        if replaying {
            warnings.push("drift not checked during replay".to_string());
        } else if spec.scripted {
            // A drill's answers are scripted: no backend is consulted.
        } else if let Some(base) = self.baseline(&def.ritual, version)? {
            let sites = ritual::model_sites(&def, &self.resolver(), spec.model.as_deref());
            let now = self.full_fingerprint(&sites);
            if base["fingerprint"].get("sites").is_none() {
                let msg = format!(
                    "the baseline for `{}` version {version} predates fingerprints that cover every model site; re-record it with `summonr assay {} baseline`",
                    def.ritual, def.ritual
                );
                match def.drift.on_mismatch.as_deref().unwrap_or("warn") {
                    "refuse" => refuse = Some(msg),
                    "note" => {}
                    _ => warnings.push(msg),
                }
            } else if now != base["fingerprint"] {
                let msg = format!(
                    "the backend no longer matches the baseline recorded for `{}` version {version}; run `summonr assay {}` to check for drift",
                    def.ritual, def.ritual
                );
                match def.drift.on_mismatch.as_deref().unwrap_or("warn") {
                    "refuse" => refuse = Some(msg),
                    "note" => {}
                    _ => warnings.push(msg),
                }
            }
        }
        let grants = ritual::narrow_grants(&grants, &rendered_requires);
        Ok(Resolved {
            caller: spec.caller.clone(),
            def,
            version,
            hash,
            inputs,
            model,
            settings,
            grants,
            missing,
            effects,
            warnings,
            refuse,
        })
    }

    /// The backend fingerprint of every model site, keyed by model id.
    fn full_fingerprint(&self, sites: &BTreeSet<String>) -> Value {
        json!({ "sites": backend::fingerprints_for_cfg(&self.cfg, sites) })
    }

    pub(crate) fn render_requires(
        requires: &[Requirement],
        inputs: &Value,
    ) -> Result<Vec<Requirement>, RpcErr> {
        let input_scope = crate::exec::Scope {
            inputs: inputs.clone(),
            frames: vec![],
        };
        let mut out = Vec::with_capacity(requires.len());
        for req in requires {
            let mut r = req.clone();
            let mut on_out = BTreeMap::new();
            for (k, v) in &req.on {
                let rv = ritual::render_value(&Value::String(v.clone()), &input_scope)
                    .map_err(RpcErr::invalid)?;
                let s = match rv {
                    Value::String(s) => s,
                    other => other.to_string(),
                };
                on_out.insert(k.clone(), s);
            }
            r.on = on_out;
            let mut into_out = BTreeMap::new();
            for (k, v) in &req.into {
                into_out.insert(
                    k.clone(),
                    ritual::render_value(v, &input_scope).map_err(RpcErr::invalid)?,
                );
            }
            r.into = into_out;
            out.push(r);
        }
        Ok(out)
    }

    fn check_authority(r: &Resolved) -> Result<(), RpcErr> {
        if let Some(msg) = &r.refuse {
            return Err(RpcErr::conflict(msg.clone()));
        }
        if r.missing.is_empty() {
            return Ok(());
        }
        let lines: Vec<String> = r.missing.iter().map(Grant::describe).collect();
        let commands: Vec<String> = r
            .missing
            .iter()
            .map(|g| {
                let mut c = format!(
                    "summonr allow {} --from {} --select {}",
                    r.def.ritual, g.reagent, g.action
                );
                for (k, v) in &g.on {
                    c.push_str(&format!(" --on {k}={v}"));
                }
                for (k, v) in &g.into {
                    c.push_str(&format!(" --into {k}={v}"));
                }
                c
            })
            .collect();
        Err(RpcErr::new(
            "not_authorized",
            4,
            format!(
                "ritual `{}` requires authority it has not been granted:\n  {}",
                r.def.ritual,
                lines.join("\n  ")
            ),
        )
        .with(json!({"missing": r.missing, "grant_commands": commands})))
    }

    fn make_run(
        self: &Arc<Self>,
        r: Resolved,
        model_override: Option<String>,
        series: Option<&str>,
        io: Option<Arc<dyn crate::io::Io>>,
    ) -> (String, Run, Arc<AtomicBool>) {
        let id = new_id("s");
        let cancel = Arc::new(AtomicBool::new(false));
        self.insert_summon(
            &id,
            None,
            &r.def.ritual,
            r.version,
            &r.hash,
            r.model.as_deref(),
            &r.inputs,
            &r.def.essence,
            series,
        );
        if !r.caller.is_empty() {
            // Children are found through their root, which records who asked.
            self.persisted(self.db.lock().unwrap().execute(
                "UPDATE summons SET familiar=?2 WHERE id=?1",
                params![id, r.caller],
            ));
        }
        self.active
            .lock()
            .unwrap()
            .insert(id.clone(), cancel.clone());
        self.emit(
            &id,
            "summon.start",
            &format!("{} version {} invoked", r.def.ritual, r.version),
            json!({
                "hash": r.hash,
                "inputs": r.inputs,
                "model": r.model,
                "model_override": model_override,
                "settings_override": r.settings,
                "warnings": r.warnings,
            }),
        );
        for w in &r.warnings {
            self.emit(&id, "drift", w, json!({}));
        }
        let root_ritual = r.def.ritual.clone();
        let cur_actor = audit::current_actor();
        let actor = if r.caller.starts_with("operator:") {
            Some(self.actor_context_for(&Self::normalize_actor_id(&r.caller)))
        } else if cur_actor.starts_with("operator:") || self.cfg.rbac.enforce {
            Some(self.actor_context_for(&Self::normalize_actor_id(&cur_actor)))
        } else {
            None
        };
        let run = Run {
            job_claim: None,
            core: self.clone(),
            id: id.clone(),
            root_id: id.clone(),
            fence_prefix: String::new(),
            meters: vec![Arc::new(Meter::new(&r.def.essence))],
            def: r.def,
            inputs: r.inputs,
            model_override,
            settings_override: r.settings,
            selected: Arc::new(Mutex::new(HashSet::new())),
            cancel: cancel.clone(),
            grants: Arc::new(r.grants),
            depth: 0,
            io: io.unwrap_or_else(|| Arc::new(crate::io::Live)),
            models_used: Arc::new(Mutex::new(BTreeSet::new())),
            root_ritual,
            actor,
            done: 0,
            total: 0,
        };
        (id, run, cancel)
    }

    /// Clean up a run created by `make_run` if a setup step fails before its
    /// thread starts, so neither the `running` row nor the `active` slot leaks.
    pub(crate) fn fail_unstarted(&self, id: &str, err: &RpcErr) {
        let fin = Finished {
            outcome: Outcome::Failed,
            result: Value::Null,
            error: Some(err.message.clone()),
        };
        self.finish_summon(id, &fin, json!([]));
        self.active.lock().unwrap().remove(id);
    }

    /// Recover the original `model_override` and `settings_override` from a
    /// root summon's `summon.start` event so revive and recast do not clobber
    /// child models or drop caller settings.
    fn load_start_overrides(&self, old_id: &str) -> (Option<String>, BTreeMap<String, String>) {
        let raw: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT data FROM events WHERE summon=?1 AND kind='summon.start' ORDER BY seq ASC LIMIT 1",
                params![old_id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        let Some(raw) = raw else {
            return (None, BTreeMap::new());
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            return (None, BTreeMap::new());
        };
        let model_override = v
            .get("model_override")
            .and_then(Value::as_str)
            .map(String::from);
        let settings_override = v
            .get("settings_override")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, val)| {
                        let s = match val {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        (k.clone(), s)
                    })
                    .collect()
            })
            .unwrap_or_default();
        (model_override, settings_override)
    }

    /// Run to completion and record the end, releasing models unless another
    /// summon still needs them.
    fn complete(self: &Arc<Self>, mut run: Run, hold_models: bool) -> Finished {
        let fin = run.execute();
        let others = {
            let active = self.active.lock().unwrap();
            active.keys().filter(|k| k.as_str() != run.id.as_str()).count()
        };
        let models: Vec<String> = run.models_used.lock().unwrap().iter().cloned().collect();
        let cleanup: Vec<Value> = models
            .iter()
            .map(|m| {
                let result = if run.io.offline() {
                    "not applicable (replay)".to_string()
                } else if run.io.scripted() {
                    "not applicable (scripted)".to_string()
                } else if hold_models {
                    "held for the rest of the series".to_string()
                } else if others > 0 {
                    format!("left loaded: {others} other summon(s) active")
                } else {
                    backend::unload(&self.cfg.backend_url_for(m), m)
                };
                json!({"model": m, "result": result})
            })
            .collect();
        for c in &cleanup {
            self.emit(
                &run.id,
                "release",
                &format!(
                    "{}: {}",
                    c["model"].as_str().unwrap_or(""),
                    c["result"].as_str().unwrap_or("")
                ),
                c.clone(),
            );
        }
        self.finish_summon(&run.id, &fin, Value::Array(cleanup));
        if run.depth == 0 {
            self.on_run_end(&run.id);
        }
        // Leave `active` only after the terminal record is durable so a clean
        // shutdown cannot exit mid-finalization and seal this run interrupted.
        self.active.lock().unwrap().remove(&run.id);
        fin
    }

    pub(crate) fn save_checkpoint(&self, summon: &str, path: &str, data: &Value) {
        self.save_checkpoint_with(summon, path, data, true)
    }

    /// A non-durable checkpoint covers only recomputable work; losing it
    /// means that work is redone on revive.
    pub(crate) fn save_checkpoint_with(&self, summon: &str, path: &str, data: &Value, durable: bool) {
        let db = self.db.lock().unwrap();
        self.persisted(relaxed(&db, durable, |db| {
            db.execute(
                "INSERT OR REPLACE INTO checkpoints (summon, path, data) VALUES (?1, ?2, ?3)",
                params![summon, path, data.to_string()],
            )
        }));
    }

    pub(crate) fn load_checkpoints(&self, summon: &str) -> Result<BTreeMap<String, Value>, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut q = db
            .prepare("SELECT path, data FROM checkpoints WHERE summon=?1")
            .map_err(storage_error)?;
        let rows = q
            .query_map(params![summon], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(storage_error)?;
        let mut out = BTreeMap::new();
        for row in rows {
            let (path, raw) = row.map_err(storage_error)?;
            let val: Value = serde_json::from_str(&raw).map_err(|e| {
                RpcErr::new("storage_failed", 11, format!("corrupt checkpoint `{path}`: {e}"))
            })?;
            out.insert(path, val);
        }
        Ok(out)
    }

    fn copy_checkpoints(&self, from: &str, to: &str) -> Result<(), RpcErr> {
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO checkpoints (summon, path, data) SELECT ?2, path, data FROM checkpoints WHERE summon=?1",
                params![from, to],
            )
            .map_err(storage_error)?;
        Ok(())
    }

    pub(crate) fn compute_fence_key(summon: &str, step: &str, op: &str, args_hash: &str) -> String {
        sha256_hex(format!("{summon}:{step}:{op}:{args_hash}").as_bytes())
    }

    pub(crate) fn fence_acquire(
        &self,
        summon: &str,
        step: &str,
        op: &str,
        effect: &str,
        args: &Value,
    ) -> Result<FenceAcquire, RpcErr> {
        let args_hash = crate::util::canonical_sha256(args);
        let fence_key = Self::compute_fence_key(summon, step, op, &args_hash);
        let db = self.db.lock().unwrap();
        struct FenceStateRow {
            state: String,
            output: Option<String>,
            evidence: Option<String>,
            error: Option<String>,
        }
        let existing: Option<FenceStateRow> = db
            .query_row(
                "SELECT state, output, evidence, error FROM effect_fence WHERE fence_key=?1",
                params![fence_key],
                |r| {
                    Ok(FenceStateRow {
                        state: r.get(0)?,
                        output: r.get(1)?,
                        evidence: r.get(2)?,
                        error: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(storage_error)?;
        match existing {
            None => {
                db.execute(
                    "INSERT INTO effect_fence (fence_key, summon, step, operation, effect, args_hash, state, updated) VALUES (?1,?2,?3,?4,?5,?6,'dispatched',?7)",
                    params![fence_key, summon, step, op, effect, args_hash, now_ms()],
                )
                .map_err(storage_error)?;
                Ok(FenceAcquire::Proceed { fence_key })
            }
            Some(FenceStateRow { state, output: out_s, evidence: ev_s, error: err }) => match state.as_str() {
                "applied" => {
                    let output: Value = out_s
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()
                        .map_err(|e| RpcErr::new("storage_failed", 11, format!("corrupt fenced output: {e}")))?
                        .unwrap_or(Value::Null);
                    let evidence: crate::records::Evidence = ev_s
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()
                        .map_err(|e| RpcErr::new("storage_failed", 11, format!("corrupt fenced evidence: {e}")))?
                        .ok_or_else(|| RpcErr::new("storage_failed", 11, "missing fenced evidence"))?;
                    Ok(FenceAcquire::Cached { output, evidence })
                }
                "dispatched" | "failed" if effect == "idempotent write" => {
                    db.execute(
                        "UPDATE effect_fence SET state='dispatched', error=NULL, updated=?2 WHERE fence_key=?1",
                        params![fence_key, now_ms()],
                    )
                    .map_err(storage_error)?;
                    Ok(FenceAcquire::Proceed { fence_key })
                }
                "dispatched" => Ok(FenceAcquire::Blocked {
                    reason: format!(
                        "step `{step}` ({op}) is fenced: a previous non-idempotent write was dispatched and its outcome is unknown; reconcile externally or recast"
                    ),
                }),
                "failed" => Ok(FenceAcquire::Blocked {
                    reason: format!(
                        "step `{step}` ({op}) is fenced after a failed non-idempotent write ({}); reconcile externally or recast",
                        err.unwrap_or_else(|| "unknown error".into())
                    ),
                }),
                other => Err(RpcErr::new(
                    "storage_failed",
                    11,
                    format!("unknown effect_fence state `{other}`"),
                )),
            },
        }
    }

    pub(crate) fn fence_complete(
        &self,
        fence_key: &str,
        output: &Value,
        evidence: &crate::records::Evidence,
    ) {
        let out_s = serde_json::to_string(output).unwrap_or_else(|_| "null".into());
        let ev_s = serde_json::to_string(evidence).unwrap_or_else(|_| "null".into());
        let saved = self.db.lock().unwrap().execute(
            "UPDATE effect_fence SET state='applied', output=?2, evidence=?3, error=NULL, updated=?4 WHERE fence_key=?1",
            params![fence_key, out_s, ev_s, now_ms()],
        );
        self.persisted(saved);
    }

    pub(crate) fn fence_fail(&self, fence_key: &str, err: &str) {
        let saved = self.db.lock().unwrap().execute(
            "UPDATE effect_fence SET state='failed', error=?2, updated=?3 WHERE fence_key=?1",
            params![fence_key, err, now_ms()],
        );
        self.persisted(saved);
    }

    pub(crate) fn fence_clear(&self, fence_key: &str) {
        let saved = self.db.lock().unwrap().execute(
            "DELETE FROM effect_fence WHERE fence_key=?1 AND state='dispatched'",
            params![fence_key],
        );
        self.persisted(saved);
    }

    fn copy_fences(
        &self,
        from: &str,
        to: &str,
        keep_steps: Option<&std::collections::HashSet<String>>,
    ) -> Result<(), RpcErr> {
        let db = self.db.lock().unwrap();
        let mut q = db
            .prepare("SELECT step, operation, effect, args_hash, state, output, evidence, error, updated FROM effect_fence WHERE summon=?1")
            .map_err(storage_error)?;
        let rows = q
            .query_map(params![from], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, i64>(8)?,
                ))
            })
            .map_err(storage_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage_error)?;
        drop(q);
        for (step, op, effect, args_hash, state, output, evidence, error, updated) in rows {
            if let Some(keep) = keep_steps {
                let top = step.split(['>', '[', '/']).next().unwrap_or(&step);
                if !keep.contains(top) {
                    continue;
                }
            }
            let new_key = Self::compute_fence_key(to, &step, &op, &args_hash);
            db.execute(
                "INSERT OR REPLACE INTO effect_fence (fence_key, summon, step, operation, effect, args_hash, state, output, evidence, error, updated) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![new_key, to, step, op, effect, args_hash, state, output, evidence, error, updated],
            )
            .map_err(storage_error)?;
        }
        Ok(())
    }

    fn fence_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
        Ok(json!({
            "fence_key": r.get::<_, String>(0)?,
            "summon": r.get::<_, String>(1)?,
            "step": r.get::<_, String>(2)?,
            "operation": r.get::<_, String>(3)?,
            "effect": r.get::<_, String>(4)?,
            "state": r.get::<_, String>(5)?,
            "error": r.get::<_, Option<String>>(6)?,
            "updated": r.get::<_, i64>(7)?,
        }))
    }

    fn fences_for_conn(db: &rusqlite::Connection, summon: &str) -> Result<Vec<Value>, RpcErr> {
        let mut q = db
            .prepare("SELECT fence_key, summon, step, operation, effect, state, error, updated FROM effect_fence WHERE summon=?1 ORDER BY updated, fence_key")
            .map_err(storage_error)?;
        let rows = q
            .query_map(params![summon], Self::fence_row)
            .map_err(storage_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage_error)?;
        Ok(rows)
    }

    fn fence_list(&self, p: Value) -> Result<Value, RpcErr> {
        let req: requests::FenceList = decode(p)?;
        let db = self.db.lock().unwrap();
        let rows = if let Some(summon) = req.summon {
            Self::fences_for_conn(&db, &summon)?
        } else {
            let mut q = db
                .prepare("SELECT fence_key, summon, step, operation, effect, state, error, updated FROM effect_fence ORDER BY updated DESC LIMIT 200")
                .map_err(storage_error)?;
            let mapped = q.query_map([], Self::fence_row).map_err(storage_error)?;
            mapped.collect::<Result<Vec<_>, _>>().map_err(storage_error)?
        };
        Ok(Value::Array(rows))
    }

    fn fence_resolve(&self, p: Value) -> Result<Value, RpcErr> {
        let req: requests::FenceResolve = decode(p)?;
        let db = self.db.lock().unwrap();
        let existing: Option<(String, String, String)> = db
            .query_row(
                "SELECT summon, operation, state FROM effect_fence WHERE fence_key=?1",
                params![req.fence_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(storage_error)?;
        let (summon, operation, prev_state) = existing
            .ok_or_else(|| RpcErr::not_found(format!("no effect fence `{}`", req.fence_key)))?;
        if req.action == "clear" {
            db.execute(
                "DELETE FROM effect_fence WHERE fence_key=?1",
                params![req.fence_key],
            )
            .map_err(storage_error)?;
            return Ok(json!({
                "fence_key": req.fence_key,
                "summon": summon,
                "operation": operation,
                "previous_state": prev_state,
                "state": "cleared",
            }));
        }
        let ev = crate::records::Evidence::Reported {
            observed: json!({"reconciled_by": "operator"}),
        };
        let ev_s = serde_json::to_string(&ev).unwrap();
        let out_s = json!({"reconciled": true}).to_string();
        db.execute(
            "UPDATE effect_fence SET state='applied', output=COALESCE(output, ?2), evidence=COALESCE(evidence, ?3), error=NULL, updated=?4 WHERE fence_key=?1",
            params![req.fence_key, out_s, ev_s, now_ms()],
        )
        .map_err(storage_error)?;
        Ok(json!({
            "fence_key": req.fence_key,
            "summon": summon,
            "operation": operation,
            "previous_state": prev_state,
            "state": "applied",
        }))
    }

    // ------------------------------------------------------------- methods

    /// Refuse new work at the active run limit. Callers hold `admission`.
    fn admit(&self) -> Result<(), RpcErr> {
        if self.active.lock().unwrap().len() >= self.cfg.max_active_runs.unwrap_or(16) {
            return Err(RpcErr::new(
                "busy",
                5,
                "active run limit reached; wait for existing work",
            ));
        }
        Ok(())
    }

    fn revive(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let req: requests::Summon = decode(p)?;
        let old_id = req.summon;
        let _admission = self.admission.lock().unwrap();
        self.storage_health()?;
        #[allow(clippy::type_complexity)]
        let row: Option<(
            Option<String>,
            String,
            i64,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT parent, ritual, version, hash, inputs, status, outcome, essence, familiar FROM summons WHERE id=?1",
                params![old_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_error)?;
        let (parent, ritual, version, hash, inputs_raw, status, outcome, essence_raw, familiar) =
            row.ok_or_else(|| RpcErr::not_found(format!("no summon `{old_id}`")))?;
        if parent.is_some() {
            return Err(RpcErr::invalid("only a root summon can be revived"));
        }
        if status != "ended" {
            return Err(RpcErr::conflict(format!(
                "summon `{old_id}` is still running"
            )));
        }
        if !matches!(
            outcome.as_deref(),
            Some("exhausted" | "cancelled" | "interrupted")
        ) {
            return Err(RpcErr::conflict(format!(
                "summon `{old_id}` ended as {}; only exhausted, cancelled, or interrupted runs can be revived",
                outcome.as_deref().unwrap_or("ended")
            )));
        }
        let args: Map<String, Value> = serde_json::from_str(&inputs_raw).map_err(|e| {
            RpcErr::new("storage_failed", 11, format!("corrupt inputs for `{old_id}`: {e}"))
        })?;
        let stored_limits: Essence = essence_raw
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let (orig_model_override, orig_settings) = self.load_start_overrides(&old_id);
        let spec = Spec {
            target: ritual,
            version: Some(version as u32),
            model: orig_model_override.clone(),
            settings: orig_settings,
            limits: stored_limits,
            args,
            expected_hash: Some(hash),
            caller: familiar.unwrap_or_default(),
            ..Default::default()
        };
        let r = self.resolve(&spec)?;
        Self::check_authority(&r)?;
        self.admit()?;
        let (id, run, _cancel) = self.make_run(r, orig_model_override, None, None);
        if let Err(e) = self
            .copy_checkpoints(&old_id, &id)
            .and_then(|_| self.copy_fences(&old_id, &id, None))
            .and_then(|_| self.storage_health())
        {
            self.fail_unstarted(&id, &e);
            return Err(e);
        }
        self.emit(
            &id,
            "revive",
            &format!("revived from {old_id}"),
            json!({"revived_from": old_id}),
        );
        let core = self.clone();
        let spawn_actor = audit::current_actor();
        std::thread::spawn(move || {
            let _actor = audit::ActorGuard::enter(spawn_actor);
            core.complete(run, false);
        });
        Ok(json!({"summon": id, "revived_from": old_id}))
    }

    fn recast(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let req: requests::Recast = decode(p)?;
        let old_id = req.summon;
        let _admission = self.admission.lock().unwrap();
        self.storage_health()?;
        #[allow(clippy::type_complexity)]
        let row: Option<(
            Option<String>,
            String,
            i64,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        )> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT parent, ritual, version, hash, inputs, status, essence, familiar FROM summons WHERE id=?1",
                params![old_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_error)?;
        let (parent, ritual, version, hash, inputs_raw, status, essence_raw, familiar) =
            row.ok_or_else(|| RpcErr::not_found(format!("no summon `{old_id}`")))?;
        if parent.is_some() {
            return Err(RpcErr::invalid("only a root summon can be recast"));
        }
        if status != "ended" {
            return Err(RpcErr::conflict(format!(
                "summon `{old_id}` is still running"
            )));
        }
        let args: Map<String, Value> = serde_json::from_str(&inputs_raw).map_err(|e| {
            RpcErr::new("storage_failed", 11, format!("corrupt inputs for `{old_id}`: {e}"))
        })?;
        let stored_limits: Essence = essence_raw
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let (orig_model_override, orig_settings) = self.load_start_overrides(&old_id);
        let chosen_model = req.model.or(orig_model_override);
        let spec = Spec {
            target: ritual,
            version: Some(version as u32),
            model: chosen_model.clone(),
            settings: orig_settings,
            limits: stored_limits,
            args,
            expected_hash: Some(hash),
            caller: familiar.unwrap_or_default(),
            ..Default::default()
        };
        let r = self.resolve(&spec)?;
        let step_idx = r
            .def
            .steps
            .iter()
            .position(|s| s.id.as_deref() == Some(req.from_step.as_str()))
            .ok_or_else(|| {
                RpcErr::invalid(format!(
                    "ritual `{}` has no top-level step `{}`",
                    r.def.ritual, req.from_step
                ))
            })?;
        let keep: std::collections::HashSet<String> = r.def.steps[..step_idx]
            .iter()
            .filter_map(|s| s.id.clone())
            .collect();
        let old_checkpoints = self.load_checkpoints(&old_id)?;
        Self::check_authority(&r)?;
        self.admit()?;
        let (id, run, _cancel) = self.make_run(r, chosen_model, None, None);
        if let Some(top) = old_checkpoints.get("top") {
            let mut kept_steps = Map::new();
            if let Some(steps) = top["steps"].as_object() {
                for (k, v) in steps {
                    if keep.contains(k) {
                        kept_steps.insert(k.clone(), v.clone());
                    }
                }
            }
            let kept_abstained = kept_steps
                .values()
                .any(|s| s["status"].as_str() == Some("abstained"));
            self.save_checkpoint(
                &id,
                "top",
                &json!({"steps": kept_steps, "abstained": kept_abstained}),
            );
        }
        for (path, data) in &old_checkpoints {
            if let Some(step_id) = path.strip_prefix("top#") {
                if keep.contains(step_id) {
                    self.save_checkpoint(&id, path, data);
                }
            } else if let Some(rest) = path.strip_prefix("each:") {
                // `each:ID` (older cores) or `each:ID#N`, one row per item.
                let step_id = rest.split_once('#').map_or(rest, |(id, _)| id);
                if keep.contains(step_id) {
                    self.save_checkpoint(&id, path, data);
                }
            }
        }
        if let Err(e) = self
            .copy_fences(&old_id, &id, Some(&keep))
            .and_then(|_| self.storage_health())
        {
            self.fail_unstarted(&id, &e);
            return Err(e);
        }
        self.emit(
            &id,
            "recast",
            &format!("recast from {old_id} at step {}", req.from_step),
            json!({"recast_from": old_id, "from_step": req.from_step, "kept_steps": keep.len()}),
        );
        let core = self.clone();
        let spawn_actor = audit::current_actor();
        std::thread::spawn(move || {
            let _actor = audit::ActorGuard::enter(spawn_actor);
            core.complete(run, false);
        });
        Ok(json!({"summon": id, "recast_from": old_id, "from_step": req.from_step}))
    }

    fn invoke(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let spec: Spec = decode(p)?;
        if signed::is_agent(&spec.caller) && (spec.record.is_some() || spec.replay.is_some())
        {
            return Err(RpcErr::new(
                "not_authorized",
                4,
                "arbitrary host cassette paths belong to the user; use named cassettes or simulate",
            ));
        }
        let _admission = self.admission.lock().unwrap();
        self.storage_health()?;
        let mut hashed = json!({"target":spec.target,"version":spec.version,"model":spec.model,
            "settings":spec.settings,"args":spec.args,"caller":spec.caller,"hash":spec.expected_hash,"limits":spec.limits});
        // The run mode is part of the request; plain live requests keep the
        // hash they always had.
        if spec.record.is_some()
            || spec.replay.is_some()
            || spec.simulate
            || spec.cassette_record.is_some()
            || spec.cassette_replay.is_some()
        {
            hashed["record"] = json!(spec.record);
            hashed["replay"] = json!(spec.replay);
            hashed["simulate"] = json!(spec.simulate);
            hashed["cassette_record"] = json!(spec.cassette_record);
            hashed["cassette_replay"] = json!(spec.cassette_replay);
        }
        let request_hash = sha256_hex(hashed.to_string().as_bytes());
        if let Some(key) = &spec.idempotency_key {
            if key.is_empty() || key.len() > 200 {
                return Err(RpcErr::invalid("idempotency_key must have 1..200 bytes"));
            }
            let prior: Option<(String, String)> = self
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT request_hash,summon FROM invocations WHERE caller=?1 AND key=?2",
                    params![spec.caller, key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
            if let Some((hash, id)) = prior {
                if hash != request_hash {
                    return Err(RpcErr::conflict(
                        "idempotency key already used with different arguments",
                    ));
                }
                let prior = self.summon(&id)?;
                let warnings = self.admission_warnings(&id);
                return Ok(
                    json!({"summon":id,"ritual":prior["ritual"],"version":prior["version"],"hash":prior["hash"],"warnings":warnings,"reused":true}),
                );
            }
        }
        self.admit()?;
        let r = self.resolve(&spec)?;
        if let Err(mut e) = Self::check_authority(&r) {
            if spec.petition_missing && !r.missing.is_empty() {
                let pids = self.propose_missing_authority(&r);
                e.data["petitions"] = json!(pids);
            }
            return Err(e);
        }
        self.storage_health()?;
        let rec_path = if let Some(path) = &spec.record {
            Some(PathBuf::from(path))
        } else if let Some(name) = &spec.cassette_record {
            let dir = self.cassette_dir(&spec.caller);
            std::fs::create_dir_all(&dir)
                .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
            Some(dir.join(format!("{name}.json")))
        } else {
            None
        };
        let rep_path = spec
            .replay
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| {
                spec.cassette_replay
                    .as_deref()
                    .map(|name| self.cassette_dir(&spec.caller).join(format!("{name}.json")))
            });
        let base: Option<Arc<dyn crate::io::Io>> = match (rec_path, rep_path.as_ref()) {
            (Some(_), Some(_)) => {
                return Err(RpcErr::invalid(
                    "replaying cannot be combined with recording",
                ))
            }
            (Some(path), None) => Some(Arc::new(
                crate::io::Recording::new(Arc::new(crate::io::Live), path)
                    .map_err(RpcErr::invalid)?,
            )),
            (None, Some(path)) => Some(Arc::new(
                crate::io::Replay::load(path).map_err(cassette_error)?,
            )),
            (None, None) => None,
        };
        // A replay always simulates writes: a recorded write is never looked
        // up, performed, observed or held as a petition.
        let io: Option<Arc<dyn crate::io::Io>> = if spec.simulate || rep_path.is_some() {
            Some(Arc::new(crate::io::Simulate::new(
                base.unwrap_or_else(|| Arc::new(crate::io::Live)),
            )))
        } else {
            base
        };
        let out = json!({"summon": "", "ritual": r.def.ritual, "version": r.version, "hash": r.hash, "warnings": r.warnings});
        let (id, run, _) = self.make_run(r, spec.model.clone(), None, io);
        if let Err(e) = self.storage_health() {
            self.fail_unstarted(&id, &e);
            return Err(e);
        }
        if let Some(key) = &spec.idempotency_key {
            let saved = self.db.lock().unwrap().execute(
                "INSERT INTO invocations(caller,key,request_hash,summon) VALUES (?1,?2,?3,?4)",
                params![spec.caller, key, request_hash, id],
            );
            self.persisted(saved);
            self.storage_health()?;
        }
        let core = self.clone();
        let spawn_actor = audit::current_actor();
        std::thread::spawn(move || {
            let _actor = audit::ActorGuard::enter(spawn_actor);
            core.complete(run, false);
        });
        let mut out = out;
        out["summon"] = json!(id);
        Ok(out)
    }

    fn rehearse(&self, p: Value) -> Result<Value, RpcErr> {
        let spec: Spec = decode(p)?;
        if spec.record.is_some() || spec.replay.is_some() || spec.simulate {
            return Err(RpcErr::invalid(
                "recording, replaying and simulating cannot be combined with rehearse",
            ));
        }
        let r = self.resolve(&spec)?;
        let bindings: Vec<Value> = r
            .def
            .bindings
            .iter()
            .map(|(n, b)| json!({"name": n, "source": b.document, "hash": b.hash, "chars": b.content.as_deref().map(|c| c.chars().count())}))
            .collect();
        let mut warnings = r.warnings;
        let mut pricing_info = serde_json::Map::new();
        for m in r
            .effects
            .iter()
            .filter(|e| e.kind == "model")
            .map(|e| e.what.as_str())
        {
            if !pricing_info.contains_key(m) {
                match self.cfg.price_for(m) {
                    Some(rate) => {
                        pricing_info.insert(m.to_string(), json!(rate));
                    }
                    None => {
                        pricing_info.insert(m.to_string(), json!("unpriced"));
                        if r.def.essence.cost.is_some() {
                            warnings.push(format!(
                                "model `{m}` has no `pricing:` entry in config.yaml and this ritual sets `essence.cost`; execution will refuse before dispatch"
                            ));
                        }
                    }
                }
            }
        }
        Ok(json!({
            "ritual": r.def.ritual, "version": r.version, "hash": r.hash,
            "inputs": r.inputs, "model": r.model, "settings": r.settings,
            "bindings": bindings, "essence": r.def.essence, "effects": r.effects,
            "pricing": pricing_info,
            "authority": {"granted": r.grants, "missing": r.missing},
            "warnings": warnings, "refused": r.refuse,
            "note": "No inference, tool call, or external action was performed. Model decisions and live tool results stay unresolved.",
        }))
    }

    /// A summon without its bulk (effects, fences, cleanup detail): what an
    /// agent needs to act, and small enough to always fit in one frame.
    /// Child runs are listed by id so their own records can be read.
    fn brief(&self, s: Value) -> Result<Value, RpcErr> {
        const KEEP: &[&str] = &[
            "id", "parent", "ritual", "version", "hash", "model", "status", "outcome", "started",
            "ended", "done", "total", "requests", "tokens", "inputs", "essence", "result", "error",
            "warnings", "petitions", "familiar", "cost_microusd",
        ];
        let mut out = serde_json::Map::new();
        for k in KEEP {
            if let Some(v) = s.get(*k) {
                out.insert((*k).to_string(), v.clone());
            }
        }
        let id = s["id"].as_str().unwrap_or_default();
        let children = self.summons("parent = ?1 ORDER BY started", id, 10_000, false)?;
        out.insert(
            "children".into(),
            Value::Array(
                children
                    .iter()
                    .map(|c| json!({"id": c["id"], "ritual": c["ritual"], "outcome": c["outcome"], "requests": c["requests"], "error": c["error"]}))
                    .collect(),
            ),
        );
        Ok(Value::Object(out))
    }

    fn wait(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::Wait = decode(p)?;
        let id = p.summon;
        let deadline = p
            .timeout_ms
            .map(|ms| Instant::now() + Duration::from_millis(ms));
        loop {
            let s = self.summon(&id)?;
            if s["status"] == "ended" {
                return if p.brief { self.brief(s) } else { Ok(s) };
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(RpcErr::new(
                    "timeout",
                    7,
                    format!("stopped waiting for `{id}`; the summon is still running"),
                )
                .with(json!({"summon": id})));
            }
            std::thread::sleep(Duration::from_millis(80));
        }
    }

    fn events(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::Events = decode(p)?;
        let id = p.summon.as_str();
        let after = p.after as i64;
        let limit = p.limit;
        let slim = p.slim;
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare("SELECT seq, ts, kind, text, data FROM events WHERE summon=?1 AND seq>?2 ORDER BY seq LIMIT ?3")
            .map_err(|e| RpcErr::new("error", 1, e.to_string()))?;
        let rows = st
            .query_map(params![id, after, limit], |r| {
                let mut v = json!({"seq": r.get::<_, i64>(0)?, "ts": r.get::<_, i64>(1)?, "kind": r.get::<_, String>(2)?, "text": r.get::<_, String>(3)?});
                if !slim {
                    v["data"] = parse_json(Some(r.get::<_, String>(4)?))?;
                }
                Ok(v)
            })
            .map_err(|e| RpcErr::new("error", 1, e.to_string()))?;
        Ok(Value::Array(
            rows.collect::<Result<Vec<_>, _>>().map_err(storage_error)?,
        ))
    }

    fn root_of(&self, id: &str) -> Result<String, RpcErr> {
        let mut cur = id.to_string();
        for _ in 0..16 {
            let parent: Option<String> = self
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT parent FROM summons WHERE id=?1",
                    params![cur],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
                .map_err(storage_error)?
                .flatten();
            match parent {
                Some(p) => cur = p,
                None => break,
            }
        }
        Ok(cur)
    }

    fn cancel(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::Summon = decode(p)?;
        let id = p.summon.as_str();
        let s = self.summon(id)?;
        if s["status"] == "ended" {
            return Err(RpcErr::conflict(format!(
                "summon `{id}` has already ended ({})",
                s["outcome"].as_str().unwrap_or("?")
            )));
        }
        let root = self.root_of(id)?;
        match self.active.lock().unwrap().get(&root) {
            Some(flag) => {
                flag.store(true, Ordering::SeqCst);
                self.emit(
                    &root,
                    "banish",
                    "termination requested; a request already underway may finish first",
                    json!({}),
                );
                Ok(json!({"summon": root, "requested": true}))
            }
            None => Err(RpcErr::conflict(format!(
                "summon `{id}` is not active in this core"
            ))),
        }
    }

    fn inscribe(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::Inscribe = decode(p)?;
        let path = PathBuf::from(&p.cwd).join(&p.path);
        let mut def = self.load_file(&path)?;
        if let Some(name) = &p.name {
            def.ritual = name.to_string();
        }
        let effects =
            ritual::validate_with_cfg(&def, &self.resolver(), &self.cfg, self.cfg.max_depth())
                .map_err(|errs| {
                    RpcErr::invalid(format!(
                        "{} is not valid:\n  {}",
                        path.display(),
                        errs.join("\n  ")
                    ))
                })?;
        let mut saved = self.save_ritual_version(&def, p.replace, &effects)?;
        saved["warnings"] = json!(ritual::lint(&def));
        Ok(saved)
    }

    pub(crate) fn save_ritual_version(
        &self,
        def: &Ritual,
        replace: bool,
        effects: &[Effect],
    ) -> Result<Value, RpcErr> {
        let hash = ritual::content_hash(def);
        let latest = self.load_ritual(&def.ritual, None)?;
        let version = match &latest {
            Some((v, h, _)) if *h == hash => {
                return Ok(json!({
                    "ritual": def.ritual, "version": v, "hash": hash,
                    "unchanged": true, "effects": effects,
                }));
            }
            Some((v, _, _)) if replace => v + 1,
            Some((v, _, _)) => {
                return Err(RpcErr::conflict(format!(
                    "ritual `{}` already exists at version {v} with different contents; add `replacement` (`--replace`) to save version {}",
                    def.ritual,
                    v + 1
                )))
            }
            None => 1,
        };
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO rituals (name, version, hash, def, created) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![def.ritual, version, hash, ritual::canonical_json(def), now_ms()],
            )
            .map_err(storage_error)?;
        self.record_audit(
            "ritual.inscribe",
            &def.ritual,
            "ok",
            json!({"version": version, "hash": hash}),
        );
        Ok(json!({
            "ritual": def.ritual, "version": version, "hash": hash,
            "unchanged": false, "effects": effects,
        }))
    }

    fn rituals(&self) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare("SELECT name, MAX(version), COUNT(*), MAX(created) FROM rituals GROUP BY name ORDER BY name")
            .map_err(storage_error)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .map_err(storage_error)?;
        let list: Vec<(String, u32, i64, i64)> =
            rows.collect::<Result<Vec<_>, _>>().map_err(storage_error)?;
        drop(st);
        drop(db);
        Ok(Value::Array(
            list.into_iter()
                .map(|(name, version, versions, created)| {
                    let def = self.load_ritual(&name, Some(version))?.map(|r| r.2);
                    Ok(json!({
                        "ritual": name, "version": version, "versions": versions, "saved": created,
                        "description": def.as_ref().map(|d| d.description.clone()),
                        "model": def.as_ref().and_then(|d| d.model.clone()),
                        "inputs": def.as_ref().map(|d| d.inputs.keys().cloned().collect::<Vec<_>>()),
                        "input_decls": def.as_ref().map(|d| d.inputs.clone()),
                        "output_schema": def.as_ref().and_then(|d| d.output_schema.clone()),
                        "baseline": self.baseline(&name, version)?.is_some(),
                    }))
                })
                .collect::<Result<Vec<_>,RpcErr>>()?,
        ))
    }

    fn ritual_read(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::RitualRead = decode(p)?;
        let name = p.name.as_str();
        let version = p.version;
        let (v, hash, def) = self
            .load_ritual(name, version)?
            .ok_or_else(|| RpcErr::not_found(format!("no ritual `{name}` at that version")))?;
        let mut base = self.baseline(name, v)?;
        if let Some(b) = base.as_mut() {
            b.as_object_mut().unwrap().remove("set_json");
        }
        Ok(
            json!({"ritual": name, "version": v, "hash": hash, "definition": def, "grants": self.grants_for(name)?, "baseline": base}),
        )
    }

    fn grant_atoms(&self, p: &requests::Grant) -> Result<Vec<Grant>, RpcErr> {
        let scopes = &p.scopes;
        let mut out = Vec::new();
        for s in scopes {
            let reagent = s.reagent.clone();
            reagent::validate_grant_scope(&reagent, &p.on, &p.into).map_err(RpcErr::invalid)?;
            let known = reagent::tools_of_cfg(Some(&self.cfg), &reagent);
            if known.is_empty() {
                return Err(RpcErr::not_found(format!(
                    "no reagent `{reagent}` in the satchel"
                )));
            }
            let chosen: Vec<String> = if s.all {
                known.clone()
            } else {
                s.select.clone()
            };
            if chosen.is_empty() {
                return Err(RpcErr::invalid(format!(
                    "select tools from `{reagent}` or say `all`; omission never means all"
                )));
            }
            for t in chosen {
                if !known.iter().any(|k| k == &t) {
                    return Err(RpcErr::not_found(format!(
                        "reagent `{reagent}` has no tool `{t}`"
                    )));
                }
                out.push(Grant {
                    reagent: reagent.clone(),
                    action: t,
                    on: p.on.clone(),
                    into: p.into.clone(),
                });
            }
        }
        Ok(out)
    }

    fn grant(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::Grant = decode(p)?;
        let ritual = p.ritual.as_str();
        if self.load_ritual(ritual, None)?.is_none() {
            return Err(RpcErr::not_found(format!(
                "no ritual `{ritual}` in the grimoire"
            )));
        }
        let atoms = self.grant_atoms(&p)?;
        {
            let db = self.db.lock().unwrap();
            for g in &atoms {
                self.persisted(db.execute(
                    "INSERT OR IGNORE INTO grants (ritual, grant_json, created) VALUES (?1, ?2, ?3)",
                    params![
                        ritual,
                        serde_json::to_string(g).unwrap_or_default(),
                        now_ms()
                    ],
                ));
            }
        }
        self.record_audit("grant.add", ritual, "ok", json!({"granted": atoms}));
        Ok(json!({"ritual": ritual, "granted": atoms}))
    }

    fn revoke(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::Grant = decode(p)?;
        let ritual = p.ritual.as_str();
        let atoms = self.grant_atoms(&p)?;
        let existing = self.grants_for(ritual)?;
        // An omitted scope withdraws the capability broadly.
        let hit: Vec<&Grant> = existing
            .iter()
            .filter(|g| {
                atoms.iter().any(|a| {
                    a.reagent == g.reagent
                        && a.action == g.action
                        && (a.on.is_empty() || a.on == g.on)
                        && (a.into.is_empty() || a.into == g.into)
                })
            })
            .collect();
        {
            let db = self.db.lock().unwrap();
            for g in &hit {
                self.persisted(db.execute(
                    "DELETE FROM grants WHERE ritual=?1 AND grant_json=?2",
                    params![ritual, serde_json::to_string(g).unwrap_or_default()],
                ));
            }
        }
        self.record_audit("grant.revoke", ritual, "ok", json!({"revoked": hit}));
        Ok(json!({"ritual": ritual, "revoked": hit}))
    }

    /// The concise contract a calling model needs: what to supply, what will
    /// happen, and whether authority is in place.
    fn contract(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::Contract = decode(p)?;
        let name = p.name.as_str();
        let (version, hash, def) = self
            .load_ritual(name, None)?
            .ok_or_else(|| RpcErr::not_found(format!("no ritual `{name}` in the grimoire")))?;
        let (effects, return_types) = ritual::validate_with_return_types_cfg(
            &def,
            &self.resolver(),
            &self.cfg,
            self.cfg.max_depth(),
        )
        .map_err(|errors| RpcErr::invalid(errors.join("; ")))?;
        let grants = self.grants_for(name)?;
        let missing: Vec<String> = def
            .requires
            .iter()
            .flat_map(Requirement::atoms)
            .filter(|n| !grants.iter().any(|g| g.covers(n)))
            .map(|g| g.describe())
            .collect();
        Ok(json!({
            "ritual": name, "version": version, "hash": hash, "description": def.description,
            "language_version": def.language_version, "inputs": def.inputs, "returns": def.ret.keys().collect::<Vec<_>>(), "return_types": return_types, "output_schema": def.output_schema, "essence": def.essence,
            "effects": effects.iter().map(|e| json!({"step": e.step, "what": e.what, "effect": e.effect, "determinism":e.determinism, "notes": e.notes})).collect::<Vec<_>>(),
            "model_sites": effects.iter().filter(|e| e.kind == "model").map(|e| json!({"step":e.step,"model":e.what})).collect::<Vec<_>>(),
            "execution_class": if effects.iter().any(|e| e.kind == "model") {"model_dependent"} else if effects.iter().all(|e| e.effect == "pure") {"pure_deterministic"} else {"deterministic_with_io"},
            "authority": if missing.is_empty() { json!("granted") } else { json!({"missing": missing, "note": "Only the user can grant authority; ask them."}) },
            "baseline": self.baseline(name, version)?.map(|b| json!({"accuracy": b["account"]["accuracy"], "cases": b["account"]["cases"]})),
        }))
    }

    pub(crate) fn dispatch(self: &Arc<Self>, method: &str, p: Value) -> Result<Value, RpcErr> {
        let diagnostic_method = matches!(
            method,
            "shutdown" | "chain_verify" | "audit_verify" | "audit_list"
        );
        if !diagnostic_method {
            self.storage_health()?;
        }
        let result = match method {
            "job_create" => self.job_create(p),
            "job_work" => self.job_work(p),
            "job_status" => {
                let p: requests::JobStatus = decode(p)?;
                jobs::status(&self.db.lock().unwrap(), &p.job, p.offset as usize)
            }
            "job_item" => {
                let p: requests::JobItem = decode(p)?;
                jobs::attempts(&self.db.lock().unwrap(), &p.job, &p.item)
            }
            "job_retry" => {
                let p: requests::JobItem = decode(p)?;
                jobs::retry(&self.db.lock().unwrap(), &p.job, &p.item)
            }
            "ping" => {
                let _: requests::Empty = decode(p)?;
                let ended: i64 = self
                    .db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT COUNT(*) FROM summons WHERE status='ended' AND parent IS NULL",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(storage_error)?;
                let petitions: i64 = self
                    .db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT COUNT(*) FROM petitions WHERE status='pending'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(storage_error)?;
                Ok(
                    json!({"version": env!("CARGO_PKG_VERSION"), "pid": std::process::id(), "active": self.active.lock().unwrap().len(), "ended": ended, "petitions": petitions, "socket": self.paths.socket()}),
                )
            }
            "identity" => {
                let _: requests::Empty = decode(p)?;
                let public = self.key.public();
                // `certified_by` is reserved for a future offline root key.
                Ok(json!({"key": public.text(), "id": public.id(), "certified_by": null}))
            }
            "epitaph_get" => self.epitaph_get(p),
            "epitaph_reveal" => self.epitaph_reveal(p),
            "epitaph_export" => self.epitaph_export(p),
            "epitaph_prove" => self.epitaph_prove(p),
            "chain_head" => self.chain_head(),
            "chain_verify" => self.chain_verify(p),
            "audit_list" => self.audit_list(p),
            "audit_verify" => self.audit_verify(),
            "audit_export" => self.audit_export(p),
            "audit_prune" => self.audit_prune(p),
            "witness_accept" => self.witness_accept(p),
            "witness_list" => self.witness_list(),
            "witness_drop" => self.witness_drop(p),
            "witness_to" => self.witness_to(p),
            "witness_tsa" => self.witness_tsa(p),
            "witness_compare" => self.witness_compare(p),
            "familiar_new" => self.familiar_new(p),
            "familiar_add" => self.familiar_add(p),
            "familiar_register_grant" => self.familiar_register_grant(p),
            "familiar_list" => self.familiar_list(),
            "familiar_allow" => self.familiar_allow(p, true),
            "familiar_disallow" => self.familiar_allow(p, false),
            "familiar_revoke" => self.familiar_revoke(p),
            "operator_new" => self.operator_new(p),
            "operator_add" => self.operator_add(p),
            "operator_list" => self.operator_list(),
            "operator_revoke" => self.operator_revoke(p),
            "rbac_role_create" => self.rbac_role_create(p),
            "rbac_permission_grant" => self.rbac_permission_grant(p),
            "rbac_permission_revoke" => self.rbac_permission_revoke(p),
            "rbac_actor_bind" => self.rbac_actor_bind(p),
            "rbac_actor_unbind" => self.rbac_actor_unbind(p),
            "rbac_list" => self.rbac_list(),
            "peer_add" => self.peer_add(p),
            "pact_admit" => self.pact_admit(p),
            "pact_offer" => self.pact_offer(p),
            "pact_fetch" => self.pact_fetch(p),
            "pact_agree" => self.pact_agree(p, true),
            "pact_decline" => self.pact_agree(p, false),
            "pact_list" | "pact_show" | "pact_call" | "pact_call_status" if p["caller"] == "agent" => Err(RpcErr::new(
                "not_authorized",
                4,
                "pacts are used by a familiar: run `summonr mcp as NAME` after `summonr familiar allow NAME pact P`",
            )),
            "pact_list" => self.pact_list(),
            "pact_show" => self.pact_show(p),
            "pact_poll" => self.pact_poll(),
            "pact_call" => self.pact_call_as("", p),
            "pact_call_status" => self.pact_call_status("", p),
            "pact_limit" => self.pact_limit(p),
            "pact_accept" => self.pact_decide_delivery(p, true),
            "pact_end" => self.pact_end(p),
            "pact_history" => self.pact_history(p),
            "pact_export" => self.pact_export(p),
            "pact_verify" => self.pact_verify(p),
            "pact_reject" => self.pact_decide_delivery(p, false),
            "pact_unadmit" => self.pact_unadmit(p),
            "reliquary_publish" => self.reliquary_publish(p),
            "reliquary_retract" => self.reliquary_retract(p),
            "reliquary_add_peer" => self.reliquary_add_peer(p),
            "reliquary_remove_peer" => self.reliquary_remove_peer(p),
            "reliquary_list" => self.reliquary_list(p),
            "reliquary_show" => self.reliquary_show(p),
            "reliquary_inscribe" => self.reliquary_inscribe(p),
            "reliquary_export" => self.reliquary_export(p),
            "reliquary_import" => self.reliquary_import(p),
            "rite_rehearse" => self.rite_rehearse(p),
            "rite_start" => self.rite_start(p),
            "rite_advance" => self.rite_advance(p),
            "rite_list" => self.rite_list(),
            "rite_show" => self.rite_show(p),
            "rite_events" => self.rite_events(p),
            "rite_falsify" => self.rite_falsify(p),
            "rite_assert" => self.rite_assert(p),
            "peer_list" => self.peer_list(),
            "peer_remove" => self.peer_remove(p),
            "invoke" => self.invoke(p),
            "rehearse" => self.rehearse(p),
            "wait" => self.wait(p),
            "get" => {
                let p: requests::Summon = decode(p)?;
                let s = self.summon(&p.summon)?;
                if p.brief {
                    self.brief(s)
                } else {
                    Ok(s)
                }
            }
            "events" => self.events(p),
            "cancel" => self.cancel(p),
            "list" => {
                let p: requests::List = decode(p)?;
                let limit = i64::from(p.limit);
                Ok(Value::Array(match p.scope {
                    requests::ListScope::Ended => self.summons(
                        "status = 'ended' AND parent IS NULL AND ?1 = '' ORDER BY ended DESC",
                        "",
                        limit,
                        false,
                    ),
                    requests::ListScope::Children => self.summons(
                        "parent = ?1 ORDER BY started",
                        p.summon.as_deref().expect("validated child scope"),
                        limit,
                        false,
                    ),
                    requests::ListScope::Active => self.summons(
                        "status != 'ended' AND ?1 = '' ORDER BY started",
                        "",
                        limit,
                        false,
                    ),
                }?))
            }

            "inscribe" => self.inscribe(p),
            "rituals" => {
                let _: requests::Empty = decode(p)?;
                self.rituals()
            }
            "ritual_read" => self.ritual_read(p),
            "grant" => self.grant(p),
            "revoke" => self.revoke(p),
            "assay" => self.assay(p),
            "assay_compare" => self.assay_compare(p),
            "compile" => self.compile(p),
            "drill" => self.drill(p),
            "drill_inline" => self.drill_inline(p),
            "revive" => self.revive(p),
            "recast" => self.recast(p),
            "session_open" => self.session_open(p),
            "session_list" => self.session_list(p),
            "session_get" => self.session_get(p),
            "session_close" => self.session_close(p),
            "session_soul" => self.session_soul(p),
            "session_bind" => self.session_bind(p),
            "session_imbue" => self.session_imbue(p),
            "session_bid" => self.session_bid(p),
            "session_lethe" => self.session_lethe(p),
            "session_inscribe" => self.session_inscribe(p),
            "experiment_sweep" => self.experiment_sweep(p),
            "experiment_judge" => self.experiment_judge(p),
            "experiment_weigh" => self.experiment_weigh(p),
            "fence_list" => self.fence_list(p),
            "fence_resolve" => self.fence_resolve(p),
            "petitions" => Ok(json!(self.petitions(&decode(p)?)?)),
            "petition_decide" => self.petition_decide(p),
            "petition_request" => self.petition_request(p),
            "glean" => self.glean(p),
            "contract" => self.contract(p),
            "tools" => {
                let _: requests::Empty = decode(p)?;
                Ok(Value::Array(
                reagent::all_tools().map(|t| json!({"reagent": t.reagent, "tool": t.name, "effect": t.effect, "determinism": t.determinism, "description": t.description, "provenance": "built-in implementation"})).collect(),
            ))
            }
            "shutdown" => {
                let p: requests::Shutdown = decode(p)?;
                let active = self.active.lock().unwrap().len();
                if active > 0 && !p.force {
                    return Err(RpcErr::conflict(format!(
                        "{active} summon(s) are still active; banish them first"
                    )));
                }
                self.stop.store(true, Ordering::SeqCst);
                Ok(json!({"stopping": true}))
            }
            other => Err(RpcErr::invalid(format!("unknown method `{other}`"))),
        };
        if !diagnostic_method {
            self.storage_health()?;
        }
        result
    }

    /// Stream stored backlog and live emitted events to a subscribed socket client.
    fn serve_subscribe(&self, stream: &mut std::os::unix::net::UnixStream, id: Value, params: &Value) {
        if let Err(e) = self.storage_health() {
            let _ = write_frame(
                stream,
                &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": e.message, "data": {"code": e.code, "exit": e.exit, "detail": e.data}}}),
            );
            return;
        }
        let summon = params["summon"].as_str().map(str::to_string);
        let after = params["after"].as_i64().unwrap_or(0);
        let slim = params["slim"].as_bool().unwrap_or(false);
        let (tx, rx) = std::sync::mpsc::channel();
        self.event_subs.lock().unwrap().push(EventSub {
            summon: summon.clone(),
            slim,
            tx,
        });
        if write_frame(
            stream,
            &json!({"jsonrpc": "2.0", "id": id, "result": {"subscribed": true, "after": after}}),
        )
        .is_err()
        {
            return;
        }
        let mut last_by_summon: HashMap<String, i64> = HashMap::new();
        let backlog: Vec<Value> = {
            let db = self.db.lock().unwrap();
            let sql = if summon.is_some() {
                "SELECT summon, seq, ts, kind, text, data FROM events WHERE summon=?1 AND seq>?2 ORDER BY seq LIMIT 5000"
            } else {
                "SELECT summon, seq, ts, kind, text, data FROM events WHERE (?1 = '' OR summon=?1) AND seq>?2 ORDER BY ts, seq LIMIT 5000"
            };
            let filter = summon.as_deref().unwrap_or("");
            db.prepare(sql)
                .and_then(|mut st| {
                    let rows = st.query_map(params![filter, after], |r| {
                        let mut v = json!({
                            "summon": r.get::<_, String>(0)?,
                            "seq": r.get::<_, i64>(1)?,
                            "ts": r.get::<_, i64>(2)?,
                            "kind": r.get::<_, String>(3)?,
                            "text": r.get::<_, String>(4)?,
                        });
                        if !slim {
                            v["data"] = parse_json(Some(r.get::<_, String>(5)?))?;
                        }
                        Ok(v)
                    })?;
                    Ok(rows.filter_map(Result::ok).collect())
                })
                .unwrap_or_default()
        };
        for ev in backlog {
            if let (Some(sid), Some(seq)) = (ev["summon"].as_str(), ev["seq"].as_i64()) {
                last_by_summon.insert(sid.to_string(), seq);
            }
            if write_frame(stream, &json!({"jsonrpc": "2.0", "method": "event", "params": ev})).is_err() {
                return;
            }
        }
        while !self.stop.load(Ordering::SeqCst) {
            match rx.recv_timeout(Duration::from_millis(150)) {
                Ok(ev) => {
                    if let (Some(sid), Some(seq)) = (ev["summon"].as_str(), ev["seq"].as_i64()) {
                        if last_by_summon.get(sid).is_some_and(|&prev| seq <= prev) {
                            continue;
                        }
                        last_by_summon.insert(sid.to_string(), seq);
                    }
                    if write_frame(stream, &json!({"jsonrpc": "2.0", "method": "event", "params": ev})).is_err() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    /// Serve until asked to stop.
    pub fn serve(self: Arc<Self>) -> Result<(), String> {
        let sock = self.paths.socket();
        if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            return Err(format!("a core is already serving {}", sock.display()));
        }
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock)
            .map_err(|e| format!("cannot bind {}: {e}", sock.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
        }
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        // The socket is the only way in. If it is removed or replaced (the
        // home was deleted, the runtime dir cleared, another core bound it),
        // nothing can reach this core again, so it ends.
        let socket_id = |p: &std::path::Path| {
            use std::os::unix::fs::MetadataExt;
            std::fs::symlink_metadata(p).ok().map(|m| (m.dev(), m.ino()))
        };
        let bound = socket_id(&sock);
        let mut checked = std::time::Instant::now();
        let poller = self.clone();
        std::thread::spawn(move || poller.pact_poller());
        eprintln!(
            "summonr core {} serving {}",
            env!("CARGO_PKG_VERSION"),
            sock.display()
        );
        while !self.stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let core = self.clone();
                    stream.set_nonblocking(false).ok();
                    std::thread::spawn(move || {
                        let _actor = audit::ActorGuard::enter(audit::peer_cred_actor(&stream));
                        while let Ok(Some(req)) = read_frame(&mut stream) {
                            let id = req["id"].clone();
                            if req.get("signed").is_some() {
                                let sealed = core.handle_signed(&req);
                                let reply = json!({"jsonrpc": "2.0", "id": id, "signed": sealed["signed"]});
                                if write_frame(&mut stream, &reply).is_err() {
                                    break;
                                }
                                continue;
                            }
                            let method = req["method"].as_str().unwrap_or("").to_string();
                            if core.cfg.rbac.enforce
                                && !matches!(
                                    method.as_str(),
                                    "ping" | "identity" | "chain_verify" | "audit_verify"
                                )
                            {
                                core.record_audit(
                                    "rbac.unsigned_refused",
                                    &method,
                                    "denied",
                                    json!({"reason": "rbac.enforce requires Ed25519-signed operator requests"}),
                                );
                                let e = RpcErr::new(
                                    "not_authorized",
                                    4,
                                    format!("RBAC enforcement is active; `{method}` requires a signed operator request"),
                                );
                                let reply = json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": e.message, "data": {"code": e.code, "exit": e.exit, "detail": e.data}}});
                                if write_frame(&mut stream, &reply).is_err() {
                                    break;
                                }
                                continue;
                            }
                            if method == "subscribe" {
                                core.serve_subscribe(&mut stream, id, &req["params"]);
                                break;
                            }
                            let reply = match core.dispatch(&method, req["params"].clone()) {
                                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                                Err(e) => {
                                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": e.message, "data": {"code": e.code, "exit": e.exit, "detail": e.data}}})
                                }
                            };
                            if write_frame(&mut stream, &reply).is_err() {
                                break;
                            }
                        }
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                Err(e) => eprintln!("accept failed: {e}"),
            }
            if checked.elapsed() >= Duration::from_secs(1) {
                checked = std::time::Instant::now();
                if socket_id(&sock) != bound {
                    eprintln!("summonr core: {} was removed or replaced; stopping", sock.display());
                    self.stop.store(true, Ordering::SeqCst);
                    return Ok(());
                }
            }
        }
        let _ = std::fs::remove_file(&sock);
        Ok(())
    }
}

fn job_effects(effects: &[Effect]) -> Result<(), RpcErr> {
    if effects
        .iter()
        .any(|e| !matches!(e.effect.as_str(), "pure" | "read" | "model" | "ritual"))
    {
        Err(RpcErr::invalid("shared job workers may compute results and read; external writes and held writes require a separate authorized workflow"))
    } else {
        Ok(())
    }
}

/// Load `config/core.key`, creating it (and `config/core.pub`) on first start.
fn load_or_create_core_key(paths: &Paths) -> Result<crate::keys::Keypair, String> {
    let path = paths.config.join("core.key");
    if !path.exists() {
        crate::keys::save_new(&path, &crate::keys::generate())?;
    }
    let key = crate::keys::load(&path)?;
    let public = paths.config.join("core.pub");
    let text = format!("{}\n", key.public().text());
    if std::fs::read_to_string(&public).ok().as_deref() != Some(text.as_str()) {
        std::fs::write(&public, text).map_err(|e| format!("cannot write {}: {e}", public.display()))?;
    }
    Ok(key)
}

/// Earlier builds called familiars "principals", then "patrons": carry their
/// tables, run attribution and key folders forward. Idempotent.
fn migrate_principals(db: &rusqlite::Connection, paths: &Paths) -> Result<(), String> {
    let err = |e: rusqlite::Error| e.to_string();
    for old in ["principals", "patrons"] {
        let has_table: bool = db
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1", params![old], |r| r.get::<_, i64>(0))
            .map_err(err)?
            > 0;
        if has_table {
            db.execute_batch(&format!(
                "INSERT OR IGNORE INTO familiars (name, kind, key, key_id, created, revoked, allowed)
                   SELECT name, kind, key, key_id, created, revoked, allowed FROM {old};
                 DROP TABLE {old};"
            ))
            .map_err(err)?;
        }
    }
    let columns: Vec<String> = {
        let mut q = db.prepare("PRAGMA table_info(summons)").map_err(err)?;
        let names = q.query_map([], |r| r.get::<_, String>(1)).map_err(err)?;
        names.collect::<Result<Vec<_>, _>>().map_err(err)?
    };
    for old in ["principal", "patron"] {
        if columns.iter().any(|c| c == old) {
            db.execute_batch(&format!(
                "UPDATE summons SET familiar = REPLACE({old}, '{old}:', 'familiar:')
                   WHERE familiar IS NULL AND {old} IS NOT NULL;
                 ALTER TABLE summons DROP COLUMN {old};"
            ))
            .map_err(err)?;
        }
    }
    let new = paths.config.join("familiars");
    for old in ["principals", "patrons"] {
        let old = paths.config.join(old);
        if old.is_dir() && !new.exists() {
            std::fs::rename(&old, &new).map_err(|e| format!("cannot move {} to {}: {e}", old.display(), new.display()))?;
        }
    }
    Ok(())
}

/// Add a column to an existing table unless it is already there.
fn add_column(db: &rusqlite::Connection, table: &str, column: &str, decl: &str) -> Result<(), String> {
    let present = {
        let mut q = db
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|e| e.to_string())?;
        let names = q
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        names.iter().any(|n| n == column)
    };
    if !present {
        db.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Writes this thread committed without their own sync (tests only).
    pub(crate) static RELAXED_WRITES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Run one write with `synchronous=NORMAL` when it need not be durable on its
/// own (the connection is otherwise FULL). Callers hold the database lock.
fn relaxed<T>(
    db: &rusqlite::Connection,
    durable: bool,
    write: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    if durable {
        return write(db);
    }
    #[cfg(test)]
    RELAXED_WRITES.with(|n| n.set(n.get() + 1));
    db.execute_batch("PRAGMA synchronous=NORMAL")?;
    let out = write(db);
    db.execute_batch("PRAGMA synchronous=FULL")?;
    out
}

#[cfg(test)]
mod robustness_tests;
#[cfg(test)]
mod safe_build_tests;
#[cfg(test)]
mod test_home;
#[cfg(test)]
mod perf_bench;
#[cfg(test)]
mod assay_tests;
#[cfg(test)]
mod petition_tests;
#[cfg(test)]
mod familiar_tests;
#[cfg(test)]
mod signed_tests;
#[cfg(test)]
mod epitaph_tests;
#[cfg(test)]
mod sight_tests;
#[cfg(test)]
mod witness_tests;
#[cfg(test)]
mod pact_tests;
#[cfg(test)]
pub(crate) mod repo_tests;
