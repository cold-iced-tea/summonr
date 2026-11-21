# Sight: image input for model steps

Status: design, 2026-11-21. Branch `feat/sight`.

## Intent

Model steps can only send text today (`ChatReq` has `system` and `user` strings, `src/backend.rs:18`). Sight lets a ritual show images to a vision model while keeping Summonr's rules: the model never chooses what it sees, code can inspect an image before inference is spent on it, and records hold digests, not image bytes.

There is no particular image type in mind. Sight is a general capability with conservative defaults; a use (handwriting transcription, screenshots, receipts) comes later and should need no redesign.

## Goals

- A `sight` reagent (plain spelling `vision`, as the alias for the reagent name in CLI/grant text where aliases exist) whose `read_image` operation reads an image beneath a granted root and returns a **handle**: digest and metadata, never bytes.
- Model steps accept `images:`, a list of handles, and send them to Ollama, OpenAI-compatible and Anthropic backends in each protocol's native form.
- Code can branch on handle metadata (mime, dimensions, size) before any model call.
- Mock and cassette backends, drills, `simulate`, `record`/`replay`, assays and MCP all work with images.

## Non-goals (this version)

- Image bindings embedded at inscription (approach C; planned next, separately).
- PDFs, page splitting, multi-frame images.
- Resizing, cropping, re-encoding or any transformation.
- Image handles from sources other than local files (`http` downloads, codex attachments). The handle shape should allow them later.
- Image output/generation.
- A new essence field for images.

## Anti-goals (failure even if it "works")

- Image bytes or base64 in elegies, epitaphs, cassettes, events, MCP results or the TUI.
- A model or calling agent getting an image sent that no granted `read_image` produced in this run (forged handle).
- Silently sending a changed file: what was hashed at `read_image` is exactly what is sent.
- Dispatching images to a model known not to support them.

## Approaches considered

- **A. Paths on the model step** (`images: [{root, path}]`, core reads at call time). Simplest, but images can only come from disk and code cannot look at an image before paying for a model call.
- **B. `sight` reagent producing handles; model steps take handles.** One extra step per image, but keeps "deterministic first" and generalises to other handle sources. **Chosen by the user.**
- **C. Image bindings at inscription.** Only fits fixed reference images. **Chosen by the user as a later addition**, not part of this work.

## Design

### The reagent

```yaml
reagents:
  sight: {select: [read_image]}
requires:
  - {reagent: sight, action: read_image, on: {root: /absolute/scans}}
steps:
  - id: page
    operation: sight.read_image
    arguments: {root: /absolute/scans, path: "${inputs.page}"}
```

Owner grant mirrors `file`: `summonr allow R --from sight --select read_image --on root=/absolute/scans`. An unscoped grant is refused.

`sight.read_image`: effect `read`, determinism `deterministic`.

- Arguments: `root` (absolute, granted), relative `path`.
- Path rules identical to the `file` reagent (descriptor-relative, no symlinks, no `..`, regular files only). Reuse its helpers rather than copying them.
- Type is decided by magic bytes, not extension. Accepted: PNG, JPEG, WebP. Anything else fails with a clear message. GIF/animated WebP refused.
- Size limit: 10 MiB per image by default.
- Output (the handle):

  ```json
  {"kind": "image", "root": "/absolute/scans", "path": "p1.png",
   "sha256": "…", "mime": "image/png", "bytes": 482113, "width": 1700, "height": 2200}
  ```

### Handles are issued, not asserted

The core keeps a per-run table of handles issued by `read_image` (keyed by sha256 + root + path), shared with child rituals the same way selection/authority flows. A model step refuses any `images` entry that is not an issued handle in its run tree, even if the JSON has the right fields. This blocks forged handles arriving via inputs, MCP arguments or model output.

Handles survive checkpoints and `revive`/`recast`: the table is rebuilt from checkpointed `read_image` outputs.

### Bytes: re-read and verify, no blob store (builder's call recorded here)

At model-step time the core re-reads the file through the same scoped path rules and checks its sha256 against the handle. A mismatch or missing file fails the step (no dispatch) with a "stale image" message, consistent with petition `stale` semantics. Reasons: no image copies accumulate in Summonr's state (private scans stay where the owner put them), and revive works without storage. Trade-off: non-file handle sources will need a content-addressed store later; the handle's `kind`/`root`/`path` fields leave room for that.

### Model steps

```yaml
- id: read
  model:
    instructions: "Transcribe the page."
    prompt: "Return the text exactly."
    images: ["${steps.page.output}"]
    schema: {text: string}
```

- `images` is a list of references (or a single reference to a list of handles). Max 8 per step by default.
- The compiler checks static types where reference typing allows (output of `sight.read_image`, or a loop variable over such outputs) and rejects literal objects.
- Images are placed in the user message before the prompt text, in list order.
- A model step without `images` is unchanged byte-for-byte (payloads, request hashes, existing cassettes still match).

### Backends

`ChatReq` gains `images: &[ImagePart]` (sha256, mime, bytes). Payload builders:

- Ollama `/api/chat`: user message `images: [base64…]`.
- OpenAI-compatible: user `content` becomes parts: `{"type":"image_url","image_url":{"url":"data:<mime>;base64,…"}}` then `{"type":"text","text":prompt}`.
- Anthropic: user `content` blocks `{"type":"image","source":{"type":"base64","media_type":…,"data":…}}` then text.
- Mock: accepts images; its estimated token count adds a fixed per-image estimate and is labeled `estimated` as today.

**Capability check.** For Ollama, before dispatching a step with images, query `/api/show` and require `"vision"` in `capabilities`; cache per model fingerprint for the core's lifetime. Missing → step fails before dispatch, nothing charged except as current pre-dispatch failures are. OpenAI/Anthropic/custom providers: no reliable check; dispatch and surface the backend error.

### Records

- The recorded `payload` replaces each base64 blob with a placeholder `{"sight": {"sha256", "mime", "bytes"}}` at the same position. Docs that say the payload is "the exact body sent" get a note about this one substitution.
- `cassette::model_hash` includes the ordered image digests (not bytes). Requests without images hash as before.
- Elegy step entries list image handles (digest, path, mime, dimensions). Epitaph fingerprints cover those digests through the existing body hash; no epitaph format change.
- Events, `scry` and MCP `get_record` show handles only.

### Limits and config

Defaults: 10 MiB per image, 8 images per step. Optional `config.yaml`:

```yaml
sight: {max_bytes: 4194304, max_per_step: 4}
```

Config may only lower the defaults. Token use is covered by existing `tokens` essence (backends report image tokens inside prompt tokens); docs warn that image steps overshoot more easily.

### Drills, assays, MCP

- Drill and assay cases pass image paths as ordinary inputs; scripted model replies work as now. A drill with `mock` must not need a vision-capable model.
- `simulate_ritual` performs reads (reads are not simulated), so handles are real.
- MCP `describe_ritual`/`preview_ritual` list `sight` requirements like any reagent.

## Constraints

- Rust, no async runtime; build and tests must pass `--offline` (CI runs `cargo test --offline`, clippy, `scripts/check_offline.py`).
- Image header parsing: `imagesize` 0.14 is in the local cargo cache and is header-only; acceptable if it passes the offline check, else hand-parse PNG/JPEG/WebP headers. Do not pull in the full `image` crate.
- `base64` is already a dependency.
- `schemas/ritual-v1.schema.json` must gain `images`. Note `scripts/generate_definition_schema.py` is known to drift (drops `defs/$defs`); edit the schema by hand or fix the script first, never let it overwrite blindly.

## Tests the builder should include

- Payload shape for each backend, with and without images (no-image payloads unchanged).
- Forged handle (from inputs, from model output) refused.
- File changed or removed between `read_image` and the model step → stale, no dispatch.
- Symlink, `..`, non-regular file, oversize, wrong magic bytes (e.g. `.png` that is a GIF) refused.
- Ollama model without `vision` refused before dispatch (mock HTTP server).
- No base64 anywhere in elegy, cassette, events or MCP record output.
- Cassette record/replay round trip with images; request hash stable.
- Handle table survives `revive`.
- Config can lower but not raise limits.

## Docs

- `book/src/reference/sight.md` (new), linked in `SUMMARY.md` beside the other reagent references.
- `ritual-format.md`: `images` on model steps.
- `capabilities.md`, `glossary.md` (`sight` / `vision`), `config.md` (`sight:`), `essence.md` (overshoot note).
- One small example with a fixture PNG and a mock drill, so it runs without a vision model.

## Left to the builder

Module layout, exact error strings, how the handle table is stored per run, the mock per-image token estimate, whether `imagesize` or a hand parser is used.
