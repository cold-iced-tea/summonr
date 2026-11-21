# Reliquary: signed ritual packs

A **reliquary** (plain spelling: `registry`) packages a saved ritual, its full pinned child dependency closure, and optional author-signed assay evidence into a content-hashed, Ed25519-signed pack (`summonr-pack-v1`). Packs travel either peer-to-peer between cores over `summonr listen` or offline as portable `.pack.json` files.

## Trust boundaries

- **Pinned keys only:** `reliquary show`, `reliquary inscribe`, and `reliquary import` verify the pack signature against a pinned peer key or an explicit `--key` argument—never a key claimed inside the pack file.
- **Full closure verification:** Every child ritual referenced by the root (and transitively by children) is bundled inside the pack and checked against its pinned content hash.
- **Zero-grant transfer:** Grants never travel with a pack. Inscribing a pack shows the static effect plan and reports any missing local grants that the owner must explicitly authorize.
- **Author-claimed evidence:** Assay results and touchstone baselines bundled in a pack are recorded as *author-claimed* evidence, separate from local `baselines`, until you run `summonr assay` against your own backend.

## Commands

| Keyword form | Plain spelling | Effect |
| --- | --- | --- |
| `reliquary publish RITUAL [version N]` | `registry publish …` | Package and sign a saved ritual and its child closure |
| `reliquary retract NAME [version N] [because T]` | `registry retract …` | Mark a published pack retracted |
| `reliquary add PEER` / `remove PEER` | `registry add/remove PEER` | Subscribe to or remove a pinned peer's reliquary |
| `reliquary list [PEER]` | `registry list [PEER]` | List local packs or browse a peer's catalog |
| `reliquary show [PEER] NAME [version N]` | `registry show …` | Verify and inspect a pack's effects and author-claimed evidence |
| `reliquary inscribe [PEER] NAME [version N]` | `registry save …` | Verify and inscribe a pack's closure (without grants) |
| `reliquary export NAME FILE` | `registry export …` | Write a signed `.pack.json` bundle for offline sharing |
| `reliquary import FILE [key K]` | `registry import …` | Verify and inscribe an offline `.pack.json` bundle |
