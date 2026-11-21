use crate::grammar::Op;
use crate::rpc::{Client, RpcErr};
use crate::theme::{bold, paint, Ink};
use crate::util::{self, fmt_ago, now_ms, Paths};
use crate::{core, finish, row, rubric, s};
use serde_json::{json, Value};


pub(crate) fn job_command(op: &Op, client: &mut Client) -> i32 {
    let result = (|| -> Result<Value, RpcErr> {
        // Job options reuse the paired given/--arg grammar, but are not ritual inputs.
        let verb = op.positionals.first().map(String::as_str).unwrap_or("");
        let allowed: &[&str] = match verb {
            "work" => &["count", "lease"],
            "status" => &["offset"],
            _ => &[],
        };
        if op.args.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(RpcErr::invalid("unknown option for this job operation"));
        }
        if op.model.is_some()
            || !op.settings.is_empty()
            || op.wait
            || op.version.is_some()
            || op.dry_run
            || op.replace
            || op.name.is_some()
            || op.against.is_some()
            || op.key.is_some()
            || op.url.is_some()
            || op.via.is_some()
            || op.baseline
            || op.live
            || op.simulate
            || op.all
            || !op.scopes.is_empty()
            || !op.on.is_empty()
            || !op.into.is_empty()
        {
            return Err(RpcErr::invalid(
                "job accepts positionals, --json, and its documented --arg options only",
            ));
        }
        let expected = if matches!(verb, "item" | "retry") {
            3
        } else {
            2
        };
        if op.positionals.len() != expected {
            return Err(RpcErr::invalid(
                "use job create FILE | work NAME | status NAME | item NAME ITEM | retry NAME ITEM",
            ));
        }
        let target = &op.positionals[1];
        let mut params = json!({"job":target});
        let method = match verb {
            "create" => {
                let file =
                    std::fs::File::open(target).map_err(|e| RpcErr::invalid(e.to_string()))?;
                use std::io::Read;
                let mut bytes = Vec::new();
                file.take(8 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| RpcErr::invalid(e.to_string()))?;
                if bytes.len() > 8 * 1024 * 1024 {
                    return Err(RpcErr::invalid("job manifest exceeds 8 MiB"));
                }
                params = serde_json::from_slice(&bytes)
                    .map_err(|e| RpcErr::invalid(format!("job manifest must be JSON: {e}")))?;
                "job_create"
            }
            "work" => {
                let count = op
                    .args
                    .get("count")
                    .map(|v| v.parse::<u64>())
                    .transpose()
                    .map_err(|_| RpcErr::invalid("count must be an integer"))?
                    .unwrap_or(1);
                let lease =
                    util::parse_duration(op.args.get("lease").map(String::as_str).unwrap_or("5m"))
                        .map_err(RpcErr::invalid)?;
                params["count"] = json!(count);
                params["lease_ms"] = json!(lease);
                "job_work"
            }
            "status" => {
                let offset = op
                    .args
                    .get("offset")
                    .map(|v| v.parse::<u64>())
                    .transpose()
                    .map_err(|_| RpcErr::invalid("offset must be an integer"))?
                    .unwrap_or(0);
                params["offset"] = json!(offset);
                "job_status"
            }
            "item" | "retry" => {
                params["item"] = json!(op.positionals[2]);
                if verb == "item" {
                    "job_item"
                } else {
                    "job_retry"
                }
            }
            _ => return Err(RpcErr::invalid("unknown job operation")),
        };
        client.call(method, params)
    })();
    finish(result, op.json, |v| {
        println!("{}", serde_json::to_string_pretty(v).unwrap())
    })
}

pub(crate) fn pact_command(op: &Op, client: &mut Client) -> i32 {
    let p = &op.positionals;
    let arg = |i: usize| p.get(i).cloned().unwrap_or_default();
    let call_no = |i: usize| p.get(i).and_then(|c| c.parse::<i64>().ok());
    let abs = |f: &str| std::fs::canonicalize(f).map(|x| x.display().to_string()).unwrap_or_else(|_| f.to_string());
    let usage = "use: pact list | offer PEER FILE [as NAME] | fetch PEER | show P | agree P | decline P [because T] | call P given k=v... [wait] | status P CALL | accept P CALL | reject P CALL [because T] | history P | export P FILE | admit NAME key K | unadmit NAME | limit P k=v... | end P [because T] | poll";
    let res = match (p.first().map(String::as_str), p.len()) {
        (None | Some("list"), 0 | 1) => client.call("pact_list", json!({})),
        (Some("offer"), 3) => client.call("pact_offer", json!({"peer": arg(1), "file": abs(&arg(2)), "name": op.name})),
        (Some("fetch"), 2) => client.call("pact_fetch", json!({"peer": arg(1)})),
        (Some("show"), 2) => client.call("pact_show", json!({"pact": arg(1)})),
        (Some("agree"), 2) => client.call("pact_agree", json!({"pact": arg(1)})),
        (Some("decline"), 2) => client.call("pact_decline", json!({"pact": arg(1), "reason": op.reason})),
        (Some("call"), 2) | (Some("call"), 3) if p.get(2).is_none_or(|w| w == "wait") => {
            let wait = op.wait || p.get(2).is_some_and(|w| w == "wait");
            client.call("pact_call", json!({"pact": arg(1), "args": op.args})).and_then(|started| {
                if wait {
                    client.call("pact_call_status", json!({"pact": arg(1), "call": started["call"], "wait_ms": 300_000}))
                } else {
                    Ok(started)
                }
            })
        }
        (Some("status"), 3) | (Some("status"), 4) if p.get(3).is_none_or(|w| w == "wait") => {
            let wait = op.wait || p.get(3).is_some_and(|w| w == "wait");
            client.call("pact_call_status", json!({"pact": arg(1), "call": call_no(2), "wait_ms": if wait { Some(300_000) } else { None }}))
        }
        (Some("accept"), 3) => client.call("pact_accept", json!({"pact": arg(1), "call": call_no(2)})),
        (Some("reject"), 3) => client.call("pact_reject", json!({"pact": arg(1), "call": call_no(2), "reason": op.reason})),
        (Some("history"), 2) => client.call("pact_history", json!({"pact": arg(1)})),
        (Some("export"), 3) => client.call("pact_export", json!({"pact": arg(1), "file": abs(&arg(2))})),
        (Some("admit"), 2) => match &op.key {
            Some(k) => client.call("pact_admit", json!({"name": arg(1), "key": k})),
            None => Err(RpcErr::invalid("give the counterparty core's key: pact admit NAME key ed25519:…")),
        },
        (Some("unadmit"), 2) => client.call("pact_unadmit", json!({"name": arg(1)})),
        (Some("limit"), n) if n >= 3 => {
            let mut limits = serde_json::Map::new();
            let mut bad = None;
            for kv in &p[2..] {
                match kv.split_once('=') {
                    Some(("time", v)) => {
                        limits.insert("time".into(), json!(v));
                    }
                    Some((k, v)) => match v.parse::<u64>() {
                        Ok(n) => {
                            limits.insert(k.into(), json!(n));
                        }
                        Err(_) => bad = Some(format!("`{kv}`: limits are whole numbers, or a duration for time")),
                    },
                    None => bad = Some(format!("`{kv}`: write limits as name=value, such as tool_calls=3")),
                }
            }
            match bad {
                Some(b) => Err(RpcErr::invalid(b)),
                None => client.call("pact_limit", json!({"pact": arg(1), "limits": limits})),
            }
        }
        (Some("end"), 2) => client.call("pact_end", json!({"pact": arg(1), "reason": op.reason})),
        (Some("poll"), 1) => client.call("pact_poll", json!({})),
        _ => Err(RpcErr::invalid(usage)),
    };
    let verb = p.first().cloned().unwrap_or_else(|| "list".into());
    finish(res, op.json, |d| match verb.as_str() {
        "list" => {
            let list = d.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                println!("{}", paint(Ink::Dim, "No pacts. Offer one: summonr pact offer PEER terms.yaml"));
            }
            for x in list {
                println!("  {} {} with {} — {}{}", bold(Ink::Bone, s(&x, "name")), paint(Ink::Dim, s(&x, "side")), s(&x, "peer"), s(&x, "state"),
                    if x["calls_open"].as_i64().unwrap_or(0) > 0 { format!(", {} call(s) open", x["calls_open"]) } else { String::new() });
            }
        }
        "show" => {
            rubric(&format!("PACT · {} · {}", s(d, "name"), s(d, "state")));
            row("with", &format!("{} (this core is the {})", s(d, "peer"), s(d, "side")));
            row("id", s(d, "id"));
            row("ritual", &format!("{} {}", s(&d["terms"]["ritual"], "name"), s(&d["terms"]["ritual"], "hash")));
            row("accept", s(&d["terms"], "accept"));
            row("entries", &d["entries"].to_string());
            for e in d["ritual"]["effects"].as_array().into_iter().flatten() {
                println!("    {} {} ({})", s(e, "step"), s(e, "what"), s(e, "effect"));
            }
            for c in d["calls"].as_array().into_iter().flatten() {
                println!("  call {} — {}", c["call"], s(c, "state"));
            }
            for w in d["warnings"].as_array().into_iter().flatten() {
                println!("  {} {}", paint(Ink::Gold, "!"), w.as_str().unwrap_or(""));
            }
        }
        "history" => {
            for e in d["entries"].as_array().into_iter().flatten() {
                let mark = if e["verified"] == true { paint(Ink::Verdigris, "✓") } else { paint(Ink::Rubric, "✕") };
                let call = e["call"].as_i64().map(|c| format!(" call {c}")).unwrap_or_default();
                println!("  {mark} #{} {}{} by {}", e["n"], s(e, "kind"), call, s(e, "by"));
            }
            for f in d["forks"].as_array().into_iter().flatten() {
                println!("  {} fork seen {}", paint(Ink::Rubric, "✕"), fmt_ago(f["seen"].as_i64().unwrap_or(0)));
            }
        }
        _ => println!("{}", serde_json::to_string_pretty(d).unwrap_or_default()),
    })
}


pub(crate) fn demo_command(op: &Op) -> i32 {
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let res = (|| -> Result<Value, RpcErr> {
        let root = std::env::temp_dir().join(format!(
            "summonr-demo-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _cleanup = Cleanup(root.clone());
        let home = root.join("home");
        let repo = root.join("repo");
        let origin = root.join("origin.git");
        let worktree = root.join("feat-demo");
        let paths = Paths {
            runtime: home.join("run"),
            state: home.join("state"),
            config: home.join("config"),
        };
        std::fs::create_dir_all(&paths.config)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        std::fs::create_dir_all(&paths.state)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        std::fs::create_dir_all(&paths.runtime)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;

        let git = |dir: &std::path::Path, args: &[&str]| -> Result<String, RpcErr> {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=Summonr Demo", "-c", "user.email=demo@summonr.local"])
                .args(args)
                .current_dir(dir)
                .output()
                .map_err(|e| RpcErr::invalid(format!("git failed: {e}")))?;
            if !out.status.success() {
                return Err(RpcErr::invalid(format!(
                    "git {} failed: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        };

        std::fs::create_dir_all(&origin)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        git(&origin, &["init", "--bare", "-b", "main"])?;
        std::fs::create_dir_all(&repo)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        git(&repo, &["init", "-b", "main"])?;
        git(&repo, &["remote", "add", "origin", &origin.to_string_lossy()])?;
        std::fs::write(repo.join("README.md"), "# Playground Repo\n")
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        git(&repo, &["add", "README.md"])?;
        git(&repo, &["commit", "-m", "initial commit"])?;
        git(&repo, &["push", "-u", "origin", "main"])?;
        git(
            &repo,
            &["worktree", "add", "-b", "feat/demo", &worktree.to_string_lossy()],
        )?;
        std::fs::write(worktree.join("hello.txt"), "hello from summonr demo\n")
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;

        let cfg = serde_yaml::to_string(&json!({
            "repos": {
                "playground": {
                    "path": repo.to_string_lossy(),
                    "trunk": "main",
                }
            }
        }))
        .unwrap();
        std::fs::write(paths.config.join("config.yaml"), cfg)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;

        let core = core::Core::open(paths)
            .map_err(|e| RpcErr::new("storage_failed", 11, e))?;

        let model = op.model.clone().unwrap_or_else(|| "mock/keyword".to_string());
        let ritual_path = root.join("demo-commit-work.yaml");
        let ritual_yaml = format!(
            r#"ritual: demo-commit-work
description: Inspect a feature worktree, draft a commit message, and hold git.commit for human approval.
model: {model}
inputs:
  repo: string
  branch: string
reagents:
  git: {{select: [worktree_for, status, commit]}}
requires:
  - {{reagent: git, action: worktree_for}}
  - {{reagent: git, action: status}}
  - {{reagent: git, action: commit}}
essence: {{requests: 1, tokens: 400, tool_calls: 10, time: 30s}}
steps:
  - id: tree
    operation: git.worktree_for
    arguments: {{repo: "${{inputs.repo}}", branch: "${{inputs.branch}}"}}
  - id: state
    operation: git.status
    arguments: {{repo: "${{inputs.repo}}", worktree: "${{steps.tree.output.path}}"}}
  - id: draft
    model:
      instructions: Draft a concise one-line commit message for these repository changes.
      prompt: "${{steps.state.output}}"
      schema:
        message: string
  - id: commit
    operation: git.commit
    hold: true
    arguments: {{repo: "${{inputs.repo}}", branch: "${{inputs.branch}}", message: "${{steps.draft.output.message}}"}}
return:
  commit: "${{steps.commit.output}}"
  message: "${{steps.draft.output.message}}"
"#
        );
        std::fs::write(&ritual_path, ritual_yaml)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        core.dispatch("inscribe", json!({"path": ritual_path.to_string_lossy()}))?;
        core.dispatch(
            "grant",
            json!({
                "ritual": "demo-commit-work",
                "scopes": [{"reagent": "git", "select": ["worktree_for", "status", "commit"]}],
            }),
        )?;

        let inv = core.dispatch(
            "invoke",
            json!({
                "target": "demo-commit-work",
                "model": model,
                "args": {"repo": "playground", "branch": "feat/demo"},
            }),
        )?;
        let summon_id = s(&inv, "summon").to_string();
        let ended = core.dispatch("wait", json!({"summon": summon_id, "timeout_ms": 10000}))?;
        let pending = core.dispatch("petitions", json!({"status": "pending", "summon": summon_id}))?;
        let petition = pending
            .as_array()
            .and_then(|a| a.first())
            .cloned()
            .ok_or_else(|| RpcErr::invalid("demo did not produce a held git.commit petition"))?;
        let pid = s(&petition, "id").to_string();

        let decided = core.dispatch(
            "petition_decide",
            json!({"ids": [pid], "permit": true}),
        )?;
        let log = git(&worktree, &["log", "-1", "--oneline"])?;
        let verified = core.dispatch("chain_verify", json!({}))?;
        drop(core);

        Ok(json!({
            "ephemeral_root": root.to_string_lossy(),
            "cleaned_up": true,
            "model": model,
            "summon": summon_id,
            "outcome": ended["outcome"],
            "held_petition": {
                "id": pid,
                "operation": petition["operation"],
                "digest": petition["digest"],
                "arguments": petition["arguments"],
            },
            "after_permit": {
                "status": decided["status"],
                "git_head": log,
            },
            "chain_verified": verified["ok"],
        }))
    })();

    finish(res, op.json, |d| {
        rubric("EPHEMERAL GIT PLAYGROUND");
        row("model", s(d, "model"));
        row("summon", &format!("{} ({})", s(d, "summon"), s(d, "outcome")));
        row("held petition", &format!("{} · {}", s(&d["held_petition"], "id"), s(&d["held_petition"], "operation")));
        row("after permit", s(&d["after_permit"], "git_head"));
        row("epitaph chain", &format!("verified = {}", d["chain_verified"]));
        row("cleanup", "disposable /tmp repo + SUMMONR_HOME removed");
    })
}
