//! Capability-sandboxed WebAssembly (WASI) reagent host.
//!
//! Each configured `wasm_reagents.<name>` entry pins a `.wasm` binary by its
//! SHA-256 hash (`sha256`), validates the WebAssembly binary magic header
//! (`\0asm\x01\0\0\0`) before every invocation, clears the child environment
//! completely (`env_clear()`), runs in its own process group with a hard
//! wall-clock timeout and bounded stdout cap, and exchanges JSON over WASI
//! stdin/stdout (`{"tool": "...", "arguments": {...}}`).

use crate::util::{sha256_hex, WasmReagentConfig};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WASM_MAGIC_V1: &[u8; 8] = b"\0asm\x01\0\0\0";

/// Verify that `cfg.module` exists, begins with the WebAssembly v1 magic header,
/// and matches its pinned `cfg.sha256` digest.
pub(crate) fn verify_module(cfg: &WasmReagentConfig) -> Result<(), String> {
    let bytes = std::fs::read(&cfg.module)
        .map_err(|e| format!("cannot read wasm module `{}`: {e}", cfg.module))?;
    if bytes.len() < WASM_MAGIC_V1.len() || &bytes[..8] != WASM_MAGIC_V1 {
        return Err(format!(
            "module `{}` is not a valid WebAssembly binary (missing \\0asm v1 header)",
            cfg.module
        ));
    }
    let expected = cfg.sha256.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("wasm_reagent `sha256` must be a 64-character lowercase hex digest".into());
    }
    let actual = sha256_hex(&bytes);
    if actual != expected {
        return Err(format!(
            "wasm module `{}` SHA-256 mismatch: pinned {expected}, found {actual}",
            cfg.module
        ));
    }
    Ok(())
}

/// Invoke one tool inside a pinned, capability-sandboxed WASI module.
pub(crate) fn call_tool(
    cfg: &WasmReagentConfig,
    tool: &str,
    args: &Value,
    deadline_remaining: Option<Duration>,
) -> Result<Value, String> {
    verify_module(cfg)?;

    let cfg_timeout = Duration::from_millis(cfg.timeout_ms.unwrap_or(10_000).max(100));
    let timeout = match deadline_remaining {
        Some(rem) => rem.min(cfg_timeout),
        None => cfg_timeout,
    };
    let max_out = cfg.max_output_bytes.unwrap_or(262_144).max(1024);

    let runtime = cfg
        .runtime
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("wasmtime");

    let mut cmd = Command::new(runtime);
    // Completely scrub host environment so API keys and passphrases never leak.
    cmd.env_clear();
    if runtime.ends_with("wasmtime") {
        cmd.arg("run");
        for dir in &cfg.preopen_dirs {
            if dir.starts_with('-') {
                return Err(format!("invalid wasm preopen_dirs entry `{dir}`: must not start with `-`"));
            }
            cmd.arg("--dir").arg(dir);
        }
        if let Some(fuel) = cfg.fuel {
            cmd.arg("-W").arg("consume-fuel=y").arg("--fuel").arg(fuel.to_string());
        }
        if let Some(mb) = cfg.max_memory_mb {
            let bytes = mb.saturating_mul(1024 * 1024);
            cmd.arg("-W").arg(format!("memory-reservation={bytes}"));
        }
        cmd.arg("--").arg(&cfg.module);
    } else {
        // Custom WASI runner receives `<module>` as its sole positional argument.
        cmd.arg(&cfg.module);
    }

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Place the WASI runner in its own process group so timeout kills the whole tree.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot spawn wasm runtime `{runtime}`: {e}"))?;

    let req_payload = json!({"tool": tool, "arguments": args}).to_string();
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(req_payload.as_bytes()) {
            // BrokenPipe means the child exited before reading stdin; let wait()
            // collect its exit status and stderr rather than leaking a zombie.
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                let pid = child.id() as libc::pid_t;
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
                let _ = child.wait();
                return Err(format!("failed writing stdin to wasm reagent: {e}"));
            }
        }
    }

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    // stderr gets the same cap as stdout, never more than 16 KiB. A stream
    // that passes its cap raises `overflow`, and the wait loop below kills
    // the runner at once instead of leaving it blocked on a full pipe.
    let max_err = max_out.min(16_384);
    let overflow = Arc::new(AtomicBool::new(false));
    let out_handle = {
        let overflow = overflow.clone();
        std::thread::spawn(move || read_bounded(&mut stdout, max_out, "stdout", &overflow))
    };
    let err_handle = {
        let overflow = overflow.clone();
        std::thread::spawn(move || read_bounded(&mut stderr, max_err, "stderr", &overflow))
    };

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let pid = child.id() as libc::pid_t;
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                    let _ = child.wait();
                    let _ = out_handle.join();
                    let _ = err_handle.join();
                    return Err(format!(
                        "wasm reagent timed out after {}ms (process group killed)",
                        timeout.as_millis()
                    ));
                }
                if overflow.load(Ordering::SeqCst) {
                    let pid = child.id() as libc::pid_t;
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                    let _ = child.wait();
                    let out = out_handle.join().map_err(|_| "stdout reader panicked".to_string())?;
                    let err = err_handle.join().map_err(|_| "stderr reader panicked".to_string())?;
                    out?;
                    err?;
                    return Err("wasm reagent output exceeded its cap (process group killed)".into());
                }
                if crate::cancel::requested() {
                    let pid = child.id() as libc::pid_t;
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                    let _ = child.wait();
                    let _ = out_handle.join();
                    let _ = err_handle.join();
                    return Err("wasm reagent stopped: the run was cancelled (process group killed)".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(format!("wait failed on wasm reagent: {e}")),
        }
    };

    let out_bytes = out_handle
        .join()
        .map_err(|_| "stdout reader panicked".to_string())??;
    let err_bytes = err_handle
        .join()
        .map_err(|_| "stderr reader panicked".to_string())??;

    if !status.success() {
        let err_s = String::from_utf8_lossy(&err_bytes);
        return Err(format!(
            "wasm reagent exited with {}: {}",
            status,
            err_s.trim()
        ));
    }

    let out_str = String::from_utf8(out_bytes)
        .map_err(|_| "wasm reagent stdout was not valid UTF-8".to_string())?;
    serde_json::from_str::<Value>(out_str.trim())
        .map_err(|e| format!("wasm reagent stdout was not valid JSON: {e}"))
}

/// Read `r` to its end, refusing more than `limit` bytes. Going over sets
/// `overflow` so the caller can stop the writer.
fn read_bounded(r: &mut impl Read, limit: usize, stream: &str, overflow: &AtomicBool) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = r.read(&mut chunk).map_err(|e| format!("reading wasm reagent {stream}: {e}"))?;
        if n == 0 {
            return Ok(buf);
        }
        if buf.len().saturating_add(n) > limit {
            overflow.store(true, Ordering::SeqCst);
            return Err(format!("wasm reagent {stream} exceeded {limit} bytes"));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{new_id, WasmToolConfig};
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;

    fn make_wasm_cfg(
        module_path: &std::path::Path,
        sha256: String,
        runtime: String,
    ) -> WasmReagentConfig {
        let mut tools = BTreeMap::new();
        tools.insert(
            "calc".into(),
            WasmToolConfig {
                effect: "pure".into(),
                description: "test calc".into(),
            },
        );
        WasmReagentConfig {
            module: module_path.display().to_string(),
            sha256,
            runtime: Some(runtime),
            tools,
            preopen_dirs: vec![],
            max_memory_mb: Some(64),
            fuel: Some(100_000),
            timeout_ms: Some(3_000),
            max_output_bytes: Some(65_536),
        }
    }

    #[test]
    fn rejects_invalid_magic_and_sha256_mismatch() {
        let dir = std::env::temp_dir().join(format!("summonr-wasm-test-{}", new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();

        let bad_magic = dir.join("not.wasm");
        std::fs::write(&bad_magic, b"not-wasm-bytes").unwrap();
        let cfg_bad = make_wasm_cfg(&bad_magic, sha256_hex(b"not-wasm-bytes"), "true".into());
        assert!(verify_module(&cfg_bad).unwrap_err().contains("not a valid WebAssembly"));

        let good_wasm = dir.join("ok.wasm");
        std::fs::write(&good_wasm, WASM_MAGIC_V1).unwrap();
        let cfg_wrong_hash = make_wasm_cfg(&good_wasm, "0".repeat(64), "true".into());
        assert!(verify_module(&cfg_wrong_hash).unwrap_err().contains("SHA-256 mismatch"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verified_module_runs_with_scrubbed_env_and_json_stdio() {
        let dir = std::env::temp_dir().join(format!("summonr-wasm-run-{}", new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();

        let wasm_file = dir.join("calc.wasm");
        std::fs::write(&wasm_file, WASM_MAGIC_V1).unwrap();
        let digest = sha256_hex(WASM_MAGIC_V1);

        // Stub WASI runner script that echoes the request and verifies OPENAI_API_KEY is absent.
        let runner = dir.join("fake-wasi.sh");
        std::fs::write(
            &runner,
            "#!/bin/sh\nif [ -n \"$OPENAI_API_KEY\" ]; then echo 'leaked env' >&2; exit 2; fi\ncat\n",
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).unwrap();

        let cfg = make_wasm_cfg(&wasm_file, digest, runner.display().to_string());
        let out = call_tool(&cfg, "calc", &json!({"x": 42}), None).unwrap();
        assert_eq!(out["tool"], "calc");
        assert_eq!(out["arguments"]["x"], 42);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_dash_preopen_dir_and_handles_early_exit_cleanly() {
        let dir = std::env::temp_dir().join(format!("summonr-wasm-hard-{}", new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();

        let wasm_file = dir.join("calc.wasm");
        std::fs::write(&wasm_file, WASM_MAGIC_V1).unwrap();
        let digest = sha256_hex(WASM_MAGIC_V1);

        // Option-injection check on preopen_dirs when runtime is wasmtime.
        let mut cfg_bad_dir = make_wasm_cfg(&wasm_file, digest.clone(), "wasmtime".into());
        cfg_bad_dir.preopen_dirs = vec!["--allow-precompiled".into()];
        let err = call_tool(&cfg_bad_dir, "calc", &json!({}), None).unwrap_err();
        assert!(err.contains("must not start with `-`"), "unexpected err: {err}");

        // Early-exiting runner (closes stdin immediately without reading): must report
        // exit status & stderr cleanly without leaking a zombie or failing on BrokenPipe.
        let early = dir.join("early-exit.sh");
        std::fs::write(&early, "#!/bin/sh\necho 'boom' >&2\nexit 7\n").unwrap();
        std::fs::set_permissions(&early, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg_early = make_wasm_cfg(&wasm_file, digest, early.display().to_string());
        let err2 = call_tool(&cfg_early, "calc", &json!({"big": "x".repeat(4096)}), None).unwrap_err();
        assert!(err2.contains("boom"), "expected stderr message, got: {err2}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn too_much_stderr_is_an_error_not_a_silent_success() {
        let dir = std::env::temp_dir().join(format!("summonr-wasm-stderr-{}", new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let wasm_file = dir.join("calc.wasm");
        std::fs::write(&wasm_file, WASM_MAGIC_V1).unwrap();
        let digest = sha256_hex(WASM_MAGIC_V1);

        // Writes 20,000 bytes to stderr, then a valid answer, and exits 0.
        let noisy = dir.join("noisy.sh");
        std::fs::write(&noisy, "#!/bin/sh\ncat >/dev/null\nhead -c 20000 /dev/zero | tr '\\0' x >&2\necho '{\"ok\":true}'\n").unwrap();
        std::fs::set_permissions(&noisy, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = make_wasm_cfg(&wasm_file, digest.clone(), noisy.display().to_string());
        cfg.max_output_bytes = Some(1024);
        let err = call_tool(&cfg, "calc", &json!({}), None).unwrap_err();
        assert!(err.contains("stderr") && err.contains("1024"), "unexpected: {err}");

        // A runner that never stops writing to stderr is stopped as soon as it
        // passes the cap, not left blocked on a full pipe until the timeout.
        let flood = dir.join("flood.sh");
        std::fs::write(&flood, "#!/bin/sh\nwhile :; do echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx >&2; done\n").unwrap();
        std::fs::set_permissions(&flood, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = make_wasm_cfg(&wasm_file, digest.clone(), flood.display().to_string());
        cfg.max_output_bytes = Some(1024);
        let started = Instant::now();
        let err = call_tool(&cfg, "calc", &json!({}), None).unwrap_err();
        assert!(err.contains("stderr") && !err.contains("timed out"), "unexpected: {err}");
        assert!(started.elapsed() < Duration::from_millis(2_000), "took {:?}", started.elapsed());

        // The same holds for stdout.
        let big = dir.join("big.sh");
        std::fs::write(&big, "#!/bin/sh\nwhile :; do echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done\n").unwrap();
        std::fs::set_permissions(&big, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = make_wasm_cfg(&wasm_file, digest, big.display().to_string());
        cfg.max_output_bytes = Some(1024);
        let err = call_tool(&cfg, "calc", &json!({}), None).unwrap_err();
        assert!(err.contains("stdout") && !err.contains("timed out"), "unexpected: {err}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
