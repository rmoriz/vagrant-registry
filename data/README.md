# Catalog data

One JSON document per box, using the catalog shape from the design contract.

- The path must equal `name`: `data/<org>/<box>.json` declares `"name": "<org>/<box>"`.
- Rendered output must stay within 1 MiB per document and 16 MiB in total.
- Unknown fields are rejected, so a typo fails the build instead of vanishing.

Validate everything with:

```bash
cargo test -p vagrant-registry-core
```
