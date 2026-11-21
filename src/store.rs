//! SQLite ownership and checked read boundaries. Transactions stay explicit.
use crate::{
    ritual::{self, Grant},
    rpc::RpcErr,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::sync::{LockResult, Mutex, MutexGuard};

pub struct Store {
    connection: Mutex<Connection>,
}
pub fn error(e: rusqlite::Error) -> RpcErr {
    RpcErr::new("storage_failed", 11, e.to_string())
}
pub fn json_value(raw: Option<String>) -> rusqlite::Result<Value> {
    match raw {
        None => Ok(Value::Null),
        Some(s) => serde_json::from_str(&s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        }),
    }
}
fn decode<T: DeserializeOwned>(raw: &str) -> Result<T, RpcErr> {
    serde_json::from_str(raw)
        .map_err(|e| RpcErr::new("storage_failed", 11, format!("invalid stored record: {e}")))
}
impl Store {
    pub fn new(connection: Connection) -> Self {
        Self {
            connection: Mutex::new(connection),
        }
    }
    /// Explicit transactions and migrations may borrow the connection; ordinary reads use named methods.
    pub(crate) fn lock(&self) -> LockResult<MutexGuard<'_, Connection>> {
        self.connection.lock()
    }
    pub fn ritual(
        &self,
        name: &str,
        version: Option<u32>,
    ) -> Result<Option<ritual::Resolved>, RpcErr> {
        let db = self.lock().unwrap();
        let row: Option<(u32, String, String)> = match version {
            Some(v) => db.query_row(
                "SELECT version,hash,def FROM rituals WHERE name=?1 AND version=?2",
                params![name, v],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ),
            None => db.query_row(
                "SELECT version,hash,def FROM rituals WHERE name=?1 ORDER BY version DESC LIMIT 1",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ),
        }
        .optional()
        .map_err(error)?;
        row.map(|(v, h, raw)| Ok((v, h, decode(&raw)?))).transpose()
    }
    pub fn grants(&self, ritual: &str) -> Result<Vec<Grant>, RpcErr> {
        let db = self.lock().unwrap();
        let mut q = db
            .prepare("SELECT grant_json FROM grants WHERE ritual=?1 ORDER BY created")
            .map_err(error)?;
        let rows = q
            .query_map([ritual], |r| r.get::<_, String>(0))
            .map_err(error)?;
        rows.map(|row| decode(&row.map_err(error)?)).collect()
    }
    pub fn baseline(&self, ritual: &str, version: u32) -> Result<Option<Value>, RpcErr> {
        self.lock().unwrap().query_row("SELECT model,set_name,set_hash,set_json,fingerprint,account,created,mode,touchstone FROM baselines WHERE ritual=?1 AND version=?2",params![ritual,version],|r|Ok(json!({
            "model":r.get::<_,Option<String>>(0)?,"set":r.get::<_,String>(1)?,"set_hash":r.get::<_,String>(2)?,"set_json":r.get::<_,String>(3)?,
            "fingerprint":json_value(Some(r.get(4)?))?,"account":json_value(Some(r.get(5)?))?,"created":r.get::<_,i64>(6)?,"mode":r.get::<_,String>(7)?,
            "touchstone":json_value(r.get::<_,Option<String>>(8)?)?,
        }))).optional().map_err(error)
    }
    pub fn summons(
        &self,
        filter: &str,
        arg: &str,
        limit: i64,
        full: bool,
    ) -> Result<Vec<Value>, RpcErr> {
        let db = self.lock().unwrap();
        // The filter is an internal constant, never socket input.
        let sql=format!("SELECT id,parent,series,ritual,version,hash,model,status,outcome,started,ended,done,total,requests,tokens,inputs,essence,result,error,cleanup,familiar,cost_microusd FROM summons WHERE {filter} LIMIT ?2");
        let mut q = db.prepare(&sql).map_err(error)?;
        let rows=q.query_map(params![arg,limit],|r|{
            let outcome:Option<String>=r.get(8)?;
            if let Some(ref outcome)=outcome {
                outcome.parse::<crate::outcome::Outcome>().map_err(|e|rusqlite::Error::FromSqlConversionFailure(8,rusqlite::types::Type::Text,Box::new(std::io::Error::other(e))))?;
            }
            let mut v=json!({"id":r.get::<_,String>(0)?,"parent":r.get::<_,Option<String>>(1)?,"series":r.get::<_,Option<String>>(2)?,"ritual":r.get::<_,String>(3)?,"version":r.get::<_,u32>(4)?,"hash":r.get::<_,String>(5)?,"model":r.get::<_,Option<String>>(6)?,"status":r.get::<_,String>(7)?,"outcome":outcome,"started":r.get::<_,i64>(9)?,"ended":r.get::<_,Option<i64>>(10)?,"done":r.get::<_,i64>(11)?,"total":r.get::<_,i64>(12)?,"requests":r.get::<_,u64>(13)?,"tokens":r.get::<_,u64>(14)?,"cost_microusd":r.get::<_,Option<u64>>(21)?,"essence":json_value(r.get(16)?)?,"error":r.get::<_,Option<String>>(18)?,"familiar":r.get::<_,Option<String>>(20)?});
            if full{v["inputs"]=json_value(r.get(15)?)?;v["result"]=json_value(r.get(17)?)?;v["cleanup"]=json_value(r.get(19)?)?;}
            Ok(v)
        }).map_err(error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(error)
    }
}
