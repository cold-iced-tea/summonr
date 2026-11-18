# Standard-library building blocks

Run without any model or grants:

```bash
summonr run examples/06-standard-library/collections.yaml --wait --json
```

Expected result: `{"positive":[2,6],"total":8}`, with zero model requests.
The bound includes three map items, three filter items, two fold items, three
collection calls and five primitive calls.

These operations ship with the executable; you do not need to save the example
to make them available. Save it only if you want this particular composition as
a callable ritual.

See the book's standard-library, data, file, and HTTP references for the primitive
catalogue and complete grant examples. No sample grants are applied automatically.
