# vagrant-registry

A read-only Vagrant box registry. It serves catalog JSON — which versions and
providers exist for an `org/box`, and where their `.box` files live — and never
touches the binaries themselves. Catalogs live as one JSON document per box
under `data/`, are validated and embedded at build time, and are published by
pull request. The same handler runs as a Cloudflare Worker and as a container
image, so both targets serve identical catalog bytes.

## Served paths

| Path | Purpose |
|---|---|
| `/api/v2/vagrant/{org}/{box}` | Vagrant's preferred discovery route |
| `/{org}/{box}` | Legacy fallback, identical bytes |
| `/catalogs/{org}/{box}.json` | Direct catalog URL for `box_url` |

All three accept `GET` and `HEAD`; `HEAD` carries the same status and
`Content-Type` with no body. `/` is an index page and `/healthz` is a health
probe. Unknown boxes are `404`; any other method on a resolved path is `405`
with `Allow: GET, HEAD`.

## Add a box

Copy `data/acme/base.json` to `data/<org>/<box>.json`, make the document's
`name` match that path, edit it, run `cargo test -p vagrant-registry-core`, and
open a pull request. Full field rules, client setup, and local preview are in
[docs/usage.md](docs/usage.md).

## Deploy

Push to `main` to deploy production to Cloudflare Workers, or push to `staging`
for a staging Worker; both go through the test gate and use two GitHub
environments with their own secrets. Alternatively run the published image
behind your own TLS proxy. Setup, rollback, and proxy requirements are in
[docs/deployment.md](docs/deployment.md).

## Limits

- 1 MiB per rendered catalog document, 16 MiB in total; both are build errors.
- Catalog responses are cacheable for 60 seconds (`public, max-age=60`).
- Documents are strict: unknown fields, invalid versions, and name/path
  mismatches fail the build.

## Non-goals

- No admin API or admin UI, and no authentication.
- No database or any runtime store.
- No box hosting, upload, or proxying; provider URLs point at your storage.
- No Vagrant Cloud `/api/v1` compatibility.

The design is in
[docs/superpowers/specs/2026-11-24-vagrant-registry-rust-design.md](docs/superpowers/specs/2026-11-24-vagrant-registry-rust-design.md).

## License

MIT — see [LICENSE](LICENSE). Copyright (c) 2026 Roland Moriz.
