# Validate before spending inference

Use the offline compiler while authoring a ritual:

```bash
summonr validate ./classify.yaml --json
# Equivalent plain spelling:
summonr compile ./classify.yaml --json
```

This reads the definition, embeds declared binding documents, resolves saved child dependencies from the local database read-only, validates the definition, and returns its canonical definition, hash, language version, and effect plan. It does not start a core, grant permissions, run tools, or contact a model backend.

A required child must already be inscribed. The compiler does not silently install dependencies. Compilation is not permission to execute: invocation still resolves inputs and checks current grants.

## Contract versioning

`language_version: 1` selects the supported language. Omission means version 1 for existing definitions. Other versions are rejected. This is separate from a saved ritual's inscription version and content hash.

A ritual may declare `output_schema`. The final return value is checked against it before a successful return is recorded. A model step's `schema` checks that individual model response instead. These are different contracts.

## Supported schema dialect

The runtime supports object schemas containing:

- `type`: one of object, array, string, boolean, integer, number, null;
- `enum`, `required`, `properties`, and `items`;
- boolean `additionalProperties`;
- `minimum`, `maximum`, `minLength`, `maxLength`, `minItems`, `maxItems`;
- `title`, `description`, and `default` as annotations (not default insertion).

Unsupported or malformed keywords are errors, including inside nested properties. `$ref`, union types, `oneOf`, regular expressions, and schema-valued `additionalProperties` are not supported. This is a specified subset, not full JSON Schema 2020-12.

String lengths count Unicode scalar values. Required fields, enums, nested items, closed objects and bounds are checked after decoding. A decoder accepting a schema does not waive the local check.

## Diagnostics and limits

Invalid definitions return exit 2 in the standard JSON envelope. Whole-definition diagnostics contain a code, file, and message. Parser failures may be a single message. Source spans, a language server, and complete static type inference for references are not implemented.

References must name declared inputs or earlier steps. Literal tool arguments are checked at compilation; rendered arguments are checked again before calling a tool. A field existing in an earlier step does not by itself prove its runtime type or presence. Model-generated content still needs validation and safe control flow.

Step modifiers are specific to their kind. `hold` and `verify` belong to operation steps; iteration controls belong to `each`; branch bodies belong to `if`. Attaching `hold` to a model step is an error, not an ignored request.

## A closed return contract

```yaml
language_version: 1
ritual: classify
model: mock/keyword
inputs:
  text: {type: string}
essence: {requests: 1, tokens: 2000, time: 10s}
output_schema:
  type: object
  required: [intent]
  additionalProperties: false
  properties:
    intent: {type: string, enum: [receipt, oos]}
steps:
  - id: answer
    model:
      prompt: "${inputs.text}"
      schema:
        type: object
        required: [intent]
        additionalProperties: false
        properties:
          intent: {type: string, enum: [receipt, oos]}
return:
  intent: "${steps.answer.output.intent}"
```

This example tests mechanics. A mock classification is not quality evidence.
