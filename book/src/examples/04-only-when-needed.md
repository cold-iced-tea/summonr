# 4. Route a contact form

**Idea:** deterministic first. Spend inference only on the part of the input that is actually ambiguous.

A website's contact form has a category dropdown: sales, support, press, or other. Most people pick a real category, and for them there is nothing to judge. Only "other" needs someone, or something, to read the message.

## The ritual

`examples/04-route-form/route-form.yaml`:

```yaml
language_version: 1
ritual: route-form
description: Route a contact-form submission. Trust the form's own category; ask a model only when the sender picked "other".
inputs:
  form: {type: string}
model: ollama/qwen3:4b
settings: {temperature: 0}
reagents:
  data: {select: [parse_json]}
essence: {requests: 1, tokens: 8000, tool_calls: 1, time: 2m}
steps:
  - id: submission
    operation: data.parse_json
    arguments:
      text: "${inputs.form}"
      schema:
        type: object
        required: [category, message]
        additionalProperties: false
        properties:
          category: {type: string, enum: [sales, support, press, other]}
          message: {type: string, minLength: 1, maxLength: 5000}
  - if: steps.submission.output.category == "other"
    then:
      - id: judged
        model:
          instructions: |
            A visitor sent this through a contact form without choosing a category.
            Pick the category that fits: sales (buying, pricing, demos),
            support (help with the product), press (journalists, interviews).
            If none fits, answer abstain. Answer with JSON only.
          prompt: "${steps.submission.output.message}"
          schema:
            type: object
            required: [category]
            additionalProperties: false
            properties:
              category: {type: string, enum: [sales, support, press, abstain]}
          abstain_if: output.category == "abstain"
return:
  from_form: "${steps.submission.output.category}"
  from_model: "${steps.judged.output.category}"
```

The `if` step is the whole idea. The model step lives inside it, so it runs only when the condition holds.

## Run it

A submission that already says what it is:

```console
$ summonr run examples/04-route-form/route-form.yaml \
    --arg form='{"category":"support","message":"The import button is greyed out."}' --wait
{
  "from_form": "support",
  "from_model": null
}
* ELEGY AT REST (returned) · 00:00 · 0 requests · 0 tokens
```

**Zero requests, zero tokens.** The branch was skipped, and a skipped step's output is `null`.

One that needs reading:

```console
$ summonr run examples/04-route-form/route-form.yaml \
    --arg form='{"category":"other","message":"Hi, I write for a trade magazine and would love to interview your founder about the launch."}' --wait
{
  "from_form": "other",
  "from_model": "press"
}
* ELEGY AT REST (returned) · 00:01 · 1 requests · 99 tokens
```

If 90% of your submissions pick a real category, this ritual spends inference on the other 10%. The records show it: count the runs with `0 requests`.

## The general pattern

```text
parse and validate         (deterministic, always)
        │
   is it ambiguous?  ──no──►  use the known answer
        │ yes
   ask a model, bounded
        │
validate the answer        (deterministic)
```

This is the shape Summonr is built around (see [Deterministic work, deliberate inference](../concepts/determinism.md)). It applies well beyond forms:

- an expense whose merchant is already in your category table needs no model; an unknown merchant does;
- a log line that matches a known error signature needs no model; a new one does;
- a support ticket with an order number can be looked up; a vague one needs reading.

The `if` makes the boundary visible in the procedure itself, not buried in a prompt that asks the model to "only think when necessary".

## Notes

- `from_model` is `null` when the model was not asked. Your caller decides: `from_model ?? from_form`.
- Conditions support `==`, `!=`, `and`, `or`, `not`. There is no `<` or `>` yet, so branch on categories and flags, not thresholds.
- A malformed form (say, `"category": "urgent"`) fails at `parse_json` with exit 11, before any model sees it.

## Try it

- Send `{"category":"other","message":"asdf"}`. Does it abstain?
- Change the condition to `steps.submission.output.category == "other" or steps.submission.output.category == "support"` to have support messages double-checked by a model.
