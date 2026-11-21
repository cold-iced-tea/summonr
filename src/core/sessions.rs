//! Interactive manual summon sessions: `summon`, `soul`, `bind`, `imbue`,
//! `bid`, `lethe`, and `inscribe` from a live session.
use super::*;
use crate::ritual::BindingDecl;
use std::time::Duration;

fn parse_setting_val(s: &str) -> Value {
    if s == "true" {
        json!(true)
    } else if s == "false" {
        json!(false)
    } else if let Ok(n) = s.parse::<i64>() {
        json!(n)
    } else if let Ok(f) = s.parse::<f64>() {
        if f.is_finite() {
            json!(f)
        } else {
            json!(s)
        }
    } else {
        json!(s)
    }
}

#[derive(Clone)]
struct SessionRow {
    id: String,
    name: Option<String>,
    model: Option<String>,
    system_prompt: String,
    settings: BTreeMap<String, Value>,
    essence: Essence,
    bindings: BTreeMap<String, BindingDecl>,
    turns: Vec<Value>,
    used_requests: u64,
    used_tokens: u64,
    used_cost_microusd: u64,
    status: String,
    created: i64,
    ended: Option<i64>,
}

impl Core {
    fn load_session(&self, id: &str) -> Result<SessionRow, RpcErr> {
        let db = self.db.lock().unwrap();
        let row = db
            .query_row(
                "SELECT id, name, model, system_prompt, settings, essence, bindings, turns, used_requests, used_tokens, status, created, ended, used_cost_microusd FROM sessions WHERE id=?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, i64>(8)?,
                        r.get::<_, i64>(9)?,
                        r.get::<_, String>(10)?,
                        r.get::<_, i64>(11)?,
                        r.get::<_, Option<i64>>(12)?,
                        r.get::<_, i64>(13)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| RpcErr::not_found(format!("no live summon session `{id}`")))?;
        Ok(SessionRow {
            id: row.0,
            name: row.1,
            model: row.2,
            system_prompt: row.3,
            settings: serde_json::from_str(&row.4).map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?,
            essence: serde_json::from_str(&row.5).map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?,
            bindings: serde_json::from_str(&row.6).map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?,
            turns: serde_json::from_str(&row.7).map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?,
            used_requests: row.8.max(0) as u64,
            used_tokens: row.9.max(0) as u64,
            used_cost_microusd: row.13.max(0) as u64,
            status: row.10,
            created: row.11,
            ended: row.12,
        })
    }

    fn require_open(s: &SessionRow) -> Result<(), RpcErr> {
        if s.status != "open" {
            return Err(RpcErr::conflict(format!(
                "session `{}` is already closed",
                s.id
            )));
        }
        Ok(())
    }

    fn session_json(s: &SessionRow) -> Value {
        json!({
            "session": s.id,
            "name": s.name,
            "model": s.model,
            "system_prompt": s.system_prompt,
            "settings": s.settings,
            "essence": s.essence,
            "bindings": s.bindings.keys().collect::<Vec<_>>(),
            "turn_count": s.turns.len(),
            "turns": s.turns,
            "used_requests": s.used_requests,
            "used_tokens": s.used_tokens,
            "used_cost_microusd": s.used_cost_microusd,
            "status": s.status,
            "created": s.created,
            "ended": s.ended,
        })
    }

    pub(super) fn session_open(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::SessionOpen = decode(p)?;
        let id = new_id("m");
        let model = p.model;
        let mut settings = BTreeMap::new();
        for (k, v) in p.settings {
            settings.insert(k, parse_setting_val(&v));
        }
        let now = now_ms();
        let settings_s = serde_json::to_string(&settings).unwrap();
        let essence_s = serde_json::to_string(&p.essence).unwrap();
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO sessions (id, name, model, system_prompt, settings, essence, bindings, turns, used_requests, used_tokens, status, created) VALUES (?1,?2,?3,'',?4,?5,'{}','[]',0,0,'open',?6)",
                params![id, p.name, model, settings_s, essence_s, now],
            )
            .map_err(storage_error)?;
        Ok(json!({
            "session": id,
            "name": p.name,
            "model": model,
            "essence": p.essence,
            "status": "open",
        }))
    }

    pub(super) fn session_list(&self, p: Value) -> Result<Value, RpcErr> {
        let _: requests::Empty = decode(p)?;
        let db = self.db.lock().unwrap();
        let mut q = db
            .prepare("SELECT id, name, model, status, used_requests, used_tokens, created, ended FROM sessions ORDER BY created DESC LIMIT 100")
            .map_err(storage_error)?;
        let rows = q
            .query_map([], |r| {
                Ok(json!({
                    "session": r.get::<_, String>(0)?,
                    "name": r.get::<_, Option<String>>(1)?,
                    "model": r.get::<_, Option<String>>(2)?,
                    "status": r.get::<_, String>(3)?,
                    "used_requests": r.get::<_, i64>(4)?,
                    "used_tokens": r.get::<_, i64>(5)?,
                    "created": r.get::<_, i64>(6)?,
                    "ended": r.get::<_, Option<i64>>(7)?,
                }))
            })
            .map_err(storage_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage_error)?;
        Ok(Value::Array(rows))
    }

    pub(super) fn session_get(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::SessionId = decode(p)?;
        let s = self.load_session(&p.session)?;
        Ok(Self::session_json(&s))
    }

    pub(super) fn session_close(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::SessionId = decode(p)?;
        let s = self.load_session(&p.session)?;
        Self::require_open(&s)?;
        let ended = now_ms();
        self.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE sessions SET status='closed', ended=?2 WHERE id=?1",
                params![s.id, ended],
            )
            .map_err(storage_error)?;
        Ok(json!({"session": s.id, "status": "closed", "ended": ended}))
    }

    pub(super) fn session_soul(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::SessionSoul = decode(p)?;
        let s = self.load_session(&p.session)?;
        Self::require_open(&s)?;
        self.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE sessions SET system_prompt=?2 WHERE id=?1",
                params![s.id, p.text],
            )
            .map_err(storage_error)?;
        Ok(json!({"session": s.id, "system_prompt": p.text}))
    }

    pub(super) fn session_bind(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::SessionBind = decode(p)?;
        let mut s = self.load_session(&p.session)?;
        Self::require_open(&s)?;
        let hash = sha256_hex(p.text.as_bytes());
        let doc = p.document.unwrap_or_else(|| format!("inline:{}", p.name));
        s.bindings.insert(
            p.name.clone(),
            BindingDecl {
                document: Some(doc.clone()),
                hash: Some(hash.clone()),
                content: Some(p.text),
            },
        );
        let raw = serde_json::to_string(&s.bindings).unwrap();
        self.db
            .lock()
            .unwrap()
            .execute("UPDATE sessions SET bindings=?2 WHERE id=?1", params![s.id, raw])
            .map_err(storage_error)?;
        Ok(json!({"session": s.id, "binding": p.name, "document": doc, "hash": hash}))
    }

    pub(super) fn session_imbue(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::SessionImbue = decode(p)?;
        let mut s = self.load_session(&p.session)?;
        Self::require_open(&s)?;
        if let Some(m) = p.model {
            if m.trim().is_empty() {
                return Err(RpcErr::invalid("model must not be empty"));
            }
            s.model = Some(m);
        }
        for (k, v) in p.settings {
            if matches!(k.as_str(), "requests" | "tokens" | "time" | "tool_calls" | "items") {
                return Err(RpcErr::invalid(format!(
                    "`{k}` is an essence limit, not a model setting; pass it under `given {k}=...` (`--arg {k}=...`)"
                )));
            }
            s.settings.insert(k, parse_setting_val(&v));
        }
        if let Some(e) = p.essence {
            if let Some(r) = e.requests {
                s.essence.requests = Some(r);
            }
            if let Some(t) = e.tokens {
                s.essence.tokens = Some(t);
            }
            if let Some(tm) = e.time {
                s.essence.time = Some(tm);
            }
            if let Some(tc) = e.tool_calls {
                s.essence.tool_calls = Some(tc);
            }
            if let Some(it) = e.items {
                s.essence.items = Some(it);
            }
        }
        let settings_s = serde_json::to_string(&s.settings).unwrap();
        let essence_s = serde_json::to_string(&s.essence).unwrap();
        self.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE sessions SET model=?2, settings=?3, essence=?4 WHERE id=?1",
                params![s.id, s.model, settings_s, essence_s],
            )
            .map_err(storage_error)?;
        Ok(json!({"session": s.id, "model": s.model, "settings": s.settings, "essence": s.essence}))
    }

    pub(super) fn session_bid(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::SessionBid = decode(p)?;
        // Serialize concurrent bids on the same session so request/token
        // limits cannot be raced and turns cannot overwrite each other.
        let lock = {
            static LOCKS: std::sync::OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
                std::sync::OnceLock::new();
            let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
            map.lock()
                .unwrap()
                .entry(p.session.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _session_guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = self.load_session(&p.session)?;
        Self::require_open(&s)?;
        let model = s.model.clone().ok_or_else(|| {
            RpcErr::invalid("session has no model; pass `--model` on `summon` or configure `default_model`")
        })?;
        if let Some(max_req) = s.essence.requests {
            if s.used_requests >= max_req {
                return Err(RpcErr::new(
                    "exhausted",
                    10,
                    format!("session request essence exhausted ({}/{})", s.used_requests, max_req),
                ));
            }
        }
        if let Some(max_tok) = s.essence.tokens {
            if s.used_tokens >= max_tok {
                return Err(RpcErr::new(
                    "exhausted",
                    10,
                    format!("session token essence exhausted ({}/{})", s.used_tokens, max_tok),
                ));
            }
        }
        let max_cost_microusd = s
            .essence
            .cost
            .as_deref()
            .map(crate::util::parse_cost_microusd)
            .transpose()
            .map_err(RpcErr::invalid)?;
        let pricing = self.cfg.price_for(&model);
        if let Some(max_cost) = max_cost_microusd {
            if pricing.is_none() {
                return Err(RpcErr::new(
                    "failed",
                    11,
                    format!("model `{model}` has no entry in config.yaml `pricing:` and this session has a cost limit"),
                ));
            }
            if s.used_cost_microusd >= max_cost {
                return Err(RpcErr::new(
                    "exhausted",
                    10,
                    format!(
                        "session cost essence exhausted ({}/{})",
                        crate::util::fmt_cost_microusd(s.used_cost_microusd),
                        crate::util::fmt_cost_microusd(max_cost)
                    ),
                ));
            }
        }
        let timeout = if let Some(ref t) = s.essence.time {
            let ms = crate::util::parse_duration(t).map_err(RpcErr::invalid)?;
            let elapsed = (now_ms() - s.created).max(0) as u64;
            if elapsed >= ms {
                return Err(RpcErr::new("exhausted", 10, "session time essence exhausted"));
            }
            Some(Duration::from_millis(ms - elapsed))
        } else {
            None
        };
        let schema = match p.schema {
            Some(sc) => {
                let expanded = ritual::expand_schema(sc).map_err(RpcErr::invalid)?;
                let inlined = crate::schema::inline_refs(&expanded).map_err(RpcErr::invalid)?;
                Some(inlined)
            }
            None => None,
        };
        let mut system_parts = Vec::new();
        if !s.system_prompt.trim().is_empty() {
            system_parts.push(s.system_prompt.clone());
        }
        for (name, b) in &s.bindings {
            if let Some(content) = &b.content {
                system_parts.push(format!("[binding {name}]\n{content}"));
            }
        }
        let system = system_parts.join("\n\n");
        let mut user_parts = Vec::new();
        for t in &s.turns {
            if let (Some(u), Some(a)) = (t["prompt"].as_str(), t["content"].as_str()) {
                user_parts.push(format!("User: {u}\nAssistant: {a}"));
            }
        }
        user_parts.push(p.prompt.clone());
        let user = if s.turns.is_empty() {
            p.prompt.clone()
        } else {
            user_parts.join("\n\n")
        };
        let remaining_tokens = s.essence.tokens.map(|m| m.saturating_sub(s.used_tokens));

        // Failed backend attempts still consume request essence.
        s.used_requests += 1;
        self.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE sessions SET used_requests=?2 WHERE id=?1",
                params![s.id, s.used_requests as i64],
            )
            .map_err(storage_error)?;

        let base_url = self.cfg.backend_url_for(&model);
        let resp = backend::chat(
            &base_url,
            &model,
            backend::ChatReq {
                model: &model,
                system: &system,
                user: &user,
                schema: schema.as_ref(),
                settings: &s.settings,
                timeout,
                max_output_tokens: remaining_tokens,
            },
        )
        .map_err(|e| RpcErr::new("backend_failed", 11, e))?;

        // Charge tokens & cost before validating the output so a schema failure
        // never becomes a free retry, and reject post-response overruns.
        let turn_tokens = resp.prompt_tokens.saturating_add(resp.output_tokens);
        let turn_cost = match &pricing {
            Some(rate) => rate
                .cost_microusd(resp.prompt_tokens, resp.output_tokens)
                .map_err(|e| RpcErr::new("failed", 11, e))?,
            None => resp.provider_cost_microusd.unwrap_or(0),
        };
        s.used_tokens = s.used_tokens.saturating_add(turn_tokens);
        s.used_cost_microusd = s.used_cost_microusd.saturating_add(turn_cost);
        self.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE sessions SET used_tokens=?2, used_cost_microusd=?3 WHERE id=?1",
                params![s.id, s.used_tokens as i64, s.used_cost_microusd as i64],
            )
            .map_err(storage_error)?;
        if let Some(max_tok) = s.essence.tokens {
            if s.used_tokens > max_tok {
                return Err(RpcErr::new(
                    "exhausted",
                    10,
                    format!(
                        "session token essence exhausted ({}/{})",
                        s.used_tokens, max_tok
                    ),
                ));
            }
        }
        if let Some(max_cost) = max_cost_microusd {
            if resp.token_confidence == "unknown" {
                return Err(RpcErr::new(
                    "failed",
                    11,
                    "backend did not report token usage; cannot check session cost essence",
                ));
            }
            if s.used_cost_microusd > max_cost {
                return Err(RpcErr::new(
                    "exhausted",
                    10,
                    format!(
                        "session cost essence exceeded ({}/{})",
                        crate::util::fmt_cost_microusd(s.used_cost_microusd),
                        crate::util::fmt_cost_microusd(max_cost)
                    ),
                ));
            }
        }

        let parsed: Value = if schema.is_some() {
            let v: Value = serde_json::from_str(&resp.content)
                .map_err(|e| RpcErr::invalid(format!("model output was not valid JSON: {e}")))?;
            if let Some(ref sc) = schema {
                crate::schema::check(sc, &v, "output").map_err(RpcErr::invalid)?;
            }
            v
        } else {
            Value::String(resp.content.clone())
        };
        let turn_idx = s.turns.len() + 1;
        s.turns.push(json!({
            "turn": turn_idx,
            "prompt": p.prompt,
            "schema": schema,
            "content": resp.content,
            "output": parsed,
            "constrained": resp.constrained,
            "prompt_tokens": resp.prompt_tokens,
            "output_tokens": resp.output_tokens,
        }));
        let turns_s = serde_json::to_string(&s.turns).unwrap();
        self.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE sessions SET turns=?2, used_tokens=?3 WHERE id=?1",
                params![s.id, turns_s, s.used_tokens as i64],
            )
            .map_err(storage_error)?;

        Ok(json!({
            "session": s.id,
            "turn": turn_idx,
            "output": parsed,
            "constrained": resp.constrained,
            "usage": {
                "prompt_tokens": resp.prompt_tokens,
                "output_tokens": resp.output_tokens,
                "total_tokens": turn_tokens,
            },
            "used_requests": s.used_requests,
            "used_tokens": s.used_tokens,
            "used_cost_microusd": s.used_cost_microusd,
        }))
    }

    pub(super) fn session_lethe(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::SessionLethe = decode(p)?;
        let mut s = self.load_session(&p.session)?;
        Self::require_open(&s)?;
        let total = s.turns.len();
        let keep = p.keep.unwrap_or(0).min(total);
        let forgotten = total - keep;
        if keep == 0 {
            s.turns.clear();
        } else {
            s.turns = s.turns.split_off(total - keep);
        }
        let turns_s = serde_json::to_string(&s.turns).unwrap();
        self.db
            .lock()
            .unwrap()
            .execute("UPDATE sessions SET turns=?2 WHERE id=?1", params![s.id, turns_s])
            .map_err(storage_error)?;
        Ok(json!({
            "session": s.id,
            "forgotten": forgotten,
            "remaining_turns": s.turns.len(),
        }))
    }

    pub(super) fn session_inscribe(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::SessionInscribe = decode(p)?;
        let s = self.load_session(&p.session)?;
        if s.turns.is_empty() {
            return Err(RpcErr::invalid(format!(
                "session `{}` has no turns; run `bid` before inscribing",
                s.id
            )));
        }
        let mut steps = Vec::with_capacity(s.turns.len());
        let mut last_id = String::new();
        for (idx, turn) in s.turns.iter().enumerate() {
            let sid = format!("turn_{}", idx + 1);
            let prompt = turn["prompt"].as_str().unwrap_or("").to_string();
            let schema = turn.get("schema").filter(|v| v.is_object()).cloned();
            let mut model_obj = json!({"prompt": prompt});
            if !s.bindings.is_empty() && idx == 0 {
                let binds: Vec<&String> = s.bindings.keys().collect();
                model_obj["bind"] = json!(binds);
            }
            if !s.system_prompt.is_empty() {
                model_obj["instructions"] = json!(s.system_prompt);
            }
            if let Some(sc) = schema {
                model_obj["schema"] = sc;
            }
            steps.push(json!({"id": sid, "model": model_obj}));
            last_id = sid;
        }
        let mut def: Ritual = serde_json::from_value(json!({
            "language_version": 1,
            "ritual": p.ritual,
            "description": format!("Inscribed from live summon session {}", s.id),
            "bindings": s.bindings,
            "model": s.model,
            "settings": s.settings,
            "essence": s.essence,
            "steps": steps,
            "return": {"output": format!("${{steps.{last_id}.output}}")},
        }))
        .map_err(|e| RpcErr::invalid(e.to_string()))?;
        ritual::prepare(&mut def, Path::new("."), &self.resolver()).map_err(RpcErr::invalid)?;
        let effects =
            ritual::validate_with_cfg(&def, &self.resolver(), &self.cfg, self.cfg.max_depth())
                .map_err(|errs| RpcErr::invalid(errs.join("\n  ")))?;
        let mut out = self.save_ritual_version(&def, p.replace, &effects)?;
        out["session"] = json!(s.id);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_home::Home;

    #[test]
    fn manual_summon_lifecycle_and_inscribe() {
        let h = Home::new("");
        let c = &h.core;

        let opened = c
            .dispatch(
                "session_open",
                json!({"name":"triage","model":"mock/keyword","essence":{"requests":2}}),
            )
            .unwrap();
        let sid = opened["session"].as_str().unwrap().to_string();
        assert_eq!(opened["status"], "open");

        c.dispatch(
            "session_soul",
            json!({"session": sid, "text": "You classify messages."}),
        )
        .unwrap();
        let bound = c
            .dispatch(
                "session_bind",
                json!({"session": sid, "name": "policy", "text": "Receipts go to Receipts."}),
            )
            .unwrap();
        assert_eq!(bound["binding"], "policy");
        c.dispatch(
            "session_imbue",
            json!({"session": sid, "settings": {"temperature": "0.1"}}),
        )
        .unwrap();

        let t1 = c
            .dispatch(
                "session_bid",
                json!({
                    "session": sid,
                    "prompt": "Here is your receipt for coffee.",
                    "schema": {"type": "object", "required": ["folder"], "properties": {"folder": "string"}}
                }),
            )
            .unwrap();
        assert_eq!(t1["turn"], 1);
        assert_eq!(t1["used_requests"], 1);
        assert!(t1["output"]["folder"].is_string(), "{t1}");

        let t2 = c
            .dispatch(
                "session_bid",
                json!({"session": sid, "prompt": "Follow-up note"}),
            )
            .unwrap();
        assert_eq!(t2["turn"], 2);
        assert_eq!(t2["used_requests"], 2);

        // Request essence exhausted on 3rd bid
        let err = c
            .dispatch("session_bid", json!({"session": sid, "prompt": "Too many"}))
            .unwrap_err();
        assert_eq!(err.code, "exhausted");

        // Lethe keeps last 1 turn
        let lethe = c
            .dispatch("session_lethe", json!({"session": sid, "keep": 1}))
            .unwrap();
        assert_eq!(lethe["forgotten"], 1);
        assert_eq!(lethe["remaining_turns"], 1);

        // Inscribe live session as a saved ritual and run it
        let ins = c
            .dispatch(
                "session_inscribe",
                json!({"session": sid, "ritual": "from-live"}),
            )
            .unwrap();
        assert_eq!(ins["ritual"], "from-live");
        assert_eq!(ins["version"], 1);

        let run_id = c
            .dispatch("invoke", json!({"target": "from-live"}))
            .unwrap()["summon"]
            .as_str()
            .unwrap()
            .to_string();
        let done = c
            .dispatch("wait", json!({"summon": run_id, "timeout_ms": 5000}))
            .unwrap();
        assert_eq!(done["outcome"], "returned", "{done}");

        c.dispatch("session_close", json!({"session": sid})).unwrap();
        let closed_err = c
            .dispatch("session_lethe", json!({"session": sid}))
            .unwrap_err();
        assert_eq!(closed_err.code, "state_conflict");
    }
}
