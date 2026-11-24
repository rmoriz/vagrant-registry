# vagrant-registry

A read-only Vagrant box registry. It serves catalog JSON — which versions and
providers exist for an `org/box`, and where their `.box` files live — and never
touches the binaries themselves. Catalogs live as one JSON document per box
under `data/`, are validated and rendered into an embedded table at build time,
and are published by pull request. The same handler runs as a Cloudflare Worker
and as a container image, so both targets serve identical catalog bytes.

## Use it

Point Vagrant at the registry and add a box by its `org/box` name. Vagrant
fetches the catalog from here and the `.box` from wherever the catalog says, so
the download never passes through this service.

```bash
export VAGRANT_SERVER_URL="https://vagrant-registry.roland-366.workers.dev"
vagrant box add --provider virtualbox oraclelinux/8-aarch64
```

Or per project, without touching your shell profile:

```ruby
# Vagrantfile
Vagrant.configure("2") do |config|
  config.vm.box = "oraclelinux/8-aarch64"
  config.vm.box_server_url = "https://vagrant-registry.roland-366.workers.dev"
end
```

Or without discovery, straight against the catalog:

```bash
vagrant init oraclelinux/8-aarch64 \
  https://vagrant-registry.roland-366.workers.dev/catalogs/oraclelinux/8-aarch64.json
```

Then the usual cycle:

```bash
vagrant box list                       # what is installed
vagrant box update oraclelinux/8-aarch64   # pull a newer build
vagrant box remove oraclelinux/8-aarch64 --provider virtualbox --all
```

Two things to keep in mind:

- **The host is remembered.** Vagrant stores the catalog URL in
  `~/.vagrant.d/boxes/<box>/<version>/metadata_url` and re-reads it on
  `vagrant box update`. Use a stable hostname, not a temporary tunnel.
- **Architecture matters.** Apple Silicon needs an `-aarch64` box and VirtualBox
  7.1+; the plain names are `x86_64`. A wrong provider fails at download, not at
  discovery.

Browse what exists with the index page (`/`); Vagrant itself has no search.

## Served paths

| Path | Purpose |
|---|---|
| `/api/v2/vagrant/{org}/{box}` | Vagrant's preferred discovery route |
| `/{org}/{box}` | Legacy fallback, identical bytes |
| `/catalogs/{org}/{box}.json` | Direct catalog URL for `box_url` |

All three accept `GET` and `HEAD`; `HEAD` carries the same status and
`Content-Type` with no body. `/` is an index page and `/healthz` is a health
probe that reports the catalog count. Unknown boxes are `404`; any other method
on a resolved path is `405` with `Allow: GET, HEAD`. Paths are decoded exactly
once, so `%2F` splits a segment while `a..b` stays one name.

## Add a box

Every catalog is one file, and a pull request is the only way in. The build
refuses a document that does not match its own path, so a wrong copy is caught
before review.

1. Fork and clone, then branch:

   ```bash
   gh repo fork rmoriz/vagrant-registry --clone
   cd vagrant-registry
   git switch -c box/example-toolbox
   ```

2. Create the document. Its path without `.json` **must** equal `name`:

   ```bash
   mkdir -p data/example
   cp data/acme/base.json data/example/toolbox.json
   ```

   `data/example/toolbox.json` may only declare `"name": "example/toolbox"`.

3. Edit it: description, every version, every provider. A copied document still
   carries the old name until you change it. Point each `url` at the real
   artifact, with a `checksum`/`checksum_type` pair — Vagrant verifies it after
   download. The field reference is in [docs/usage.md](docs/usage.md).

4. Validate the whole tree. The build reads every document, reports every
   failure with file and field, and re-renders the catalogs:

   ```bash
   cargo test -p vagrant-registry-core
   ```

5. Commit, push, and open the pull request:

   ```bash
   git add data/example/toolbox.json
   git commit -m "data: add example/toolbox"
   git push -u origin box/example-toolbox
   gh pr create --fill
   ```

CI runs the gate, the real Worker and container smoke tests, and re-checks the
catalog tree. Once merged to `main`, the box is live; `staging` is a separate
Worker for trying catalogs early. Unknown fields, bad versions, a name/path
mismatch, and a document over 1 MiB all fail the build rather than ship.

`data/oraclelinux/` is a mirror of Oracle's published box metadata; refresh it
with `./scripts/oracle_boxes.py` instead of editing it by hand.

## Layout

```text
crates/core     catalog model, validation, rendering, path grammar, dispatch, tests
crates/core/build.rs  validates data/ and embeds the rendered catalogs
crates/worker   Cloudflare Worker adapter (wasm32 only)
crates/server   axum adapter with --healthcheck, in a distroless image
data/           one catalog document per box: data/<org>/<box>.json
scripts/        contract smoke tests for the Worker and the container
```

## Local development

Requirements: Rust 1.91 (`rust-toolchain.toml` pins it), Node 22, and Docker
for the image. `worker-build 0.9.0` is needed for the Worker only.

```bash
npm ci
cargo test -p vagrant-registry-core -p vagrant-registry-server
npx wrangler dev                                # Worker on port 8787
cargo run -p vagrant-registry-server            # server on port 8080, PORT overrides
```

Neither path needs a secret. `scripts/smoke.sh <base-url>` runs every contract
check against a live target; `scripts/container_smoke.sh` builds, health-checks
and smokes the container image.

## Deploy

Push to `main` to deploy production to Cloudflare Workers, or push to `staging`
for a staging Worker; both go through the test gate and use two GitHub
environments with their own secrets. Alternatively run the published image
behind your own TLS proxy. Setup, rollback, and proxy requirements are in
[docs/deployment.md](docs/deployment.md).

Merging is the only publish step: catalogs are compiled in, so a catalog error
fails the build and the merge gate before anything ships.

## Limits

- 1 MiB per rendered catalog document, 16 MiB in total; both are build errors.
- Catalog responses are cacheable by the shared cache for a year
  (`public, max-age=0, s-maxage=31536000`); browsers revalidate. A deploy
  starts a cold cache because the Worker version is part of the cache key.
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
