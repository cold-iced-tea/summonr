//! Strict socket request DTOs. Arbitrary ritual inputs remain task-owned JSON.
use crate::{ritual::Essence, rpc::RpcErr};
use serde::{de::DeserializeOwned, Deserialize, Deserializer};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub trait Validate {
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}
pub fn decode<T: DeserializeOwned + Validate>(value: Value) -> Result<T, RpcErr> {
    let request: T = serde_json::from_value(value).map_err(|e| RpcErr::invalid(e.to_string()))?;
    request.validate().map_err(RpcErr::invalid)?;
    Ok(request)
}
fn nonempty<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    if s.trim().is_empty() {
        Err(serde::de::Error::custom("must be a nonempty string"))
    } else {
        Ok(s)
    }
}
fn cwd() -> String {
    ".".into()
}
fn list_limit() -> u32 {
    100
}
fn events_limit() -> u32 {
    500
}
fn sample_count() -> u32 {
    10
}
fn one() -> u32 {
    1
}
fn lease() -> u64 {
    300000
}
fn range(value: u64, max: u64, label: &str) -> Result<(), String> {
    if value == 0 || value > max {
        Err(format!("{label} must be 1..{max}"))
    } else {
        Ok(())
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Invoke {
    #[serde(deserialize_with = "nonempty")]
    pub target: String,
    pub version: Option<u32>,
    pub model: Option<String>,
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    #[serde(default)]
    pub args: Map<String, Value>,
    #[serde(default = "cwd")]
    #[allow(dead_code)]
    pub cwd: String,
    #[serde(default)]
    pub caller: String,
    pub expected_hash: Option<String>,
    /// Absolute path of a cassette to record this run into.
    pub record: Option<String>,
    /// Absolute path of a cassette to replay this run from.
    pub replay: Option<String>,
    /// Answer writes with what they would do instead of doing them.
    #[serde(default)]
    pub simulate: bool,
    /// File pending `authority.grant` petitions when standing grants are missing.
    #[serde(default)]
    pub petition_missing: bool,
    /// Named cassette in `<state>/cassettes/<name>.json` for safe MCP recording.
    pub cassette_record: Option<String>,
    /// Named cassette in `<state>/cassettes/<name>.json` for safe MCP replay.
    pub cassette_replay: Option<String>,
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub limits: Essence,
    /// Set only inside the core for drills: model answers are scripted, so
    /// no backend is consulted for drift. Never read from a request.
    #[serde(skip)]
    pub scripted: bool,
}
impl Validate for Invoke {
    fn validate(&self) -> Result<(), String> {
        if self.version == Some(0) {
            return Err("version must be positive".into());
        }
        if self.model.as_ref().is_some_and(|s| s.trim().is_empty()) {
            return Err("model must not be empty".into());
        }
        if let Some(r) = &self.record {
            if !std::path::Path::new(r).is_absolute() {
                return Err("recording path must be absolute".into());
            }
        }
        if let Some(r) = &self.replay {
            if !std::path::Path::new(r).is_absolute() {
                return Err("replay path must be absolute".into());
            }
            if self.record.is_some() {
                return Err("replaying cannot be combined with recording".into());
            }
        }
        let valid_cassette_name = |n: &str| {
            !n.is_empty()
                && n.len() <= 128
                && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        };
        if let Some(n) = &self.cassette_record {
            if !valid_cassette_name(n) {
                return Err("cassette_record must be alphanumeric, `-`, or `_`".into());
            }
            if self.record.is_some() || self.replay.is_some() || self.cassette_replay.is_some() {
                return Err("cannot combine cassette_record with another record/replay option".into());
            }
        }
        if let Some(n) = &self.cassette_replay {
            if !valid_cassette_name(n) {
                return Err("cassette_replay must be alphanumeric, `-`, or `_`".into());
            }
            if self.record.is_some() || self.replay.is_some() || self.cassette_record.is_some() {
                return Err("cannot combine cassette_replay with another record/replay option".into());
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PetitionRequest {
    #[serde(deserialize_with = "nonempty")]
    pub ritual: String,
    #[serde(default)]
    pub args: Map<String, Value>,
    #[serde(default = "cwd")]
    #[allow(dead_code)]
    pub cwd: String,
}
impl Validate for PetitionRequest {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}
impl Validate for Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compile {
    pub definition: Value,
}
impl Validate for Compile {
    fn validate(&self) -> Result<(), String> {
        if !(self.definition.is_object() || self.definition.is_string()) {
            return Err("definition must be a ritual object or YAML/JSON string".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrillInline {
    #[serde(deserialize_with = "nonempty")]
    pub ritual: String,
    pub version: Option<u32>,
    pub cases: Value,
}
impl Validate for DrillInline {
    fn validate(&self) -> Result<(), String> {
        if self.version == Some(0) {
            return Err("version must be positive".into());
        }
        if !(self.cases.is_object() || self.cases.is_string()) {
            return Err("cases must be a drill object or YAML/JSON string".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionOpen {
    pub name: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    #[serde(default)]
    pub essence: Essence,
}
impl Validate for SessionOpen {
    fn validate(&self) -> Result<(), String> {
        if self.model.as_ref().is_some_and(|s| s.trim().is_empty()) {
            return Err("model must not be empty".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionId {
    #[serde(deserialize_with = "nonempty")]
    pub session: String,
}
impl Validate for SessionId {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSoul {
    #[serde(deserialize_with = "nonempty")]
    pub session: String,
    pub text: String,
}
impl Validate for SessionSoul {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionBind {
    #[serde(deserialize_with = "nonempty")]
    pub session: String,
    #[serde(deserialize_with = "nonempty")]
    pub name: String,
    pub document: Option<String>,
    pub text: String,
}
impl Validate for SessionBind {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionImbue {
    #[serde(deserialize_with = "nonempty")]
    pub session: String,
    pub model: Option<String>,
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    pub essence: Option<Essence>,
}
impl Validate for SessionImbue {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionBid {
    #[serde(deserialize_with = "nonempty")]
    pub session: String,
    #[serde(deserialize_with = "nonempty")]
    pub prompt: String,
    pub schema: Option<Value>,
}
impl Validate for SessionBid {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionLethe {
    #[serde(deserialize_with = "nonempty")]
    pub session: String,
    pub keep: Option<usize>,
}
impl Validate for SessionLethe {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInscribe {
    #[serde(deserialize_with = "nonempty")]
    pub session: String,
    #[serde(deserialize_with = "nonempty")]
    pub ritual: String,
    #[serde(default)]
    pub replace: bool,
}
impl Validate for SessionInscribe {}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SweepCase {
    pub id: Option<String>,
    #[serde(default)]
    pub inputs: Map<String, Value>,
    pub expect: Option<Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sweep {
    #[serde(deserialize_with = "nonempty")]
    pub ritual: String,
    pub version: Option<u32>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub settings: Vec<BTreeMap<String, String>>,
    #[serde(default)]
    pub cases: Vec<SweepCase>,
    #[serde(default)]
    pub live: bool,
}
impl Validate for Sweep {
    fn validate(&self) -> Result<(), String> {
        if self.version == Some(0) {
            return Err("version must be positive".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Judge {
    #[serde(deserialize_with = "nonempty")]
    pub experiment: String,
    pub touchstone: Option<String>,
}
impl Validate for Judge {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Weigh {
    #[serde(deserialize_with = "nonempty")]
    pub experiment: String,
    pub against: Option<String>,
}
impl Validate for Weigh {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FenceList {
    pub summon: Option<String>,
}
impl Validate for FenceList {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FenceResolve {
    #[serde(deserialize_with = "nonempty")]
    pub fence_key: String,
    #[serde(deserialize_with = "nonempty")]
    pub action: String,
}
impl Validate for FenceResolve {
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.action.as_str(), "applied" | "clear") {
            return Err("action must be `applied` or `clear`".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summon {
    #[serde(deserialize_with = "nonempty")]
    pub summon: String,
    /// Only the outcome, result, usage and child runs; no effects or fences.
    #[serde(default)]
    pub brief: bool,
}
impl Validate for Summon {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recast {
    #[serde(deserialize_with = "nonempty")]
    pub summon: String,
    #[serde(deserialize_with = "nonempty")]
    pub from_step: String,
    pub model: Option<String>,
}
impl Validate for Recast {
    fn validate(&self) -> Result<(), String> {
        if self.model.as_ref().is_some_and(|s| s.trim().is_empty()) {
            return Err("model must not be empty".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wait {
    #[serde(deserialize_with = "nonempty")]
    pub summon: String,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub brief: bool,
    /// Cancel the summon if this client goes away before the wait ends.
    /// Read by the connection loop, which watches the client (`watch_for_drop`).
    #[serde(default)]
    #[allow(dead_code)]
    pub cancel_on_drop: bool,
}
impl Validate for Wait {
    fn validate(&self) -> Result<(), String> {
        if self.timeout_ms.is_some_and(|t| t > 86400000) {
            Err("timeout_ms must be at most one day".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Events {
    #[serde(deserialize_with = "nonempty")]
    pub summon: String,
    #[serde(default)]
    pub after: u64,
    #[serde(default = "events_limit")]
    pub limit: u32,
    #[serde(default)]
    pub slim: bool,
}
impl Validate for Events {
    fn validate(&self) -> Result<(), String> {
        range(self.limit.into(), 10000, "limit")?;
        if self.after > i64::MAX as u64 {
            Err("after exceeds sequence range".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ListScope {
    #[default]
    Active,
    Ended,
    Children,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    #[serde(default)]
    pub scope: ListScope,
    pub summon: Option<String>,
    #[serde(default = "list_limit")]
    pub limit: u32,
}
impl Validate for List {
    fn validate(&self) -> Result<(), String> {
        range(self.limit.into(), 10000, "limit")?;
        match (&self.scope, &self.summon) {
            (ListScope::Children, Some(id)) if !id.trim().is_empty() => Ok(()),
            (ListScope::Children, _) => Err("children scope requires summon".into()),
            (_, Some(_)) => Err("summon only applies to children scope".into()),
            _ => Ok(()),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantCheck {
    /// One ritual and its children; absent: every ritual.
    #[serde(default)]
    pub ritual: Option<String>,
}
impl Validate for GrantCheck {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inscribe {
    #[serde(deserialize_with = "nonempty")]
    pub path: String,
    #[serde(default = "cwd")]
    #[allow(dead_code)]
    pub cwd: String,
    pub name: Option<String>,
    #[serde(default)]
    pub replace: bool,
}
impl Validate for Inscribe {
    fn validate(&self) -> Result<(), String> {
        if self.name.as_ref().is_some_and(|n| n.trim().is_empty()) {
            Err("name must not be empty".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RitualRead {
    #[serde(deserialize_with = "nonempty")]
    pub name: String,
    pub version: Option<u32>,
}
impl Validate for RitualRead {
    fn validate(&self) -> Result<(), String> {
        if self.version == Some(0) {
            Err("version must be positive".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    #[serde(deserialize_with = "nonempty")]
    pub name: String,
}
impl Validate for Contract {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantScope {
    #[serde(deserialize_with = "nonempty")]
    pub reagent: String,
    pub version: Option<u32>,
    #[serde(default)]
    pub select: Vec<String>,
    #[serde(default)]
    pub all: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    #[serde(deserialize_with = "nonempty")]
    pub ritual: String,
    pub scopes: Vec<GrantScope>,
    #[serde(default)]
    pub on: BTreeMap<String, String>,
    #[serde(default)]
    pub into: BTreeMap<String, String>,
}
impl Validate for Grant {
    fn validate(&self) -> Result<(), String> {
        if self.scopes.is_empty() {
            return Err("name at least one reagent scope".into());
        }
        for scope in &self.scopes {
            if scope.version.is_some_and(|v| v != 1) {
                return Err("built-in reagents only support version 1".into());
            }
            if scope.all == !scope.select.is_empty() {
                return Err("choose either all or a nonempty tool selection".into());
            }
            if scope.select.iter().any(|s| s.trim().is_empty()) {
                return Err("tool names must not be empty".into());
            }
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assay {
    #[serde(deserialize_with = "nonempty")]
    pub ritual: String,
    pub version: Option<u32>,
    #[serde(default)]
    pub baseline: bool,
    pub model: Option<String>,
    pub against: Option<String>,
    #[serde(default = "cwd")]
    #[allow(dead_code)]
    pub cwd: String,
    /// Perform writes for real. Without it every case is simulated.
    #[serde(default)]
    pub live: bool,
}
impl Validate for Assay {
    fn validate(&self) -> Result<(), String> {
        if self.version == Some(0) {
            Err("version must be positive".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Drill {
    #[serde(deserialize_with = "nonempty")]
    pub ritual: String,
    pub version: Option<u32>,
    #[serde(deserialize_with = "nonempty")]
    pub against: String,
    #[serde(default = "cwd")]
    #[allow(dead_code)]
    pub cwd: String,
}
impl Validate for Drill {
    fn validate(&self) -> Result<(), String> {
        if self.version == Some(0) {
            Err("version must be positive".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PetitionFilter {
    #[default]
    Pending,
    Executing,
    Unknown,
    Applied,
    Denied,
    Expired,
    Unverified,
    Failed,
    All,
}
impl PetitionFilter {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Executing => "executing",
            Self::Unknown => "unknown",
            Self::Applied => "applied",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Unverified => "unverified",
            Self::Failed => "failed",
            Self::All => "all",
        }
    }
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Petitions {
    #[serde(default)]
    pub status: PetitionFilter,
    pub summon: Option<String>,
}
impl Validate for Petitions {
    fn validate(&self) -> Result<(), String> {
        if self.summon.as_ref().is_some_and(|s| s.trim().is_empty()) {
            return Err("summon must not be empty".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decide {
    pub permit: bool,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub ids: Vec<String>,
    pub ritual: Option<String>,
}
impl Validate for Decide {
    fn validate(&self) -> Result<(), String> {
        if self.ritual.as_ref().is_some_and(|s| s.trim().is_empty()) {
            return Err("ritual must not be empty".into());
        }
        if self.all == !self.ids.is_empty() {
            return Err("choose either all or nonempty petition IDs".into());
        }
        if !self.all && self.ritual.is_some() {
            return Err("ritual filtering requires all".into());
        }
        if self.ids.iter().any(|s| s.trim().is_empty()) {
            return Err("petition IDs must not be empty".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Glean {
    #[serde(deserialize_with = "nonempty")]
    pub ritual: String,
    #[serde(default = "sample_count")]
    pub count: u32,
    pub against: Option<String>,
    #[serde(default = "cwd")]
    #[allow(dead_code)]
    pub cwd: String,
}
impl Validate for Glean {
    fn validate(&self) -> Result<(), String> {
        range(self.count.into(), 10000, "count")
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shutdown {
    #[serde(default)]
    pub force: bool,
}
impl Validate for Shutdown {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobWork {
    #[serde(deserialize_with = "nonempty")]
    pub job: String,
    #[serde(default = "one")]
    pub count: u32,
    #[serde(default = "lease")]
    pub lease_ms: u64,
}
impl Validate for JobWork {
    fn validate(&self) -> Result<(), String> {
        range(self.count.into(), 100, "count")?;
        if !(1000..=3600000).contains(&self.lease_ms) {
            Err("lease_ms must be 1000..3600000".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobStatus {
    #[serde(deserialize_with = "nonempty")]
    pub job: String,
    #[serde(default)]
    pub offset: u32,
}
impl Validate for JobStatus {
    fn validate(&self) -> Result<(), String> {
        if self.offset > 10000 {
            Err("offset exceeds job size limit".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobItem {
    #[serde(deserialize_with = "nonempty")]
    pub job: String,
    #[serde(deserialize_with = "nonempty")]
    pub item: String,
}
impl Validate for JobItem {}
