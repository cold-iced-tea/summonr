use crate::grammar::Op;
use crate::rpc::{Client, RpcErr};
use crate::theme::{self, bold, paint, Ink};
use crate::util::{fmt_ago, now_ms};
use crate::{epitaph, finish, keys, row, rubric, s, short};
use serde_json::{json, Value};


pub(crate) fn epitaph_command(op: &Op, client: &mut Client) -> i32 {
    let p = &op.positionals;
    match (p.first().map(String::as_str), p.len()) {
        (Some("prove"), 2) => finish(
            client.call("epitaph_prove", json!({"run": p[1]})),
            op.json,
            |d| {
                rubric(&format!("MERKLE PROOF · {}", s(d, "run")));
                row("seq", &format!("#{} of {}", d["seq"], d["count"]));
                row("leaf", s(d, "leaf_hash"));
                row("merkle_root", s(d, "merkle_root"));
                row("steps", &format!("{}", d["proof"].as_array().map(Vec::len).unwrap_or(0)));
            },
        ),
        (Some("export"), 4) => {
            let (from, to) = match (p[1].parse::<i64>(), p[2].parse::<i64>()) {
                (Ok(f), Ok(t)) => (f, t),
                _ => return finish(Err(RpcErr::invalid("use: epitaph export FROM TO FILE")), op.json, |_| {}),
            };
            let res = client.call("epitaph_export", json!({"from": from, "to": to})).and_then(|d| {
                std::fs::write(&p[3], serde_json::to_string_pretty(&d).unwrap())
                    .map_err(|e| RpcErr::new("storage_failed", 11, format!("cannot write {}: {e}", p[3])))?;
                Ok(json!({"file": p[3], "count": d["epitaphs"].as_array().map(Vec::len)}))
            });
            finish(res, op.json, |d| println!("wrote {} epitaph(s) to {}", d["count"], s(d, "file")))
        }
        (Some(run), 3) if p[1] == "reveal" => finish(
            client.call("epitaph_reveal", json!({"run": run, "part": p[2]})),
            op.json,
            |d| println!("{}", serde_json::to_string_pretty(d).unwrap()),
        ),
        (Some(run), 1) => finish(client.call("epitaph_get", json!({"run": run})), op.json, |d| {
            let body: Value = serde_json::from_str(s(d, "body")).unwrap_or_default();
            let link: Value = serde_json::from_str(s(d, "link")).unwrap_or_default();
            rubric(&format!(
                "EPITAPH · {} v{} · {}",
                s(&body["ritual"], "name"),
                body["ritual"]["version"],
                s(&body, "outcome")
            ));
            let by = match s(&body, "caller") {
                "" => "the owner".to_string(),
                other => other.to_string(),
            };
            row("run", &format!("{}   by {by}", s(&body, "run")));
            row("chain", &format!("#{}   after {}", d["seq"], short(s(&link, "prev"))));
            let events = body["fingerprints"]["events"]["count"].as_u64().unwrap_or(0);
            let children = body["children"].as_array().map(Vec::len).unwrap_or(0);
            row("contents", &format!("inputs, result and {events} event(s); {children} child run(s) — fingerprints only"));
            let key = keys::PublicKey::parse(s(d, "core_key")).map(|k| k.id()).unwrap_or_default();
            let mark = if d["verifies"] == true { paint(Ink::Verdigris, "✓ verifies") } else { paint(Ink::Rubric, "✕ does not verify") };
            row("signed by", &format!("{}   {mark}", short(&key)));
            if theme::tty() {
                for line in theme::seal(s(d, "hash")).into_iter().flatten() {
                    println!("              {}", paint(Ink::Gold, &line));
                }
            }
        }),
        _ => finish(
            Err(RpcErr::invalid("use: epitaph RUN | epitaph RUN reveal PART | epitaph prove RUN | epitaph verify FILE | epitaph export FROM TO FILE")),
            op.json,
            |_| {},
        ),
    }
}

pub(crate) fn chain_command(op: &Op, client: &mut Client) -> i32 {
    let marking = match op.positionals.as_slice() {
        [] => false,
        [m] if m == "mark" => true,
        _ => return finish(Err(RpcErr::invalid("use: chain | chain mark")), op.json, |_| {}),
    };
    finish(client.call("chain_head", json!({})), op.json, |d| {
        if d["seq"] == 0 {
            println!("{}", paint(Ink::Dim, "The chain is empty: no run has ended since epitaphs began."));
            return;
        }
        if marking {
            println!("{}  {}", s(d, "mark"), paint(Ink::Dim, &crate::util::now_iso()));
            println!("{}", paint(Ink::Dim, "  Paste this anywhere other people can see it: an email, a chat, a notebook."));
            return;
        }
        rubric("CHAIN · this core");
        row("head", &format!("#{}", d["seq"]));
        row("hash", &keys::grouped(s(d, "hash")));
        if theme::tty() {
            for line in theme::seal(s(d, "hash")).into_iter().flatten() {
                println!("              {}", paint(Ink::Gold, &line));
            }
        }
    })
}

pub(crate) fn witness_command(op: &Op, client: &mut Client) -> i32 {
    let p = &op.positionals;
    match (p.first().map(String::as_str), p.len()) {
        (Some("accept"), 2) => match &op.key {
            Some(k) => finish(client.call("witness_accept", json!({"name": p[1], "key": k})), op.json, |d| {
                rubric(&format!("WARD · {}", s(d, "name")));
                show_fingerprint(s(d, "id"));
                println!("{}", paint(Ink::Gold, COMPARE));
            }),
            None => finish(Err::<Value, _>(RpcErr::invalid("give the ward core's key: witness accept NAME key ed25519:…")), op.json, |_| {}),
        },
        (Some("list"), 1) => finish(client.call("witness_list", json!({})), op.json, |d| {
            let list = d.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                println!("{}", paint(Ink::Dim, "This core witnesses no one. Admit a core: summonr witness accept NAME key K"));
            }
            for w in list {
                let held = if w["seq"] == 0 { "not yet witnessed".to_string() } else { format!("witnessed through #{} {}", w["seq"], fmt_ago(w["time"].as_i64().unwrap_or(0))) };
                let forks = match w["forks"].as_i64().unwrap_or(0) {
                    0 => String::new(),
                    n => paint(Ink::Rubric, &format!("  {n} fork(s): this ward offered a rewritten history")),
                };
                println!("  {} {}{forks}", bold(Ink::Bone, s(&w, "name")), paint(Ink::Dim, &held));
            }
        }),
        (Some("drop"), 2) => finish(client.call("witness_drop", json!({"name": p[1]})), op.json, |d| println!("ward {} dropped", s(d, "name"))),
        (Some("tsa"), 2) => finish(client.call("witness_tsa", json!({"url": p[1]})), op.json, |d| {
            println!("{} {} timestamped head #{}", paint(Ink::Verdigris, "✓"), s(d, "url"), d["seq"])
        }),
        (Some("compare"), n) if n >= 1 => {
            let peers: Vec<&str> = p[1..].iter().map(String::as_str).collect();
            let args = if peers.is_empty() {
                json!({"live": op.live})
            } else {
                json!({"peers": peers, "live": op.live})
            };
            let res = client.call("witness_compare", args);
            let ok = res.as_ref().map(|d| d["ok"] == true).unwrap_or(false);
            let no_witnesses = res
                .as_ref()
                .map(|d| d["witnesses"].as_u64() == Some(0))
                .unwrap_or(false);
            let code = finish(res, op.json, |d| {
                if d["witnesses"].as_u64() == Some(0) {
                    println!("{} no witnesses to compare", paint(Ink::Gold, "?"));
                } else if d["ok"] == true {
                    println!("{} witnesses agree (no split views detected)", paint(Ink::Verdigris, "✓"));
                } else {
                    println!("{} witness comparison failed (split_view={}, invalid={})", paint(Ink::Rubric, "✕"), d["split_view"], d["invalid"]);
                    for c in d["conflicts"].as_array().into_iter().flatten() {
                        println!("  #{} {} ({}) vs {} ({})", c["seq"], c["a"], c["hash_a"], c["b"], c["hash_b"]);
                    }
                }
            });
            if code != 0 {
                code
            } else if no_witnesses {
                13
            } else if ok {
                0
            } else {
                15
            }
        }
        (Some(peer), 1) => finish(client.call("witness_to", json!({"peer": peer})), op.json, |d| {
            println!("{} {} witnessed head #{}", paint(Ink::Verdigris, "✓"), s(d, "peer"), d["seq"])
        }),
        _ => finish(
            Err::<Value, _>(RpcErr::invalid("use: witness PEER | witness accept NAME key K | witness list | witness drop NAME | witness tsa URL | witness compare [PEER...] [--live]")),
            op.json,
            |_| {},
        ),
    }
}

/// The key a file must verify against: `key K`, or this core's own key or a
/// pinned peer's, never one the file merely claims.
pub(crate) fn trusted_key(op: &Op, client: &mut Client, claimed: &str) -> Result<keys::PublicKey, RpcErr> {
    if let Some(k) = &op.key {
        return keys::PublicKey::parse(k).map_err(RpcErr::invalid);
    }
    let me = client.call("identity", json!({}))?;
    let peers = client.call("peer_list", json!({}))?;
    let pinned = std::iter::once(me["key"].clone()).chain(peers.as_array().into_iter().flatten().map(|p| p["key"].clone()));
    if pinned.into_iter().any(|k| k.as_str() == Some(claimed)) {
        return keys::PublicKey::parse(claimed).map_err(RpcErr::invalid);
    }
    Err(RpcErr::invalid("this file is from a core you haven't pinned; give its key: verify FILE key ed25519:…"))
}

pub(crate) fn verify_command(op: &Op, client: &mut Client) -> i32 {
    let p = &op.positionals;
    let failed = |why: String, json: bool| -> i32 {
        let shown = finish(Err::<Value, _>(RpcErr::new("verification_failed", 15, why)), json, |_| {});
        if shown == 0 { 15 } else { shown }
    };
    // This core's own chain, optionally checking a mark.
    if p.is_empty() || (p.len() == 1 && p[0].starts_with("summonr-mark:")) {
        let mut args = json!({});
        if let Some(m) = p.first() {
            args["mark"] = json!(m);
        }
        if let Some(ca) = &op.tsa_ca {
            args["tsa_ca"] = json!(ca);
        }
        let res = client.call("chain_verify", args);
        let ok = res.as_ref().map(|d| d["ok"] == true).unwrap_or(false);
        let code = finish(res, op.json, |d| {
            match &d["problem"] {
                Value::Null => println!("{} {} epitaph(s) verify", paint(Ink::Verdigris, "✓"), d["checked"]),
                pr => println!("{} epitaph {}: {}", paint(Ink::Rubric, "✕"), pr["seq"], s(pr, "reason")),
            }
            for run in d["not_checkable"].as_array().into_iter().flatten() {
                println!("  {} {}: its elegy is gone, so its contents can't be re-checked", paint(Ink::Gold, "?"), run.as_str().unwrap_or(""));
            }
            match (&d["mark"]["added_since"], d["mark"]["error"].as_str()) {
                (Value::Number(n), _) => println!("{} the chain still contains this mark; {n} epitaph(s) added since, none altered", paint(Ink::Verdigris, "✓")),
                (_, Some(e)) => println!("{} {e}", paint(Ink::Rubric, "✕")),
                _ => {}
            }
            for w in d["witnesses"].as_array().into_iter().flatten() {
                println!("  witness {}: last countersigned #{} {}", s(w, "peer"), w["seq"], fmt_ago(w["time"].as_i64().unwrap_or(0)));
                if w["valid"] == false {
                    println!("    {} countersignature does not verify under the pinned witness key", paint(Ink::Rubric, "✕"));
                }
                if w["matches"] == false {
                    println!("    {} it countersigned a different epitaph #{}: this history was rewritten after it was witnessed", paint(Ink::Rubric, "✕"), w["seq"]);
                }
            }
            for t in d["timestamps"].as_array().into_iter().flatten() {
                println!("  timestamp #{} from {}: {}", t["seq"], s(t, "url"), s(t, "verdict"));
                if t["matches"] == false {
                    println!("    {} it timestamped a different epitaph #{}: this history was rewritten after it was timestamped", paint(Ink::Rubric, "✕"), t["seq"]);
                }
            }
        });
        return if code != 0 { code } else if ok { 0 } else { 15 };
    }
    // A file: an export (optionally with a mark) or a reveal.
    let file: Value = match std::fs::read_to_string(&p[0]).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
        Ok(v) => v,
        Err(e) => return finish(Err::<Value, _>(RpcErr::invalid(format!("{}: {e}", p[0]))), op.json, |_| {}),
    };
    if file["kind"] == "pact-thread" {
        let path = std::fs::canonicalize(&p[0]).map(|x| x.display().to_string()).unwrap_or_else(|_| p[0].clone());
        let res = client.call("pact_verify", json!({"file": path, "key": op.key}));
        let ok = res.as_ref().map(|d| d["ok"] == true).unwrap_or(false);
        let code = finish(res, op.json, |d| {
            if d["ok"] == true {
                println!("{} the pact thread verifies: {} entries, {} call(s), {} delivery epitaph(s) checked", paint(Ink::Verdigris, "✓"), d["entries"], d["calls"], d["epitaphs_checked"]);
            } else if let Some(f) = d["forks"].as_array().filter(|f| !f.is_empty()) {
                println!("{} {} fork(s): a core signed two different entries on the same previous entry — signed evidence that it equivocated", paint(Ink::Rubric, "✕"), f.len());
            } else {
                println!("{} entry {}: {}", paint(Ink::Rubric, "✕"), d["problem"]["entry"], s(&d["problem"], "reason"));
            }
        });
        return if code != 0 { code } else if ok { 0 } else { 15 };
    }
    if file.get("reveal").is_some() {
        let e = &file["epitaph"];
        let key = match trusted_key(op, client, s(e, "core_key")) {
            Ok(k) => k,
            Err(err) => return finish(Err::<Value, _>(err), op.json, |_| {}),
        };
        let checked = epitaph::check_link(s(e, "link"), s(e, "sig"), &key)
            .and_then(|l| epitaph::check_body(&l, s(e, "body")))
            .and_then(|b| epitaph::verify_reveal(&b, &file["reveal"]));
        return match checked {
            Ok(()) => finish(Ok(json!({"ok": true})), op.json, |_| {
                println!("{} {} of {} matches its epitaph", paint(Ink::Verdigris, "✓"), s(&file["reveal"], "part"), s(&file["reveal"], "run"))
            }),
            Err(why) => failed(why, op.json),
        };
    }
    let key = match trusted_key(op, client, s(&file, "core_key")) {
        Ok(k) => k,
        Err(err) => return finish(Err::<Value, _>(err), op.json, |_| {}),
    };
    match epitaph::verify_export(&file, &key) {
        Err((seq, why)) => failed(format!("epitaph {seq}: {why}"), op.json),
        Ok(last) => match p.get(1) {
            None => finish(Ok(json!({"ok": true, "last": last})), op.json, |_| {
                println!("{} the exported chain verifies through #{last}", paint(Ink::Verdigris, "✓"))
            }),
            Some(m) => match epitaph::parse_mark(m).ok_or("not a chain mark".to_string()).and_then(|mk| epitaph::verify_mark(&file, &mk)) {
                Ok(n) => finish(Ok(json!({"ok": true, "added_since": n})), op.json, |_| {
                    println!("{} the chain still contains this mark; {n} epitaph(s) added since, none altered", paint(Ink::Verdigris, "✓"))
                }),
                Err(why) => failed(why, op.json),
            },
        },
    }
}

/// The full fingerprint for comparing by eye, with its seal on a terminal.
pub(crate) fn show_fingerprint(id: &str) {
    row("fingerprint", &keys::grouped(id));
    if theme::tty() {
        for line in theme::seal(id).into_iter().flatten() {
            println!("              {}", paint(Ink::Gold, &line));
        }
    }
}

const COMPARE: &str = "Compare this fingerprint with the other device before relying on it.";

pub(crate) fn familiar_command(op: &Op, client: &mut Client) -> i32 {
    let p = &op.positionals;
    let name = p.get(1).cloned().unwrap_or_default();
    let res = match (p.first().map(String::as_str), p.len()) {
        (None | Some("list"), 0 | 1) => {
            return finish(client.call("familiar_list", json!({})), op.json, |d| {
                let list = d.as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    println!("{}", paint(Ink::Dim, "No familiars. Create one: summonr familiar new NAME"));
                }
                for x in list {
                    let state = if x["revoked"].is_null() { "" } else { " (revoked)" };
                    println!("  {} {}{}", bold(Ink::Bone, s(&x, "name")), paint(Ink::Dim, s(&x, "kind")), paint(Ink::Rubric, state));
                    println!("      {}", paint(Ink::Dim, &keys::grouped(s(&x, "id"))));
                    if theme::tty() {
                        for line in theme::seal(s(&x, "id")).into_iter().flatten() {
                            println!("      {}", paint(Ink::Gold, &line));
                        }
                    }
                    println!("      allowed: {}", x["allowed"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).filter(|s| !s.is_empty()).unwrap_or_else(|| "nothing".into()));
                }
            });
        }
        (Some("new"), 2) => client.call("familiar_new", json!({"name": name})),
        (Some("add"), 2) => match &op.key {
            Some(k) => client.call("familiar_add", json!({"name": name, "key": k})),
            None => Err(RpcErr::invalid("give the public key: familiar add NAME key ed25519:…")),
        },
        (Some("register-grant"), 2) => {
            match std::fs::read_to_string(&p[1])
                .map_err(|e| RpcErr::invalid(format!("cannot read {}: {e}", p[1])))
                .and_then(|t| {
                    serde_json::from_str::<Value>(&t)
                        .map_err(|e| RpcErr::invalid(format!("invalid grant JSON: {e}")))
                }) {
                Ok(parsed) => {
                    let grant = parsed.get("data").cloned().unwrap_or(parsed);
                    let mut args = json!({"grant": grant});
                    if let Some(k) = &op.key {
                        args["root_key"] = json!(k);
                    }
                    client.call("familiar_register_grant", args)
                }
                Err(e) => Err(e),
            }
        }
        (Some(verb @ ("allow" | "disallow")), n) if n >= 3 || (n == 2 && op.pact.is_some()) => client.call(
            if verb == "allow" { "familiar_allow" } else { "familiar_disallow" },
            json!({"name": name, "rituals": &p[2..], "pacts": op.pact.iter().collect::<Vec<_>>()}),
        ),
        (Some("revoke"), 2) => client.call("familiar_revoke", json!({"name": name})),
        _ => Err(RpcErr::invalid(
            "use: familiar [list] | new NAME | add NAME key K | allow NAME RITUAL... | disallow NAME RITUAL... | revoke NAME",
        )),
    };
    let verb = p.first().cloned().unwrap_or_default();
    finish(res, op.json, |d| match verb.as_str() {
        "new" => {
            rubric(&format!("FAMILIAR · {}", s(d, "name")));
            row("key", s(d, "key"));
            show_fingerprint(s(d, "id"));
            row("key file", s(d, "key_file"));
            println!("{}", paint(Ink::Dim, "It may call nothing yet: summonr familiar allow NAME RITUAL..."));
        }
        "add" => {
            rubric(&format!("FAMILIAR · {} (remote)", s(d, "name")));
            show_fingerprint(s(d, "id"));
            println!("{}", paint(Ink::Gold, COMPARE));
        }
        "register-grant" => {
            rubric(&format!("FAMILIAR · {} (delegated)", s(d, "name")));
            row("root", s(d, "root_key_id"));
            row("allowed", &d["allowed"].to_string());
        }
        "allow" | "disallow" => println!("{} may call: {}", s(d, "name"), d["allowed"]),
        _ => println!("{} revoked; its next request will be refused.", s(d, "name")),
    })
}

pub(crate) fn peer_command(op: &Op, client: &mut Client) -> i32 {
    let p = &op.positionals;
    let name = p.get(1).cloned().unwrap_or_default();
    let res = match (p.first().map(String::as_str), p.len()) {
        (None | Some("list"), 0 | 1) => {
            return finish(client.call("peer_list", json!({})), op.json, |d| {
                let list = d.as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    println!("{}", paint(Ink::Dim, "No peers pinned."));
                }
                for x in list {
                    println!("  {} {}", bold(Ink::Bone, s(&x, "name")), s(&x, "url"));
                    println!("      {}", paint(Ink::Dim, &keys::grouped(s(&x, "id"))));
                }
            });
        }
        (Some("add"), 2) => match (&op.url, &op.key) {
            (Some(u), Some(k)) => client.call("peer_add", json!({"name": name, "url": u, "key": k})),
            _ => Err(RpcErr::invalid("give both: peer add NAME url URL key ed25519:…")),
        },
        (Some("remove"), 2) => client.call("peer_remove", json!({"name": name})),
        _ => Err(RpcErr::invalid("use: peer [list] | add NAME url URL key K | remove NAME")),
    };
    let adding = p.first().map(String::as_str) == Some("add");
    finish(res, op.json, |d| {
        if adding {
            rubric(&format!("PEER · {} at {}", s(d, "name"), s(d, "url")));
            show_fingerprint(s(d, "id"));
            println!("{}", paint(Ink::Gold, COMPARE));
        } else {
            println!("peer {} removed", s(d, "name"));
        }
    })
}


pub(crate) fn epitaph_verify_command(op: &Op) -> i32 {
    let res = (|| -> Result<Value, RpcErr> {
        let path = op
            .positionals
            .get(1)
            .ok_or_else(|| RpcErr::invalid("use: epitaph verify FILE (or `-` for stdin)"))?;
        let text = if path == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| RpcErr::invalid(format!("stdin: {e}")))?;
            buf
        } else {
            std::fs::read_to_string(path)
                .map_err(|e| RpcErr::invalid(format!("cannot read {path}: {e}")))?
        };
        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| RpcErr::invalid(format!("invalid proof JSON: {e}")))?;
        let data = parsed.get("data").unwrap_or(&parsed);
        let leaf_hash = data["leaf_hash"]
            .as_str()
            .ok_or_else(|| RpcErr::invalid("proof JSON missing `leaf_hash`"))?;
        let root = data["merkle_root"]
            .as_str()
            .ok_or_else(|| RpcErr::invalid("proof JSON missing `merkle_root`"))?;
        let proof: Vec<epitaph::MerkleStep> = serde_json::from_value(data["proof"].clone())
            .map_err(|e| RpcErr::invalid(format!("invalid `proof` array: {e}")))?;
        let ok = epitaph::verify_merkle_proof(leaf_hash, root, &proof);
        if !ok {
            return Err(RpcErr::new(
                "verification_failed",
                15,
                "Merkle inclusion proof does not match merkle_root",
            )
            .with(json!({"ok": false, "leaf_hash": leaf_hash, "merkle_root": root})));
        }
        Ok(json!({
            "ok": true,
            "run": data["run"],
            "seq": data["seq"],
            "leaf_hash": leaf_hash,
            "merkle_root": root,
            "steps": proof.len(),
        }))
    })();
    finish(res, op.json, |d| {
        println!(
            "{} Merkle proof verified for {} (#{}, {} step(s))",
            paint(Ink::Verdigris, "✓"),
            s(d, "run"),
            d["seq"],
            d["steps"]
        );
    })
}

pub(crate) fn key_command(op: &Op) -> i32 {
    let p = &op.positionals;
    let res = match (p.first().map(String::as_str), p.len()) {
        (Some("init-root"), 2) => (|| -> Result<Value, RpcErr> {
            let path = std::path::Path::new(&p[1]);
            let kp = keys::generate();
            keys::save_new(path, &kp)
                .map_err(|e| RpcErr::new("storage_failed", 11, e))?;
            let pub_path = path.with_extension("pub");
            let pub_text = kp.public().text();
            let _ = std::fs::write(&pub_path, format!("{pub_text}\n"));
            Ok(json!({
                "key_file": p[1],
                "pub_file": pub_path.display().to_string(),
                "key": pub_text,
                "id": kp.public().id(),
            }))
        })(),
        (Some("sign-grant"), 4) => (|| -> Result<Value, RpcErr> {
            let root_kp = keys::load(std::path::Path::new(&p[1]))
                .map_err(|e| RpcErr::invalid(format!("cannot load root key {}: {e}", p[1])))?;
            let familiar_name = p[2].clone();
            let familiar_pub = keys::PublicKey::parse(&p[3]).map_err(RpcErr::invalid)?;
            let split_list = |key: &str| -> Vec<String> {
                op.args
                    .get(key)
                    .map(|v| {
                        v.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let rituals = split_list("rituals");
            let pacts = split_list("pacts");
            let now = now_ms();
            let expires_at_ms = match op.args.get("expires_days") {
                Some(raw) => {
                    let days: i64 = raw
                        .parse()
                        .map_err(|_| RpcErr::invalid("expires_days must be an integer"))?;
                    if days <= 0 {
                        return Err(RpcErr::invalid("expires_days must be positive"));
                    }
                    Some(now + days * 86_400_000)
                }
                None => None,
            };
            let grant = keys::FamiliarGrant::sign(
                &root_kp,
                familiar_name,
                &familiar_pub,
                rituals,
                pacts,
                now,
                expires_at_ms,
            );
            let val = serde_json::to_value(&grant).map_err(|e| RpcErr::invalid(e.to_string()))?;
            if let Some(out) = op.args.get("out") {
                std::fs::write(out, serde_json::to_string_pretty(&val).unwrap())
                    .map_err(|e| RpcErr::new("storage_failed", 11, format!("cannot write {out}: {e}")))?;
            }
            Ok(val)
        })(),
        _ => Err(RpcErr::invalid(
            "use: key init-root FILE | key sign-grant ROOT_KEY FAMILIAR PUBKEY [given rituals=a,b pacts=p expires_days=30 out=grant.json]",
        )),
    };
    finish(res, op.json, |d| {
        if d.get("key_file").is_some() {
            rubric("ROOT KEY");
            row("key file", s(d, "key_file"));
            row("pub file", s(d, "pub_file"));
            row("public key", s(d, "key"));
            row("id", s(d, "id"));
        } else {
            println!("{}", serde_json::to_string_pretty(d).unwrap_or_default());
        }
    })
}
