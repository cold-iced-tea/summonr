# 3. Total a receipt

**Idea:** split the work by what each side is good at. The model *reads* messy text. Code *computes*. Then code checks the model, by making two independent readings agree.

Language models are good at reading a receipt and unreliable at arithmetic. So don't ask one to add. Ask it to copy numbers into a structure, and let code do the sum exactly.

## The ritual

`examples/03-receipt-total/receipt-total.yaml`:

```yaml
language_version: 1
ritual: receipt-total
description: Read a pasted receipt, extract the line amounts with a model, add them up in code, and check the sum against the printed subtotal.
inputs:
  text: {type: string}
model: ollama/qwen3:4b
settings: {temperature: 0}
reagents:
  data: {select: [sum_integers, count, assert]}
essence: {requests: 1, tokens: 8000, tool_calls: 3, time: 2m}
steps:
  - id: extract
    model:
      instructions: |
        You read receipts. Report two things, in cents, as integers (4.50 is 450):
        - line_items_cents: the price of every purchased line item. Do not
          include subtotals, tax, tips, totals, or payments.
        - printed_subtotal_cents: the subtotal printed on the receipt.
        Copy numbers only. Do not add anything up. Answer with JSON only.
      prompt: "${inputs.text}"
      schema:
        type: object
        required: [line_items_cents, printed_subtotal_cents]
        additionalProperties: false
        properties:
          line_items_cents:
            type: array
            maxItems: 200
            items: {type: integer, minimum: 0, maximum: 10000000}
          printed_subtotal_cents: {type: integer, minimum: 0}
  - id: lines
    operation: data.count
    arguments: {value: "${steps.extract.output.line_items_cents}"}
  - id: total
    operation: data.sum_integers
    arguments: {values: "${steps.extract.output.line_items_cents}"}
  - if: steps.total.output != steps.extract.output.printed_subtotal_cents
    then:
      - id: mismatch
        operation: data.assert
        arguments:
          condition: false
          message: "the line items do not add up to the printed subtotal; check the receipt by hand"
return:
  line_items_cents: "${steps.extract.output.line_items_cents}"
  lines: "${steps.lines.output}"
  subtotal_cents: "${steps.total.output}"
```

What each part contributes:

1. **One model call** turns text into two things: a list of integers and one integer. Integers in cents, so nothing is ever rounded.
2. **`data.sum_integers`** adds with checked 64-bit arithmetic. Overflow fails; it never wraps.
3. **The cross-check.** The model read the line items *and* the printed subtotal. Those are two independent readings of the same receipt. If code's sum of the first doesn't equal the second, something is wrong: a missed line, a misread digit, or the receipt itself. `data.assert` with `condition: false` turns that into a failure with your message.

## Run it

```text
THE COPPER KETTLE
2 x Flat white          9.00
Almond croissant        3.75
Avocado toast          11.50
Sparkling water         2.95
Subtotal               27.20
Tax 8%                  2.18
TOTAL                  29.38
```

```console
$ summonr run examples/03-receipt-total/receipt-total.yaml \
    --arg text="$(cat examples/03-receipt-total/cafe.txt)" --wait --json
```

The result:

```json
{"line_items_cents": [900, 375, 1150, 295], "lines": 4, "subtotal_cents": 2720}
```

One model request. The model skipped the subtotal, tax, total, and card lines as told. 900 + 375 + 1150 + 295 = 2720, which matches the printed 27.20, so the check passed.

> Use `--json` here. The readable printer currently treats any list in a result as a batch and hides the other fields.

Now `doctored.txt`: same items, but someone changed the subtotal to 32.20.

```console
$ summonr run examples/03-receipt-total/receipt-total.yaml \
    --arg text="$(cat examples/03-receipt-total/doctored.txt)" --wait
* ELEGY SPOKE AMISS (invalid) · 00:01 · 1 requests · ...
  invalid output: the line items do not add up to the printed subtotal; check the receipt by hand
```

Exit code 11, and no total is returned. The same check catches a model that drops or misreads a line.

## What you can and can't trust

The record labels each piece of evidence:

| Value | Evidence class | Meaning |
| --- | --- | --- |
| `line_items_cents` | a model's claim | Probably right; checked for shape, not truth |
| `subtotal_cents` | computed by the runtime | Exactly the sum of those claims |
| the agreement check | computed by the runtime | Two independent readings matched |

The sum is exact. The extraction is still a claim, now corroborated by the receipt's own subtotal. That's a much stronger position than "the model said the total is 29.38."

## Try it

- Add a line to `cafe.txt` without changing the subtotal. The check should fail.
- Remove the `if` step and feed it `doctored.txt`. It now "succeeds" with a number that disagrees with the receipt.
- Extend it: also extract `printed_tax_cents`, add a second `data.sum_integers` over `[subtotal, tax]`, and check that against the printed total.
