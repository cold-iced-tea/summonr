//! Local, read-only index of completed assays. Execution always goes through the core.
use super::client;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Series {
    pub id: String,
    pub ritual: String,
    pub date: i64,
    pub count: i64,
    pub passed: i64,
}
#[derive(Clone)]
pub struct Shelf {
    pub db: PathBuf,
    pub series: Vec<Series>,
    pub recent: Vec<(String, String)>,
}

fn read(path: &Path) -> Result<Connection, String> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())
}
pub fn shelf(socket: &Path) -> Result<Shelf, String> {
    let status = client::request(socket, "ping", serde_json::json!({}))?;
    let dir = if let Some(home) = status["home"].as_str() {
        PathBuf::from(home).join("state")
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .ok_or("Cannot locate core state")?
            .join("summonr")
    };
    let path = dir.join("summonr.db");
    let conn = read(&path)?;
    let query = || -> rusqlite::Result<_> {
        let mut q=conn.prepare("SELECT series,ritual,created,json_array_length(trials),(SELECT count(*) FROM json_each(trials) WHERE json_extract(value,'$.correct')=1) FROM assay_trials ORDER BY created DESC LIMIT 100")?;
        let series = q
            .query_map([], |r| {
                Ok(Series {
                    id: r.get(0)?,
                    ritual: r.get(1)?,
                    date: r.get(2)?,
                    count: r.get(3)?,
                    passed: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut q=conn.prepare("SELECT id,ritual FROM summons WHERE parent IS NULL AND ended IS NOT NULL ORDER BY ended DESC LIMIT 35")?;
        let recent = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Shelf {
            db: path,
            series,
            recent,
        })
    };
    query().map_err(|e| e.to_string())
}
pub fn cases(db: &Path, series: &str) -> Result<Vec<Value>, String> {
    let raw: String = read(db)?
        .query_row(
            "SELECT trials FROM assay_trials WHERE series=?1",
            [series],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}
