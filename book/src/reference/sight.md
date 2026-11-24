# The `sight` reagent (vision)

Sight lets a model step look at an image without letting the model choose what it sees. Code reads the image first, can branch on its size and type, and only then hands a *handle* to a model step. In plain prose this is image input, or `vision`.

Select the operation under `reagents`, declare it under `requires` with an absolute `on.root`, and have the owner grant the same root:

```bash
summonr allow read-page --from sight --select read_image --on root=/absolute/scans
# or, for a root taken from an input, any directory beneath one:
summonr allow read-page --from sight --select read_image --under /absolute/scans
```

An unscoped grant is refused.

| Operation | Arguments | Output |
| --- | --- | --- |
| `sight.read_image` | `root` (absolute, granted), relative `path`, optional `max_bytes` (1 to 10,485,760) | A handle: `kind: "image"`, `root`, `path`, `sha256`, `mime`, `bytes`, `width`, `height` |

```yaml
reagents:
  sight: {select: [read_image]}
requires:
  - {reagent: sight, action: read_image, on: {root: /absolute/scans}}
steps:
  - id: page
    operation: sight.read_image
    arguments: {root: /absolute/scans, path: "${inputs.page}"}
  - id: read
    model:
      instructions: "Transcribe the page."
      prompt: "Return the text exactly."
      images: ["${steps.page.output}"]
      schema: {type: object, required: [text], properties: {text: {type: string}}}
```

`examples/08-sight` takes its root as an input and is granted with `--under` (see the examples README).

## What is read

- The type comes from the file's magic bytes, not its name: PNG, JPEG or WebP. Anything else fails. GIF and animated WebP are refused.
- Path rules match [`file`](file.md): every component is opened relative to a directory descriptor, with no symlinks, no `..`, and regular files only.
- A file over the limit fails the step as `invalid`, naming the limit; it is never truncated. The limit is the lower of `max_bytes` (default 10 MiB) and the owner's `sight.max_bytes` in `config.yaml`, applied at read time too. Being `invalid` rather than `exhausted`, one oversize file does not abort an `each` with `on_failure: continue`, and retry and fallback apply as for any bad input. A read that runs out of time is still `exhausted`.
- The output is a handle, never bytes. Reads are `deterministic over live state`, like other reads.

## Handles are issued, not asserted

Each run keeps a table of handles that `read_image` issued, shared with child rituals. A model step accepts only handles in that table. A handle-shaped object that arrives through inputs, MCP arguments or model output is denied, even with the right fields.

## Re-read and verify

At model-step time the core re-checks the run's grant and re-reads the file through the same path rules. The sha256 must match the handle; otherwise the step fails as a stale image and nothing is sent. A handle whose recorded size exceeds the current (possibly config-lowered) limit is invalid. All of these checks happen before a request is made or cost charged. No image copies are kept in Summonr's state; the bytes sent are exactly the bytes that were hashed.

## Limits

Defaults are 10,485,760 bytes per image and 8 images per step. `config.yaml` may lower them with `sight: {max_bytes, max_per_step}`; a higher value is a load error. See [Configuration](config.md).

## Backends and the vision check

Ollama receives the images in a separate `images` field on the user message (the text stays in `content`) and the model must report `vision` in `/api/show`; the answer is checked once per model and cached, and skipped in drills and replays. OpenAI-compatible providers receive `image_url` data URLs, and Anthropic receives `image` base64 blocks; for both, the images come before the text in the user turn, in `images:` order. For these two there is no reliable pre-check, so an unsupported model surfaces the backend's error. The mock backend accepts images, so a [drill](../guides/safe-building.md) with a scripted reply needs no vision model.

## Records hold digests only

- A recorded request payload replaces each image's data with `{"sight": {"sha256", "mime", "bytes"}}` at the same position. This is the one substitution to the "exact body sent" rule.
- Cassette request hashes cover image digests, not bytes.
- Backend error text for image requests is scrubbed of base64 (exact copies, and any run of 64 or more base64-looking characters, including truncated, `\/`-escaped or line-wrapped copies) and capped at 2,000 characters.
- Elegies, epitaphs, events and MCP records show handles and digests only.
- `revive` re-adopts only handles from a live `read_image` result at the top level of the run, never from a fallback's output, and re-checks the digests. Handles produced inside child rituals or carried out of `each` items are refused after a revive (it fails closed); re-read the image in the step that uses it.

## Memory

Each image step holds the raw bytes, a base64 copy and the serialized request body at once. Many large images combined with a high `parallel` use proportionally more memory, so size `parallel` for the images, not just the model.

Image tokens count under the `tokens` essence; see [Essence](../concepts/essence.md).
