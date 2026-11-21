# Configuration and files

## Where things live

With `SUMMONR_HOME` set, everything is under that directory:

| Path | Holds |
| --- | --- |
| `$SUMMONR_HOME/config/config.yaml` | Configuration |
| `$SUMMONR_HOME/config/mcp_token` | Secret for the MCP HTTP URL |
| `$SUMMONR_HOME/config/root.pub` | Optional Ed25519 root public key for verifying offline-signed familiar grants (`familiar register-grant`) |
| `$SUMMONR_HOME/state/summonr.db` | SQLite: rituals, grants, summons, events, petitions, baselines |
| `$SUMMONR_HOME/state/core.log` | Core log |
| `$SUMMONR_HOME/state/core.lock` | Exclusive lifetime ownership lock; never delete to bypass a live core |
| `$SUMMONR_HOME/run/core.sock` | The core's socket |

If that socket path would be longer than a Unix socket allows (about 100 bytes), the socket moves to `$XDG_RUNTIME_DIR/summonr-<hash>/`, named from the home, so two homes never share a core.

Without `SUMMONR_HOME`, the XDG locations apply:

| What | Where |
| --- | --- |
| Config | `$XDG_CONFIG_HOME/summonr` (default `~/.config/summonr`) |
| State | `$XDG_STATE_HOME/summonr` (default `~/.local/state/summonr`) |
| Socket | `$XDG_RUNTIME_DIR/summonr/core.sock`, or under the state directory if that is unset |

There is no shared `/tmp` fallback for the socket.

## `config.yaml`

```yaml
ollama_url: "http://127.0.0.1:11434"
mail_root: "/home/me/summonr/mailbox"
max_depth: 3
max_active_runs: 16
style: keyword
```

| Key | Default | Meaning |
| --- | --- | --- |
| `ollama_url` | see below | Ollama server. `http://` is added if no scheme is given. |
| `openai_url` | `https://api.openai.com` | OpenAI-compatible endpoint for `openai/<model>` (or `SUMMONR_OPENAI_URL`) |
| `anthropic_url` | `https://api.anthropic.com` | Anthropic Messages endpoint for `anthropic/<model>` (or `SUMMONR_ANTHROPIC_URL`) |
| `providers` | none | Named model providers (`NAME: {kind, url, api_key_env}`) for `<provider>/<model>` |
| `mail_root` | none | Mailbox root for the `mail` reagent. Required to use it. |
| `max_active_runs` | `16` | Maximum admitted active top-level invocations; excess requests receive `busy` |
| `repos` | none | Repositories the `git`, `gh` and `trial` reagents may act on: `NAME: {path, trunk, remote, test, disposable}`. See [Repositories](../guides/repositories.md). |
| `commands` | none | Scoped commands the `cmd` reagent may execute, by name |
| `mcp_reagents` | none | External stdio MCP servers configured as reagents, by name |
| `git`, `gh` | on PATH | The `git` and `gh` programs those reagents run |
| `openssl` | on PATH | The `openssl` program used for RFC 3161 timestamps |
| `max_depth` | `3` | How deeply rituals may call rituals |
| `style` | `keyword` | `keyword` or `flag`: the spelling used in help and suggestions |
| `egress` | none | HTTP egress policy (`allow_hosts`, `deny_hosts`, `block_private_ips`) |
| `http_auth` | none | Named authenticated HTTP credential bindings (`NAME: {origin, header, prefix, token_env}`) |
| `petition_ttl` | none | Optional default TTL for pending petitions (e.g. `"24h"`) |
| `tls` | none | Optional PEM `cert` and `key` paths for TLS on `summonr listen` and `summonr mcp http` |
| `mcp_auth` | none | Optional OAuth 2.1 Bearer JWT verification settings for `summonr mcp http` |
| `mcp_allowed_hosts` | `[]` | Additional allowed `Host` header values (host or `host:port`) for `summonr mcp http` |

### `egress`

```yaml
egress:
  allow_hosts: ["api.example.com"]
  deny_hosts: ["internal.example.com"]
  block_private_ips: true
```

- `allow_hosts`: optional list of permitted hostnames; when set, hosts not matching are refused.
- `deny_hosts`: optional list of forbidden hostnames; always refused, even if matched by `allow_hosts`.
- `block_private_ips`: when `true`, refuses loopback, private, and link-local addresses.

Unknown keys and unreadable configuration files are errors. A missing or empty file means all defaults.

The Ollama URL is taken from the first of: `ollama_url`, `SUMMONR_OLLAMA_URL`, `OLLAMA_HOST`, `http://127.0.0.1:11434`.

## Environment variables

| Variable | Effect |
| --- | --- |
| `SUMMONR_HOME` | Put config, state and socket under one directory |
| `SUMMONR_OLLAMA_URL`, `OLLAMA_HOST` | Ollama URL, when the config does not set one |
| `SUMMONR_OPENAI_URL`, `OPENAI_API_KEY` | OpenAI-compatible endpoint URL and API key |
| `SUMMONR_ANTHROPIC_URL`, `ANTHROPIC_API_KEY` | Anthropic endpoint URL and API key |
| `NO_COLOR`, `SUMMONR_PLAIN` | No colour or ornament (also implies ASCII marks) |
| `SUMMONR_ASCII` | Colour, but ASCII marks instead of other glyphs |
| `SUMMONR_KEY_PASSPHRASE` | Passphrase for encrypted Ed25519 key files |

## MCP HTTP OAuth 2.1 (`mcp_auth`)

To require OAuth 2.1 Bearer JWT authentication on `summonr mcp http` (and serve RFC 9728 Protected Resource Metadata at `/.well-known/oauth-protected-resource`), add `mcp_auth:` to `config.yaml`:

```yaml
mcp_auth:
  issuer: "https://idp.example.com"
  audience: "https://summonr.internal/mcp"
  authorization_servers:
    - "https://idp.example.com"
  required_scopes:
    - "summonr:run"
  public_key: "ed25519:..."       # EdDSA public key (or hs256_secret_env: MY_SECRET_ENV)
  clock_skew_secs: 60
```

Requests to `/mcp` or `/mcp/<token>` without a valid `Authorization: Bearer <jwt>` matching `issuer`, `audience`, `exp`, and `required_scopes` are refused with `401 Unauthorized` (`403 Forbidden` for missing scopes) and a `WWW-Authenticate` challenge pointing to `/.well-known/oauth-protected-resource`.
