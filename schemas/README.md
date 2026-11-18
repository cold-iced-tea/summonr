# Summonr definition contract, version 1

`ritual-v1.schema.json` is the public structural JSON Schema (Draft 2020-12) for
source and prepared definitions. Its identifier is `urn:summonr:ritual:1`; this
is an identifier, not a hosted download URL. The mdBook includes a downloadable
copy. Definitions do not accept a `$schema` field: configure an editor schema
association instead. `language_version` defaults to 1 when omitted.

The Rust parser, validator, preparation, and executor are authoritative. This
schema does not resolve references, pin children, enforce grants, validate duration
syntax, check tool argument schemas, or verify external state. Passing the schema
is necessary for the supported structural contract, not sufficient to execute.
Always run `summonr compile definition.json --json` and review the compiled result.
The model-output schema dialect is a different, intentionally restricted subset;
do not send this full document to `data.parse_json` or use it as a model schema.

The runtime accepts JSON, YAML, and SDK-emitted JSON through the same semantic
pipeline. Content hashes cover the **prepared canonical definition**, including
embedded bindings and child pins, not source-file bytes or the builder script.
Canonicalization uses the Rust serializer's defaults and omissions; in particular
version 1 is omitted from the canonical object to preserve existing hashes.
Whitespace and key order do not change a hash; array order, descriptions, prompts,
IDs, and schema contents can. No new hash algorithm is introduced here.

Regenerate with `python3 scripts/generate_definition_schema.py`. Run
`python3 scripts/check_authoring.py` with the development requirements installed.
It checks the generated artifact, the paired examples' structural shapes, positive and
negative fixtures, and exact prepared-definition/hash/effect-plan parity across
five Python/YAML examples. Structurally valid semantic failures are deliberate
fixtures, not schema validation successes masquerading as runtime acceptance.
When Rust definition fields change, update this contract and its tests together.

This is not a proof that every conceivable schema-valid document is semantically
valid, nor a claim that a structural schema generates the Rust validator.
