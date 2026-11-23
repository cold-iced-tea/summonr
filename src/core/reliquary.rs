//! Reliquary service: publish, retract, browse, export, import, and inscribe
//! signed ritual packs across cores or offline files. Grants never travel.
use super::*;
use crate::keys::PublicKey;
use crate::reliquary::{
    self, collect_pinned_children, PackEvidence, PackManifest, PackRitual, SignedPack,
    PACK_FORMAT_V1,
};

fn sql(e: rusqlite::Error) -> RpcErr {
    RpcErr::new("storage_failed", 11, e.to_string())
}

fn parse<T: serde::de::DeserializeOwned>(p: Value) -> Result<T, RpcErr> {
    serde_json::from_value(p).map_err(|e| RpcErr::invalid(e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishArgs {
    ritual: String,
    #[serde(default)]
    version: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetractArgs {
    name: String,
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerNameArgs {
    peer: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    peer: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackRefArgs {
    #[serde(default)]
    peer: Option<String>,
    name: String,
    #[serde(default)]
    version: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportArgs {
    name: String,
    #[serde(default)]
    version: Option<u32>,
    file: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportArgs {
    file: String,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    peer: Option<String>,
    #[serde(default)]
    replace: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InscribePackArgs {
    #[serde(default)]
    peer: Option<String>,
    name: String,
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    replace: bool,
}

impl Core {
    /// Look up a pinned peer's public key by its key ID (for signed peer reliquary reads).
    pub(super) fn peer_by_key(&self, key_id: &str) -> Option<(String, PublicKey)> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare("SELECT name, key FROM peers").ok()?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .ok()?;
        for row in rows.flatten() {
            if let Ok(pk) = PublicKey::parse(&row.1) {
                if pk.id() == key_id {
                    return Some((row.0, pk));
                }
            }
        }
        None
    }

    /// Look up a pinned peer's `(url, PublicKey)` by peer name.
    fn peer_endpoint(&self, peer: &str) -> Result<(String, PublicKey), RpcErr> {
        let db = self.db.lock().unwrap();
        let row: Option<(String, String)> = db
            .query_row(
                "SELECT url, key FROM peers WHERE name=?1",
                params![peer],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        let Some((url, key_text)) = row else {
            return Err(RpcErr::not_found(format!(
                "no pinned peer `{peer}`; add it with `summonr peer add {peer} url URL key K`"
            )));
        };
        let key = PublicKey::parse(&key_text)
            .map_err(|e| RpcErr::new("storage_failed", 11, format!("peer `{peer}` key: {e}")))?;
        Ok((url, key))
    }

    /// Send a signed RPC to a pinned peer core.
    fn reliquary_send(&self, peer: &str, method: &str, params: Value) -> Result<Value, RpcErr> {
        self.pact_send(peer, method, params)
    }

    /// Recursively collect the topological closure of pinned child rituals for `def`.
    fn collect_closure(
        &self,
        def: &Ritual,
        visited: &mut BTreeSet<(String, u32)>,
        out: &mut Vec<PackRitual>,
        evidence: &mut Vec<PackEvidence>,
    ) -> Result<(), RpcErr> {
        let mut refs = Vec::new();
        collect_pinned_children(&def.steps, &mut refs);
        collect_pinned_children(&def.finally, &mut refs);
        for (cname, cver, chash) in refs {
            let Some((v, h, cdef)) = self.load_ritual(&cname, cver)? else {
                return Err(RpcErr::not_found(format!(
                    "cannot package `{}`: child ritual `{cname}` is missing from the grimoire",
                    def.ritual
                )));
            };
            if let Some(expected) = chash {
                if h != expected {
                    return Err(RpcErr::conflict(format!(
                        "child ritual `{cname}` v{v} hash {h} does not match pinned {expected}"
                    )));
                }
            }
            if visited.insert((cname.clone(), v)) {
                self.collect_closure(&cdef, visited, out, evidence)?;
                if let Some(ev) = self.pack_evidence_for(&cname, v)? {
                    evidence.push(ev);
                }
                out.push(PackRitual {
                    name: cname,
                    version: v,
                    hash: h,
                    definition: cdef,
                });
            }
        }
        Ok(())
    }

    /// Build author-claimed `PackEvidence` from a local baseline, if one exists.
    fn pack_evidence_for(&self, ritual: &str, version: u32) -> Result<Option<PackEvidence>, RpcErr> {
        let Some(b) = self.baseline(ritual, version)? else {
            return Ok(None);
        };
        Ok(Some(PackEvidence {
            ritual: ritual.into(),
            version,
            model: b["model"].as_str().map(str::to_string),
            set_name: b["set"].as_str().unwrap_or("").into(),
            set_hash: b["set_hash"].as_str().unwrap_or("").into(),
            fingerprint: b["fingerprint"].clone(),
            account: b["account"].clone(),
            created: b["created"].as_i64().unwrap_or(0),
            mode: b["mode"].as_str().unwrap_or("simulated").into(),
            touchstone: b.get("touchstone").filter(|v| !v.is_null()).cloned(),
            author_claimed: true,
        }))
    }

    /// Load a locally published pack by name and optional version.
    fn load_local_pack(&self, name: &str, version: Option<u32>) -> Result<SignedPack, RpcErr> {
        let db = self.db.lock().unwrap();
        let raw: Option<String> = match version {
            Some(v) => db
                .query_row(
                    "SELECT pack_json FROM reliquary_packs WHERE name=?1 AND version=?2",
                    params![name, v],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql)?,
            None => db
                .query_row(
                    "SELECT pack_json FROM reliquary_packs WHERE name=?1 ORDER BY version DESC LIMIT 1",
                    params![name],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql)?,
        };
        let Some(text) = raw else {
            return Err(RpcErr::not_found(format!(
                "no published pack `{name}` in this core's reliquary"
            )));
        };
        serde_json::from_str(&text)
            .map_err(|e| RpcErr::new("storage_failed", 11, format!("stored pack is damaged: {e}")))
    }

    pub(super) fn reliquary_publish(&self, p: Value) -> Result<Value, RpcErr> {
        let p: PublishArgs = parse(p)?;
        let (v, h, def) = self.load_ritual(&p.ritual, p.version)?.ok_or_else(|| {
            RpcErr::not_found(format!("no ritual `{}` in the grimoire", p.ritual))
        })?;
        // Ensure ritual validates cleanly before publishing.
        ritual::validate_with_cfg(&def, &self.resolver(), &self.cfg, self.cfg.max_depth())
            .map_err(|e| RpcErr::invalid(e.join("; ")))?;

        let mut visited = BTreeSet::new();
        visited.insert((p.ritual.clone(), v));
        let mut children = Vec::new();
        let mut evidence = Vec::new();
        self.collect_closure(&def, &mut visited, &mut children, &mut evidence)?;
        if let Some(ev) = self.pack_evidence_for(&p.ritual, v)? {
            evidence.push(ev);
        }

        let created = now_ms();
        let manifest = PackManifest {
            format: PACK_FORMAT_V1,
            name: p.ritual.clone(),
            version: v,
            description: def.description.clone(),
            author_key: self.key.public().text(),
            created,
            root: PackRitual {
                name: p.ritual.clone(),
                version: v,
                hash: h,
                definition: def,
            },
            children,
            evidence,
            retracted: None,
        };
        let signed = reliquary::sign_pack(&self.key, manifest);
        let pack_json = serde_json::to_string(&signed).unwrap_or_default();
        self.persisted(self.db.lock().unwrap().execute(
            "INSERT OR REPLACE INTO reliquary_packs (name, version, hash, author_key, pack_json, created, retracted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                signed.manifest.name,
                signed.manifest.version,
                signed.manifest_hash,
                signed.manifest.author_key,
                pack_json,
                created,
            ],
        ));
        Ok(json!({
            "pack": signed.manifest.name,
            "version": signed.manifest.version,
            "hash": signed.manifest_hash,
            "author_key": signed.manifest.author_key,
            "children": signed.manifest.children.len(),
            "evidence": signed.manifest.evidence.len(),
        }))
    }

    pub(super) fn reliquary_retract(&self, p: Value) -> Result<Value, RpcErr> {
        let p: RetractArgs = parse(p)?;
        let mut pack = self.load_local_pack(&p.name, p.version)?;
        let reason = p.reason.unwrap_or_else(|| "retracted by author".into());
        pack.manifest.retracted = Some(reason.clone());
        let signed = reliquary::sign_pack(&self.key, pack.manifest);
        let pack_json = serde_json::to_string(&signed).unwrap_or_default();
        self.persisted(self.db.lock().unwrap().execute(
            "UPDATE reliquary_packs SET hash=?3, pack_json=?4, retracted=?5 WHERE name=?1 AND version=?2",
            params![
                signed.manifest.name,
                signed.manifest.version,
                signed.manifest_hash,
                pack_json,
                reason,
            ],
        ));
        Ok(json!({
            "pack": signed.manifest.name,
            "version": signed.manifest.version,
            "hash": signed.manifest_hash,
            "retracted": reason,
        }))
    }

    pub(super) fn reliquary_add_peer(&self, p: Value) -> Result<Value, RpcErr> {
        let p: PeerNameArgs = parse(p)?;
        let (url, key) = self.peer_endpoint(&p.peer)?;
        self.persisted(self.db.lock().unwrap().execute(
            "INSERT OR REPLACE INTO reliquary_peers (peer, added) VALUES (?1, ?2)",
            params![p.peer, now_ms()],
        ));
        Ok(json!({
            "peer": p.peer,
            "url": url,
            "key": key.text(),
        }))
    }

    pub(super) fn reliquary_remove_peer(&self, p: Value) -> Result<Value, RpcErr> {
        let p: PeerNameArgs = parse(p)?;
        let n = self
            .db
            .lock()
            .unwrap()
            .execute("DELETE FROM reliquary_peers WHERE peer=?1", params![p.peer])
            .map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::not_found(format!(
                "peer `{}` is not in the reliquary list",
                p.peer
            )));
        }
        Ok(json!({"removed": p.peer}))
    }

    pub(super) fn reliquary_local_catalog(&self) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare(
                "SELECT name, version, hash, author_key, pack_json, created, retracted
                 FROM reliquary_packs ORDER BY name, version DESC",
            )
            .map_err(sql)?;
        let rows = st
            .query_map([], |r| {
                let pack_json: String = r.get(4)?;
                let parsed: Option<SignedPack> = serde_json::from_str(&pack_json).ok();
                let (desc, children, evidence) = parsed
                    .map(|p| {
                        (
                            p.manifest.description,
                            p.manifest.children.len(),
                            p.manifest.evidence.len(),
                        )
                    })
                    .unwrap_or_default();
                Ok(json!({
                    "name": r.get::<_, String>(0)?,
                    "version": r.get::<_, u32>(1)?,
                    "hash": r.get::<_, String>(2)?,
                    "author_key": r.get::<_, String>(3)?,
                    "description": desc,
                    "children": children,
                    "evidence": evidence,
                    "created": r.get::<_, i64>(5)?,
                    "retracted": r.get::<_, Option<String>>(6)?,
                }))
            })
            .map_err(sql)?;
        let items: Vec<Value> = rows.collect::<Result<_, _>>().map_err(sql)?;
        Ok(Value::Array(items))
    }

    pub(super) fn reliquary_list(&self, p: Value) -> Result<Value, RpcErr> {
        let p: ListArgs = parse(p)?;
        match p.peer {
            None => {
                let packs = self.reliquary_local_catalog()?;
                let db = self.db.lock().unwrap();
                let mut st = db
                    .prepare("SELECT peer, added FROM reliquary_peers ORDER BY peer")
                    .map_err(sql)?;
                let peers: Vec<Value> = st
                    .query_map([], |r| {
                        Ok(json!({"peer": r.get::<_, String>(0)?, "added": r.get::<_, i64>(1)?}))
                    })
                    .map_err(sql)?
                    .collect::<Result<_, _>>()
                    .map_err(sql)?;
                Ok(json!({"packs": packs, "peers": peers}))
            }
            Some(peer) => {
                let packs = self.reliquary_send(&peer, "reliquary_catalog", json!({}))?;
                Ok(json!({"peer": peer, "packs": packs}))
            }
        }
    }

    /// Fetch and strictly verify a pack from local store or a pinned peer.
    fn fetch_verified_pack(
        &self,
        peer: Option<&str>,
        name: &str,
        version: Option<u32>,
    ) -> Result<(SignedPack, PublicKey), RpcErr> {
        match peer {
            None => {
                let pack = self.load_local_pack(name, version)?;
                let key = self.key.public();
                reliquary::verify_pack(&pack, &key)
                    .map_err(|e| RpcErr::new("verification_failed", 15, e))?;
                Ok((pack, key))
            }
            Some(peer_name) => {
                let (_, key) = self.peer_endpoint(peer_name)?;
                let raw = self.reliquary_send(
                    peer_name,
                    "reliquary_fetch",
                    json!({"name": name, "version": version}),
                )?;
                let pack: SignedPack = serde_json::from_value(raw).map_err(|e| {
                    RpcErr::new("verification_failed", 15, format!("invalid pack payload: {e}"))
                })?;
                reliquary::verify_pack(&pack, &key)
                    .map_err(|e| RpcErr::new("verification_failed", 15, e))?;
                Ok((pack, key))
            }
        }
    }

    pub(super) fn reliquary_show(&self, p: Value) -> Result<Value, RpcErr> {
        let p: PackRefArgs = parse(p)?;
        let (pack, key) = self.fetch_verified_pack(p.peer.as_deref(), &p.name, p.version)?;
        let effects = self.preview_pack_effects(&pack)?;
        Ok(json!({
            "pack": pack.manifest.name,
            "version": pack.manifest.version,
            "hash": pack.manifest_hash,
            "author_key": pack.manifest.author_key,
            "author_id": key.id(),
            "verified": true,
            "description": pack.manifest.description,
            "retracted": pack.manifest.retracted,
            "children": pack.manifest.children.iter().map(|c| json!({"name": c.name, "version": c.version, "hash": c.hash})).collect::<Vec<_>>(),
            "evidence": pack.manifest.evidence,
            "effects": effects,
            "grants_included": false,
        }))
    }

    pub(super) fn reliquary_export(&self, p: Value) -> Result<Value, RpcErr> {
        let p: ExportArgs = parse(p)?;
        let pack = self.load_local_pack(&p.name, p.version)?;
        reliquary::verify_pack(&pack, &self.key.public())
            .map_err(|e| RpcErr::new("verification_failed", 15, e))?;
        let text = serde_json::to_string_pretty(&pack).unwrap_or_default() + "\n";
        std::fs::write(&p.file, text)
            .map_err(|e| RpcErr::invalid(format!("cannot write {}: {e}", p.file)))?;
        Ok(json!({
            "pack": pack.manifest.name,
            "version": pack.manifest.version,
            "hash": pack.manifest_hash,
            "file": p.file,
        }))
    }

    pub(super) fn reliquary_import(&self, p: Value) -> Result<Value, RpcErr> {
        let p: ImportArgs = parse(p)?;
        let text = std::fs::read_to_string(&p.file)
            .map_err(|e| RpcErr::not_found(format!("cannot read {}: {e}", p.file)))?;
        let pack: SignedPack = serde_json::from_str(&text)
            .map_err(|e| RpcErr::invalid(format!("invalid pack file {}: {e}", p.file)))?;
        let trusted = match (p.key.as_deref(), p.peer.as_deref()) {
            (Some(k), _) => PublicKey::parse(k).map_err(RpcErr::invalid)?,
            (None, Some(peer)) => self.peer_endpoint(peer)?.1,
            (None, None) => {
                // Check if author_key matches this core or a pinned peer; never trust an unpinned key.
                let claimed = PublicKey::parse(&pack.manifest.author_key)
                    .map_err(|e| RpcErr::new("verification_failed", 15, e))?;
                if claimed == self.key.public() {
                    claimed
                } else if let Some((_, pk)) = self.peer_by_key(&claimed.id()) {
                    pk
                } else {
                    return Err(RpcErr::new(
                        "verification_failed",
                        15,
                        "pack author key is not pinned; pass `key ed25519:...` or pin the peer first",
                    ));
                }
            }
        };
        reliquary::verify_pack(&pack, &trusted)
            .map_err(|e| RpcErr::new("verification_failed", 15, e))?;
        self.inscribe_verified_pack(&pack, &trusted, p.replace)
    }

    pub(super) fn reliquary_inscribe(&self, p: Value) -> Result<Value, RpcErr> {
        let p: InscribePackArgs = parse(p)?;
        let (pack, key) = self.fetch_verified_pack(p.peer.as_deref(), &p.name, p.version)?;
        self.inscribe_verified_pack(&pack, &key, p.replace)
    }

    /// Compute the static effect plan of a pack's root ritual using its bundled child closure.
    fn preview_pack_effects(&self, pack: &SignedPack) -> Result<Vec<Value>, RpcErr> {
        let resolve = |name: &str, ver: Option<u32>| -> Option<ritual::Resolved> {
            pack.manifest
                .children
                .iter()
                .rev()
                .find(|c| c.name == name && ver.is_none_or(|v| c.version == v))
                .map(|c| (c.version, c.hash.clone(), c.definition.clone()))
        };
        let effects = ritual::validate_with_cfg(
            &pack.manifest.root.definition,
            &resolve,
            &self.cfg,
            self.cfg.max_depth(),
        )
        .map_err(|e| RpcErr::invalid(e.join("; ")))?;
        Ok(effects
            .into_iter()
            .map(|e| {
                json!({
                    "step": e.step,
                    "kind": e.kind,
                    "what": e.what,
                    "effect": e.effect,
                    "determinism": e.determinism,
                    "notes": e.notes,
                })
            })
            .collect())
    }

    /// Inscribe a verified pack's full closure into the local grimoire.
    /// Never transfers grants; records author-claimed evidence separately from local baselines.
    fn inscribe_verified_pack(
        &self,
        pack: &SignedPack,
        trusted: &PublicKey,
        replace: bool,
    ) -> Result<Value, RpcErr> {
        if let Some(ref why) = pack.manifest.retracted {
            return Err(RpcErr::conflict(format!(
                "pack `{}` v{} was retracted by its author: {why}",
                pack.manifest.name, pack.manifest.version
            )));
        }
        let effects = self.preview_pack_effects(pack)?;
        let mut inscribed = Vec::new();
        let all_rituals: Vec<&PackRitual> = pack
            .manifest
            .children
            .iter()
            .chain(std::iter::once(&pack.manifest.root))
            .collect();

        // Check for hash conflicts before writing anything.
        for pr in &all_rituals {
            if let Some((existing_v, existing_h, _)) = self.load_ritual(&pr.name, None)? {
                if existing_h != pr.hash && !replace {
                    return Err(RpcErr::conflict(format!(
                        "ritual `{}` already exists locally at v{existing_v} with a different hash; pass `replace` to save a new version",
                        pr.name
                    )));
                }
            }
        }

        // Inscribe missing or changed rituals in topological order (children first, root last).
        let now = now_ms();
        for pr in &all_rituals {
            if let Some((existing_v, existing_h, _)) = self.load_ritual(&pr.name, None)? {
                if existing_h == pr.hash {
                    inscribed.push(json!({"ritual": pr.name, "version": existing_v, "hash": existing_h, "status": "unchanged"}));
                    continue;
                }
            }
            let db = self.db.lock().unwrap();
            let next_v: u32 = db
                .query_row(
                    "SELECT COALESCE(MAX(version), 0) + 1 FROM rituals WHERE name=?1",
                    params![pr.name],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            let def_json = serde_json::to_string(&pr.definition).unwrap_or_default();
            self.persisted(db.execute(
                "INSERT INTO rituals (name, version, hash, def, created) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![pr.name, next_v, pr.hash, def_json, now],
            ));
            inscribed.push(json!({"ritual": pr.name, "version": next_v, "hash": pr.hash, "status": "inscribed"}));
        }

        // Record author-claimed evidence separately from local baselines.
        for ev in &pack.manifest.evidence {
            let ev_json = serde_json::to_string(ev).unwrap_or_default();
            self.persisted(self.db.lock().unwrap().execute(
                "INSERT OR REPLACE INTO reliquary_evidence (ritual, version, author_key, pack_hash, evidence_json, imported)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    ev.ritual,
                    ev.version,
                    pack.manifest.author_key,
                    pack.manifest_hash,
                    ev_json,
                    now,
                ],
            ));
        }

        // Compute missing grants on the root ritual (grants never travel).
        let grants = self.grants_for(&pack.manifest.root.name)?;
        let missing: Vec<String> = pack
            .manifest
            .root
            .definition
            .requires
            .iter()
            .flat_map(Requirement::atoms)
            .filter(|n| !grants.iter().any(|g| g.covers(n)))
            .map(|g| g.describe())
            .collect();

        Ok(json!({
            "pack": pack.manifest.name,
            "version": pack.manifest.version,
            "hash": pack.manifest_hash,
            "author_key": pack.manifest.author_key,
            "author_id": trusted.id(),
            "inscribed": inscribed,
            "effects": effects,
            "missing_grants": missing,
            "author_claimed_evidence": pack.manifest.evidence.len(),
            "grants_transferred": false,
        }))
    }

    /// Handle signed peer reliquary reads (`reliquary_catalog`, `reliquary_fetch`).
    pub(super) fn dispatch_reliquary_peer(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcErr> {
        match method {
            "reliquary_catalog" => self.reliquary_local_catalog(),
            "reliquary_fetch" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct FetchArgs {
                    name: String,
                    #[serde(default)]
                    version: Option<u32>,
                }
                let p: FetchArgs = parse(params)?;
                let pack = self.load_local_pack(&p.name, p.version)?;
                serde_json::to_value(pack).map_err(|e| RpcErr::invalid(e.to_string()))
            }
            _ => Err(RpcErr::new(
                "not_authorized",
                4,
                format!("`{method}` is not a reliquary peer method"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_core(dir: &std::path::Path) -> Arc<Core> {
        let paths = Paths {
            config: dir.join("config"),
            state: dir.join("state"),
            runtime: dir.join("run"),
        };
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::create_dir_all(&paths.state).unwrap();
        std::fs::create_dir_all(&paths.runtime).unwrap();
        Core::open(paths).unwrap()
    }

    #[test]
    fn publish_export_import_inscribes_closure_without_grants() {
        let tmp = tempfile_dir("reliquary-closure");
        let a = test_core(&tmp.join("a"));
        let b = test_core(&tmp.join("b"));

        // Write a child + parent ritual with a mail requirement (never granted on B).
        let child_path = tmp.join("leaf.yaml");
        std::fs::write(
            &child_path,
            "ritual: leaf\ninputs:\n  x: {type: string}\nessence: {requests: 0, time: 1m}\nsteps:\n  - id: s1\n    operation: data.trim\n    arguments: {text: \"${inputs.x}\"}\nreturn:\n  out: \"${steps.s1.output}\"\n",
        )
        .unwrap();
        a.inscribe(json!({"path": child_path.to_str().unwrap()})).unwrap();

        let parent_path = tmp.join("root.yaml");
        std::fs::write(
            &parent_path,
            "ritual: root\ninputs:\n  x: {type: string}\nrequires:\n  - reagent: mail\n    action: locate\nessence: {requests: 0, time: 1m}\nsteps:\n  - id: c1\n    ritual: leaf\n    arguments: {x: \"${inputs.x}\"}\n  - id: loc\n    operation: mail.locate\n    arguments: {message: \"${steps.c1.output.out}\"}\nreturn:\n  out: \"${steps.c1.output.out}\"\n",
        )
        .unwrap();
        a.inscribe(json!({"path": parent_path.to_str().unwrap()})).unwrap();
        // Grant mail.locate on A.
        a.grant(json!({"ritual": "root", "scopes": [{"reagent": "mail", "select": ["locate"]}]}))
            .unwrap();

        let pub_res = a.reliquary_publish(json!({"ritual": "root"})).unwrap();
        assert_eq!(pub_res["children"], 1);

        let pack_file = tmp.join("root.pack.json");
        a.reliquary_export(json!({"name": "root", "file": pack_file.to_str().unwrap()}))
            .unwrap();

        // Importing on B without pinning or passing key must fail.
        let err = b
            .reliquary_import(json!({"file": pack_file.to_str().unwrap()}))
            .unwrap_err();
        assert_eq!(err.exit, 15);

        // Importing with explicit key succeeds, inscribes both rituals, transfers zero grants.
        let imp = b
            .reliquary_import(json!({
                "file": pack_file.to_str().unwrap(),
                "key": a.key.public().text(),
            }))
            .unwrap();
        assert_eq!(imp["grants_transferred"], false);
        assert_eq!(imp["inscribed"].as_array().unwrap().len(), 2);
        assert!(b.load_ritual("leaf", None).unwrap().is_some());
        assert!(b.load_ritual("root", None).unwrap().is_some());
        assert!(b.grants_for("root").unwrap().is_empty());
        assert_eq!(imp["missing_grants"].as_array().unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn tempfile_dir(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("summonr-test-{label}-{}", new_id("t")));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
