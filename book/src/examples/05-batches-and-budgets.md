# 5. Label reviews on a budget

**Idea:** run one ritual over many items, under a hard limit that the runtime enforces. When something looks wrong, read the record instead of guessing.

You have a pile of product reviews and want each one labeled positive, negative, mixed, or not-a-review. Two rituals: one labels a single review, and one runs it over a list.

## Two rituals

`examples/05-label-reviews/label-review.yaml` labels one review:

```yaml
language_version: 1
ritual: label-review
description: Label the sentiment of one product review.
inputs:
  text: {type: string}
model: ollama/qwen3:4b
settings: {temperature: 0}
essence: {requests: 1, tokens: 4000, time: 1m}
steps:
  - id: label
    model:
      instructions: |
        Label the overall sentiment of this product review.
        - positive: the reviewer is happy with the product.
        - negative: the reviewer is unhappy with the product.
        - mixed: the review contains both real praise and real complaints.
        - abstain: the text is not a review of the product (a question, spam).
        Answer with JSON only.
      prompt: "${inputs.text}"
      schema:
        type: object
        required: [sentiment]
        additionalProperties: false
        properties:
          sentiment: {type: string, enum: [positive, negative, mixed, abstain]}
      abstain_if: output.sentiment == "abstain"
return:
  sentiment: "${steps.label.output.sentiment}"
```

`examples/05-label-reviews/label-reviews.yaml` runs it over a list:

```yaml
language_version: 1
ritual: label-reviews
description: Label a list of reviews, within a fixed budget of model requests.
inputs:
  reviews: {type: list}
essence: {requests: 10, tokens: 40000, items: 50, time: 5m}
steps:
  - id: labels
    each: review in inputs.reviews
    on_failure: continue
    steps:
      - id: label
        ritual: label-review
        arguments: {text: "${review}"}
return:
  labels: "${steps.labels.output}"
```

- **`each: review in inputs.reviews`** runs the body once per element, in order.
- **`ritual: label-review`** calls the saved single-review ritual. Each call is its own linked run, in a fresh context. One review's text never leaks into the next.
- **`on_failure: continue`** records a failed item and moves on, rather than stopping the batch.
- **`essence`** on the parent bounds the *whole* batch, children included: at most 10 model requests and 50 items.

A ritual that calls another must call a *saved* one, so inscribe the child first:

```bash
summonr inscribe examples/05-label-reviews/label-review.yaml
```

## Run it

`reviews.json` holds five reviews, one of which is really a question:

```console
$ summonr run examples/05-label-reviews/label-reviews.yaml \
    --arg reviews="$(cat examples/05-label-reviews/reviews.json)" --wait
* labels: 5 items
  0            returned   {"sentiment":"positive"}
  1            returned   {"sentiment":"negative"}
  2            returned   {"sentiment":"mixed"}
  3            abstained  {"sentiment":"abstain"}
  4            returned   {"sentiment":"positive"}
  counts     1 abstained · 4 returned
* ELEGY AT REST (returned) · 00:01 · 5 requests · 534 tokens
```

"Does anyone know if this ships to Canada?" abstained, as it should.

## When the budget runs out

Change the parent's essence to `requests: 3` and run it again:

```console
* labels: 4 items
  0            returned   {"sentiment":"positive"}
  1            returned   {"sentiment":"negative"}
  2            returned   {"sentiment":"mixed"}
  3            exhausted  essence exhausted: in label-review (s-…): request limit of 3 reached
  counts     1 interrupted · 3 processed · 2 remaining · 3 returned · 5 selected
* ELEGY SPENT (exhausted) · 00:01 · 3 requests · 321 tokens
```

Exit code 10. The fourth review was never sent to the model. The batch reports exactly what happened: 5 selected, 3 processed, 2 remaining. A partial batch can never pass itself off as a complete one.

This is a ceiling the runtime enforces, not a polite request in a prompt. The child's own `requests: 1` and the parent's `requests: 3` are both checked on every call, so no child can spend more than its parent has.

## Debugging from the record

The first version of `label-review` defined only two of its labels:

```yaml
instructions: |
  Label the overall sentiment of this product review.
  mixed means it clearly praises and criticizes. abstain means it is
  not a review of the product. Answer with JSON only.
```

It labeled *every* review "mixed", including "Best purchase this year." Instead of guessing why, look at what was sent:

```bash
summonr logs s-PARENT                  # find the child run ids ("called label-review v1 as s-…")
summonr logs s-CHILD --json            # the model event holds payload, response, usage
```

The payload showed the review arriving intact, and the response was literally `{"sentiment": "mixed"}`. So the input was fine; the instructions were the problem. With only "mixed" and "abstain" spelled out, the small model leaned on the labels it had been told about. Defining all four fixed it.

The fix produced **version 2** of `label-review` (`summonr inscribe ... --replace`). The old runs still point at version 1, so the records show which instructions produced which labels.

## Try it

- Add 20 reviews and set `items: 10`. Watch the item limit stop the batch.
- Swap `on_failure: continue` for `stop` and add a review the model cannot handle within its token budget.
- Use `--model mock/keyword` to test the batch and budget mechanics with no model.
