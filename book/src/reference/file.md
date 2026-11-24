# Scoped file operations

Select `file` operations under `reagents`, and declare each under `requires` with an
explicit absolute `on.root`. The owner grants the same root:

```bash
summonr allow read-document --from file --select read_text --on root=/absolute/documents
```

| Operation | Arguments | Output |
| --- | --- | --- |
| `file.read_text` | `root`, relative `path`, optional `max_bytes` (default and maximum 1,048,576) | `root`, `path`, UTF-8 `text`, byte count `bytes`, `sha256` of the bytes read |
| `file.read_json` | `root`, relative `path`, optional `max_bytes`, optional `schema` | `root`, `path`, parsed `value`, byte count `bytes`, `sha256` |
| `file.stat` | `root`, relative `path` | `root`, `path`, `exists`, `kind` (`file`, `directory`, `other`, or `null`), `bytes`, `sha256` |
| `file.list` | `root`, relative `path` (empty for root), optional `max_entries` (default 1,000; maximum 10,000) | `root`, `path`, `entries`, sorted by name; each has `name` and `kind` (`file`, `directory`, `symlink`, `other`) |
| `file.write_text` | `root`, relative `path`, UTF-8 `text` (at most 1 MiB) | `root`, `path`, `bytes`, `sha256`; supports `hold` with TOCTOU target digest verification |
| `file.append_jsonl` | `root`, relative `path`, JSON `record` | `root`, `path`, `bytes_appended`, `record_sha256`, file `sha256`; supports `hold` with TOCTOU target digest verification |
| `file.delete` | `root`, relative `path` | `root`, `path`, `deleted: true`; supports `hold` with TOCTOU target digest verification |

```yaml
language_version: 1
ritual: read-document
reagents:
  file: {select: [read_text]}
requires:
  - reagent: file
    action: read_text
    on: {root: /absolute/documents}
essence: {requests: 0, tool_calls: 1, time: 5s}
steps:
  - id: document
    operation: file.read_text
    arguments: {root: /absolute/documents, path: notes.txt, max_bytes: 65536}
return:
  document: "${steps.document.output}"
```

Replace the root and filename before saving this example. To point one ritual at
any project instead, take the root from an input (`on: {root: "${inputs.target}"}`)
and grant the directory the projects live in with `--under DIR`; the run then
holds only the root it was given. See
[roots chosen at run time](../concepts/authority.md#roots-chosen-at-run-time).

Paths in operations are
relative to the explicitly granted root, never to a model-selected working
directory. An unscoped file grant is refused. No recursive traversal
operation is provided. A list grant does not imply a read or write grant.

Every path component is opened relative to an already-open directory descriptor
with symlink following disabled. Absolute child paths, `.` and `..` components,
symlink traversal (including in the root), and non-regular file reads fail. A
symlink is refused as *not authorized*, wherever it points.
Listings identify symlinks but never follow them. Linux `/proc/self/fd` is used to
list an already-open directory. This is the current Linux implementation, not a
portable filesystem sandbox.

Oversized reads/listings fail with exhaustion; they do not return silently
truncated data. Invalid UTF-8 fails. Deadlines are checked between reads, but a
blocking native filesystem call may delay completion. Concurrent edits can change
what is read; the hash identifies the bytes returned, not an atomic file snapshot.
Filesystem permissions still apply. A granted directory's contents, including
hard links and mounted filesystems, remain the owner's responsibility.

Read text is retained in operation evidence and may be passed explicitly into
later steps. There is no automatic secret detection or data-egress policy.
