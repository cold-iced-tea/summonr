# References and conditions

## References

A reference names a value with a dotted path inside `${ ... }`.

| Starts with | Refers to |
| --- | --- |
| `inputs.NAME` | A resolved input |
| `steps.ID` | A step's record: `{status, kind, output}` |
| `steps.ID.output...` | That step's output, and fields inside it |
| `VAR...` | The current loop element, inside an `each` |

Path segments walk into objects by key and into lists by index: `steps.select.output.0.id`.

Anything else is rejected at validation as ambiguous. `${input.text}` is an error, not a guess at `inputs.text`.

A step can only refer to steps **defined earlier** in its own scope or an enclosing one. Ids inside an `each` body are not visible outside it.

### Typed or textual

- A string that is **exactly one** reference yields the value with its type: `newest: "${inputs.count}"` passes the integer `6`.
- Any other string has each reference substituted as text: `"Folder: ${steps.c.output.folder}"`.

### Nulls and missing fields

A `null` along a path stays `null`. That is how a skipped step's output behaves: `steps.move.output.moved` is `null` if `move` was skipped. A missing key in an object, or an out-of-range index, is an error at run time.

## Conditions

Conditions appear in `if:` and `abstain_if:`. They are written without `${ }`.

```yaml
if: inputs.apply and steps.classify.output.folder != "abstain"
abstain_if: output.folder == "abstain"
```

| Syntax | Meaning |
| --- | --- |
| `a == b`, `a != b` | Equality of any two values |
| `a < b`, `a <= b`, `a > b`, `a >= b` | Ordering of two integers, two floating-point numbers, or two strings; no implicit conversion |
| `a and b`, `a or b`, `not a` | Boolean logic; operands must be `true` or `false` |
| `( ... )` | Grouping |
| `"text"`, `'text'` | Strings |
| `42`, `-2`, `0.5`, `1e3` | Numbers; integers retain their exact representation |
| `true`, `false`, `null` | Literals |
| `inputs.x`, `steps.id.output.f`, `item.f` | References |
| `output.f` | The step's own output (only in `abstain_if`) |

Precedence, loosest first: `or`, `and`, `not`, then comparison.

Arithmetic is an explicit `data.arithmetic` operation, not an expression operator. Conditions are limited to 4,096 bytes and 256 tokens. A condition must evaluate to a boolean; anything else is an error. `and` and `or` short-circuit: if the left side decides the result, the right side is not evaluated.
