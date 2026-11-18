# Scoped HTTP operations

`http.get_text` and `http.get_json` perform one HTTP(S) GET (`http.get_json` also parses strict JSON and optionally validates `schema`, returning `value` instead of `text`). Authenticated write operations (`http.post`, `http.put`, `http.patch`, `http.delete`) use owner-configured `http_auth` profiles in `config.yaml` and support `hold` with ETag/body-digest verification. Declare the tool under `reagents` and `requires`, then grant the exact canonical origin:

```bash
summonr allow read-page --from http --select get_text --on origin=https://example.com
```

An origin is `scheme://host[:port]`, with no trailing slash, path, query,
credentials, or fragment. Scheme and port are part of the permission. An
unscoped HTTP grant is refused. The grant permits all paths and queries on that
origin; it is not a path allowlist or an IP-address restriction. Grant only hosts
you intend to access, including any private addresses to which they resolve.

| Argument | Meaning |
| --- | --- |
| `url` | Full HTTP(S) URL, at most 8,192 characters; credentials and fragments refused |
| `max_bytes` | Maximum decoded body bytes; default and maximum 1,048,576 |
| `timeout_ms` | Default 5,000; 1–30,000; shortened to the earliest ancestor deadline |

The result contains `url`, `status`, `content_type`, `text`, `bytes`, and `sha256`
of the returned body bytes. Only 2xx responses succeed. Redirects are never
followed, even within the granted origin. Oversized responses fail with exhaustion
instead of returning partial text. Non-UTF-8 bodies fail; no HTML parsing or
character-set conversion is attempted. HTTP transport errors, including socket
timeouts, produce failed outcomes; explicit deadline checks produce exhaustion.

The client sends no user-supplied headers, authentication, cookies, or request
body. Ambient proxy configuration is disabled. HTTPS uses the client's normal
certificate verification. There are no retries. DNS resolution and OS scheduling
can outlast a requested timeout; this is not an exact wall-clock guarantee or a
network sandbox. HTTP GET can still trigger behavior on a poorly designed server.

```yaml
language_version: 1
ritual: read-page
reagents:
  http: {select: [get_text]}
requires:
  - reagent: http
    action: get_text
    on: {origin: https://example.com}
essence: {requests: 0, tool_calls: 1, time: 10s}
steps:
  - id: page
    operation: http.get_text
    arguments: {url: "https://example.com/", max_bytes: 65536, timeout_ms: 5000}
return:
  page: "${steps.page.output}"
```

The URL and text are recorded as evidence. Query parameters must not contain
secrets you do not want recorded. Returned content remains untrusted; fetching it
does not authorize any instructions it contains. The integration tests use only
a loopback fake server and verify grant refusal, redirects, size and timeout
failures. They do not establish compatibility with every public service.
