# MCP-backed stdio reagents

Beyond its built-in reagents (`data`, `file`, `http`, `mail`, `git`, `gh`, `trial`), Summonr can call owner-configured external MCP servers over stdio as named reagents.

## Configuration

Declare external MCP servers in `config.yaml` under `mcp_reagents`:

```yaml
mcp_reagents:
  notes:
    command: ["python3", "/path/to/notes_mcp.py"]
    cwd: "/path/to/workdir"          # optional
    env:                             # optional
      NOTES_DIR: "/home/user/notes"
    tools:
      list_notes:
        effect: read
        description: "List note titles"
      save_note:
        effect: write                # default if `effect` is omitted
        description: "Write a note"
```

## Security and authority rules

- **No shell execution:** `command` is spawned directly (`exec` with argv), never via `sh -c`.
- **Fail-closed effect classification:** Summonr never trusts an external server to self-declare whether a tool is read-only. Each allowed tool must be listed under `tools:`; if `effect` is omitted, it defaults to `write`. Undeclared tools are rejected at both validation and runtime.
- **Explicit grants:** Like any non-pure reagent, rituals must declare `requires: [{reagent: notes, action: ...}]` and be granted permission with `summonr grant <ritual> --from notes --select <tool>`. MCP grants cannot be scoped with `--on`/`--into`: nothing maps a scope to the tool's arguments, so such a grant is refused, and a scoped grant (or a scoped `requires` entry) authorizes nothing.
- **Dry runs stay dry:** under `--simulate`, drills, assays and replays, every MCP tool not declared `effect: read` is simulated, never sent to the server.
- **Per-call isolation:** Each invocation spawns the configured process, runs the JSON-RPC 2.0 `initialize` / `notifications/initialized` handshake, calls `tools/call`, extracts `structuredContent` (or parses JSON / text from `content[0].text`), and terminates the child process cleanly.
