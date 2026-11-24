# Vagrant Registry — Read-Only Design (Cloudflare Workers and Docker)

**Status:** approved
**Date:** 2026-11-24
**Supersedes:** the D1 admin surface of the Option D plan (2026-11-13); the
public contract, model, and validation rules come from
[the design and contract](2026-11-13-vagrant-registry-design.md) unchanged.

---

## 1. Intent

Run a public, catalog-only Vagrant box registry from catalog JSON kept in this
git repository and served by an immutable build artifact. Box binaries live
wherever the JSON says (S3, GitHub Releases, a CDN, an internal file server);
the registry never proxies them.

Why read-only: the catalog is edited by pull request, so git is already the
editor, the review process, and the audit log. A write API would add auth,
CSRF, and a database without replacing any of that.

Why an immutable artifact: a merge cannot half-publish, because the served
bytes are fixed at build time and a bad document fails the build. Rollback is
redeploying the previous artifact.

Two deploy targets, one behavior:

- **Cloudflare Workers** — no host to patch, free TLS, deploy on merge,
  `wrangler rollback` for rollback. Needs a Cloudflare account and two Actions
  secrets, no DNS zone. Both environments are workers.dev hostnames:
  `vagrant-registry.<account>.workers.dev` for production and
  `vagrant-registry-staging.<account>.workers.dev` for staging. A custom domain
  can be added later as a config change without touching code, and workers.dev
  then stays enabled so persisted catalog URLs keep resolving.
- **Docker** — a distroless image for a VPS, homelab, or any container
  platform. No Cloudflare account, no vendor lock-in; TLS and DNS are the
  operator's front proxy.

Both targets run the same handler, the same catalog table, and the same
contract tests, so the served bytes are identical by construction.

## 2. Goals

- Serve `HEAD|GET /api/v2/vagrant/{org}/{box}` and `HEAD|GET /{org}/{box}`
  with `Content-Type: application/json`, identical bytes on `GET` for both
  routes, and the same status and headers with an empty body on `HEAD`.
- Reject every invalid catalog at build time, so a bad document cannot reach
  production.
- Keep the published catalog canonical: the bytes served are the bytes the
  model renders, not the bytes committed.
- Make publishing one commit: edit `data/`, open a PR, merge.
- Keep all routing, method dispatch, status, and header logic in one place
  shared by both targets, with only response translation per target.

## 3. Non-goals

- No write path: no `PUT`, no admin API, no admin UI, no auth.
- No D1, KV, R2, Durable Objects, or any runtime store.
- No YAML authoring or import; JSON is the source format.
- No box hosting, upload, proxying, or Vagrant Cloud `/api/v1` compatibility.
- No search endpoint, accounts, teams, or download counters.
- No second implementation of the routing or header rules.

## 4. Anti-goals

These count as failure even if the routes return 200:

- A catalog whose `name` differs from its URL.
- A provider on a non-default architecture without
  `"default_architecture": false`.
- `HEAD` returning a body, a non-JSON `Content-Type`, or a method other than
  `GET`/`HEAD` succeeding.
- An invalid document deploying, or a valid document being altered by a
  renderer that disagrees with the model.
- A catalog that changes in production without a matching commit.
- The two targets drifting: same request, different status, headers, or bytes.

## 5. Constraints

- Rust 1.91, edition 2024; Worker target `wasm32-unknown-unknown`, `worker`
  0.9, Wrangler 4; server target native with `axum` + `tokio`.
- Workspace crates: `vagrant-registry-core` (pure, no runtime crate in its
  dependency tree), `vagrant-registry-worker` (`cdylib`), and
  `vagrant-registry-server` (binary). The root `Cargo.toml` is a virtual
  manifest; only `crates/core/build.rs` and the two adapter crates exist.
- Data layout: `data/<org>/<box>.json`; the path without the extension must
  equal `name`. Limits apply to **rendered** bytes: 1 MiB per document, 16 MiB
  in total, both enforced at build.
- Data documents are strict: unknown fields are an error, never dropped.
- Every response sets `X-Content-Type-Options: nosniff`; only a catalog `GET` or
  `HEAD` may be cached (`public, max-age=60`), everything else is `no-store`.
- Cloudflare may omit `Content-Length` or report `0` on `HEAD`; status and
  `Content-Type` are the contract. Vagrant does not read `Content-Length` for
  discovery.
- Docker image: multi-stage build, `distroless/cc` runtime, non-root user,
  documented run flags for a read-only root filesystem, `PORT` (default
  `8080`), `/healthz` as the container healthcheck.
- Cloudflare deploy needs `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`
  as GitHub Actions secrets, one per environment. No zone or DNS record is
  required. Production is the `vagrant-registry` Worker, staging the
  `vagrant-registry-staging` Worker; both keep workers.dev enabled.

## 6. Approaches

**A. Embed catalogs at build time, one shared handler, two adapters
(chosen).** `build.rs` reads `data/`, loads and validates every document
through the core crate, renders each to canonical JSON, and writes a sorted
`&[(name, bytes)]` table that the core crate includes. Core exposes
`handle(request) -> Reply`, a plain struct of status, headers, and body, with
no runtime types. The Worker crate maps `Reply` to `worker::Response`; the
server crate maps it to an `axum` response. Both are a few dozen lines.

Trades: content changes need a rebuild, and the binary grows with the catalog.
In exchange, validation runs in `cargo build` and CI, the artifact is
immutable, publishing is atomic by construction, the request path has no I/O,
a committed catalog cannot diverge from the served bytes, and the two targets
cannot drift on anything that matters.

**B. Fetch the JSON from `raw.githubusercontent.com` at runtime.** No rebuild
for content, and one small file. Trades: GitHub and its rate limits sit in the
request path, an unreachable upstream is a registry outage, and caching needs
a TTL plus a fallback story. Adds a runtime dependency both targets must
configure. Validation still has to run in CI, or a broken document goes live
without a build.

**C. Write catalogs to a KV namespace during deploy** (`wrangler kv bulk
put`). Same freshness model as A with a Cloudflare-only namespace, an extra
deploy step, and no equivalent for Docker. Worth it only once the catalog is
too large for one binary.

**D. Two independent apps, one per target.** Each target gets its own router
and handlers. Simple per app, but every rule has two homes, and the contract
tests would have to be duplicated or would cover only one side.

Rejected: Cloudflare Pages/static assets — extensionless keys need
`Content-Type`, which static hosting does not let us set, so the shorthand
`VAGRANT_SERVER_URL` route breaks. D1 — a database for a corpus that fits in
a few hundred kilobytes. YAML — the JSON is already the wire format, so JSON
removes a conversion and the numeric-scalar trap. Worker first, Docker later —
cheap now, but retrofitting the adapter seam after the handlers exist means
rewriting the handlers.

## 7. Design

### 7.1 Repository layout

```text
.
├── Cargo.toml                  # virtual workspace manifest
├── LICENSE                     # MIT
├── rust-toolchain.toml         # 1.91 + wasm32 target
├── Dockerfile                  # server target only
├── .dockerignore
├── wrangler.toml               # two environments, observability, no routes
├── package.json                # Wrangler devDependency and scripts
├── data/
│   ├── README.md               # how to add a box
│   └── acme/
│       └── base.json
├── crates/core/
│   ├── build.rs                # validates data/, enforces caps, emits the table
│   └── src/
│       ├── lib.rs
│       ├── model.rs            # Catalog, Version, Provider
│       ├── version.rs          # Gem::Version validation
│       ├── validate.rs         # aggregated validation
│       ├── render.rs           # canonical JSON
│       ├── tree.rs             # data/ loading, path==name rule, table
│       ├── http.rs             # Reply, request view, header and status rules
│       └── catalogs.rs         # include! of the generated table
├── crates/worker/
│   └── src/lib.rs              # #[event(fetch)] + Router + Reply mapping
├── crates/server/
│   └── src/main.rs             # axum fallback + Reply mapping + PORT + --healthcheck
├── scripts/smoke.sh            # shared contract checks against a base URL
├── scripts/container_smoke.sh  # docker run + smoke.sh + teardown
├── docs/usage.md
├── docs/deployment.md
└── .github/workflows/
    ├── ci.yml                  # fmt, clippy, tests, dry-run, both smoke jobs
    ├── deploy.yml              # main → production, staging → staging
    └── image.yml               # tag → build and push the image
```

### 7.2 Data documents

One JSON document per box, exactly the shape of contract §2.3 — `name`,
optional `description`, `versions[]`, each version with `providers[]`, each
provider with `name` and `url`, optional checksum and architecture fields.
Documents may be compact or pretty; the build re-renders them.

Rules beyond the contract:

- File path and `name` must match (`data/acme/base.json` → `acme/base`).
- Duplicate `name` across files is an error.
- `data/` must contain at least one catalog, or the build fails.
- Unknown fields are an error (`deny_unknown_fields`), so a typo cannot
  vanish at render time.
- Rendered output must stay within 1 MiB per document and 16 MiB in total.
  This leaves a wide margin under the 64 MiB uncompressed Worker bundle and
  turns an over-large catalog into a build error instead of a deploy failure.
- A provider `url` may be presigned; it is never fetched by the registry.

### 7.3 Build step

`crates/core/build.rs`:

1. Runs `vagrant_registry_core::tree::load_dir(data)` as a build-dependency
   (host target).
2. Collects **all** errors, not the first.
3. On any error: `panic!` with a report naming file, field, and message, so
   `cargo build`, `cargo test`, and CI all fail.
4. On success: renders each catalog with `to_json`, checks the per-document
   and total rendered caps, sorts by name, and writes `OUT_DIR/catalogs.rs`
   with byte-string literals.

`catalogs.rs` does `include!(concat!(env!("OUT_DIR"), "/catalogs.rs"));` and
exposes `pub static CATALOGS: &[(&str, &[u8])]`. It lives in core, so every
consumer gets the same table at compile time.

### 7.4 Core handler

`http.rs` has no runtime crate in its tree:

```rust
pub enum Body { Empty, Json(&'static [u8]), Html(String), Text(&'static str) }
pub struct Reply { pub status: u16, pub headers: &'static [(&'static str, &'static str)], pub body: Body }

pub fn handle(method: &str, path: &str) -> Reply;
pub fn lookup(name: &str) -> Option<&'static [u8]>;
```

`path` is the **raw, still percent-encoded** path from the adapter
(`worker::Request::path`, `axum::extract::OriginalUri`), including any query
string stripped by the adapter. Decoding happens exactly once, in core, so the
adapters cannot disagree about it.

`handle` owns every rule, in this order:

1. Split the raw path on `/`. Drop exactly one leading empty segment, which the
   leading `/` always produces; any remaining empty segment — doubled or
   trailing slash — makes the path invalid. Percent-decode each segment once; a
   decoded segment that itself contains `/` is split further (so
   `/acme%2Fbase` is two segments and resolves).
2. Dispatch on the remaining segments:
   - zero segments (the root `/`) → index page.
   - one segment `healthz` → health.
   - `api`, `v2`, `vagrant`, `{org}`, `{box}` → catalog.
   - `catalogs`, `{org}`, `{box}.json` → the `.json` escape hatch: the `{box}`
     segment must end in `.json`; strip it and serve the same bytes as the
     canonical name.
   - `{org}`, `{box}` → catalog.
   - anything else → 404.
3. `{org}` and `{box}` are validated with `valid_box_name`-style segment rules:
   each non-empty, no `.` or `..` segment, allowed bytes only. A decoded
   segment containing `..` anywhere (`acme/a..b`) is valid; the contract's name
   rule forbids only a whole segment of `..`. Unknown name → 404.
4. Method resolution comes **after** path resolution and applies to every kind:
   anything but `GET`/`HEAD` → 405 with `Allow: GET, HEAD`; `POST /acme/nope` →
   404, `DELETE /healthz` → 405.
5. `GET` on a catalog → the stored bytes; `HEAD` → same status and headers,
   with an empty body. Same for the index and health.

Headers come from the `Reply` table, never from the adapter. `Body::Json` →
`application/json`; `Html` → `text/html; charset=utf-8`; `Text` →
`text/plain; charset=utf-8`; `Empty` → no `Content-Type`. Every reply gets
`nosniff`, and every non-catalog reply gets `Cache-Control: no-store`. A 404
body is the fixed text `not found`, never the requested path.

Both catalog routes and the escape hatch resolve through `lookup` to the same
`&'static [u8]`, so their bodies are byte-identical.

Adapters must not add or reorder decision logic. They only translate the
incoming request and the outgoing `Reply`, and the Worker adapter adds nothing
beyond mapping `Body` to a `worker::Response`.

### 7.5 Adapters

Both adapters are deliberately dumb: build the raw path string, call `handle`,
map `Reply` to a runtime response, add no headers of their own.

- `crates/worker/src/lib.rs`: `#[event(fetch)]` with
  `Router::new().on_async("/", ...).on_async("/*", ...)`. The catch-all means
  every request reaches `handle`, so 404, 405, and `Allow` come from core and
  not from the router's own method matching.
- `crates/server/src/main.rs`: `axum` with a single fallback route
  (`Router::new().fallback(...)`), reading `PORT` (default `8080`) and binding
  `0.0.0.0`, `tower_http::trace` for request logs, `Reply` → response via
  `hyper`'s body types. Graceful shutdown on `SIGTERM`/`SIGINT`.
- `--healthcheck`: the server binary, run with that flag, issues one
  `GET /healthz` against `127.0.0.1:$PORT` over `std::net::TcpStream` (no HTTP
  client dependency), prints the status, and exits `0` only on `200`. The
  Dockerfile's `HEALTHCHECK` uses it, which is why `distroless/cc` needs no
  shell or `curl`. `scripts/container_smoke.sh` polls
  `docker inspect --format '{{.State.Health.Status}}'` until `healthy` or a
  timeout, so the same code path is exercised host-side.
- Neither adapter sets `Content-Length` by hand.

### 7.6 Tests

- Core unit tests: every contract §4.1 validation row, the renderer's
  `default_architecture` behaviour, one `load_dir` case per layout rule, the
  unknown-field and size-cap rejections, a fixture round-trip asserting the
  table holds exactly the rendered bytes, and a `handle` matrix:
  - both catalog route shapes and the `.json` escape hatch, each `GET` and
    `HEAD`, asserting identical bytes across all three;
  - a name containing `..` inside a segment (`acme/a..b`) is served, a `.` or
    `..` segment is not;
  - `%2F` inside a segment resolves to two segments; `%2e%2e` is rejected;
    doubled and trailing slashes are 404;
  - `POST /acme/nope` → 404 and `DELETE /healthz` → 405 with `Allow` (path
    resolves before method);
  - `/catalogs/acme/base` without the suffix is 404; `/healthz` and `/` are 200
    for `HEAD` too, with empty bodies;
  - every `Reply` carries the header table from §7.4, `no-store` everywhere
    except a catalog `GET`/`HEAD`, and no `Content-Type` on an empty body.
- A test in core pointed at the real `data/`: it must load and its paths must
  equal names. Same check as the build, phrased as a test for a readable
  failure.
- Adapter parity: one test per adapter builds the same request set and asserts
  the `Reply` it passes to `handle` has the raw, undecoded path.
- `scripts/smoke.sh <base-url>` is target-agnostic: all three catalog paths
  200 with identical bodies, `HEAD` 200 + `application/json` with no body,
  unknown box 404, encoded traversal 404, `POST` 405 with `Allow`, `/healthz`
  200, index 200. No mutation, so it runs against a dev Worker, a container,
  and a deployed instance.
- `scripts/container_smoke.sh`: `docker build`, `docker run --rm -d -p ...`,
  poll `docker inspect` health, `smoke.sh`, teardown on exit.
- CI: fmt, clippy `-D warnings` on all crates, core tests, `cargo check
  --target wasm32-unknown-unknown` for the Worker, `wrangler deploy
  --dry-run`, a smoke job against `wrangler dev`, and a smoke job against the
  container image. A job that adds an invalid fixture catalog and expects
  `cargo build` to fail guards the build gate.

### 7.7 Deployment

Cloudflare (`.github/workflows/deploy.yml`):

- `wrangler.toml` declares `name`, `main = "build/index.js"`,
  `compatibility_date`, `[build] command = "sh -c 'cd crates/worker &&
  worker-build --release'"` (the root manifest is virtual, so the build must
  run in the package), `[observability] enabled = true`, no `routes` at all,
  and an `[env.staging]` block with `name = "vagrant-registry-staging"`; the
  top-level Worker is `vagrant-registry`.
- `deploy.yml` has two jobs: production on `push` to `main`, staging on `push`
  to `staging`, each in its own GitHub environment with its own secrets, each
  `cloudflare/wrangler-action@v3` (staging with `--env staging`). Both depend
  on the same test job as CI.
- Rollback: `npx wrangler rollback` (and `--env staging` for staging),
  documented.
- Both environments serve their own branch, so staging can hold experimental
  catalogs; `data/` is otherwise shared history.

Docker (`.github/workflows/image.yml`):

- Runs on `push` of a `v*` tag plus `workflow_dispatch`, building
  `linux/amd64` and `linux/arm64` with `docker/build-push-action`, pushing to
  the image name in `docs/deployment.md`.
- Merge to `main` builds but does not push, so tags alone decide releases.
- The image is immutable per digest; rollback is redeploying the previous tag
  or digest.
- Pull requests run `ci.yml` only; a catalog error fails the PR, so a merge
  cannot publish a broken document.

### 7.8 Operations

- Adding a box: copy an existing document, rename, edit, run `cargo test`.
- Local preview: `npx wrangler dev` for the Worker, `cargo run -p
  vagrant-registry-server` or the container smoke script for the server. Both
  need no secrets.
- Logs: Workers observability for the Worker; stdout access logs for the
  server. Errors logged with `worker::console_error!` and `tracing::error!`.
- The catalog is public. Private artifact URLs do not belong in it.
- `docs/deployment.md` covers both targets: Cloudflare secret setup for both
  environments, the workers.dev URLs, the staging branch, the container run
  command, front-proxy headers, TLS termination, and the optional custom
  domain with its workers.dev caveat.

## 8. License

MIT, copyright Roland Moriz. A root `LICENSE` file holds the standard MIT
text, each `Cargo.toml` declares `license = "MIT"`, and `README.md` states it.
Contributions are accepted under the same license; no CLA, no per-file
headers.

## 9. Decisions taken here

- Read-only, JSON in git, embedded at build; production deploys from `main`,
  staging from the `staging` branch, the image from `v*` tags.
- Both Cloudflare environments are workers.dev hostnames; a custom domain is
  optional and config-only.
- One core handler with two thin adapters; no routing or header rule outside
  core.
- The served bytes are rendered at build, never parsed at request time.
- `data/` is validated by `build.rs` and by a `cargo test`, both failing closed.
- Core takes the raw path and decodes once itself; adapters only forward.
- Path resolves before method, so the disposition does not depend on the
  adapter's router.
- Headers come from a static table on `Reply`; adapters add none.
- The `.json` escape hatch from contract §2.4 is served, so existing
  `box_url` values keep working after a migration.
- Limits are on rendered bytes, checked at build with a 16 MiB total budget.
- Documents reject unknown fields.
- Neither adapter sets `Content-Length`.
- The index page and `/healthz` are additions beyond the contract and carry no
  catalog detail beyond a count.
- The four earlier option plans stay as history behind a one-line superseded
  banner pointing at this design; `README.md` is rewritten to describe this
  single implementation.
- The container healthcheck is a `--healthcheck` flag on the binary, so the
  image needs no shell tooling.

## 10. Left to the builder

Test names and fixture layout, the exact panic and CLI wording in `build.rs`,
the HTML of the index page, the `docs/` section order, logging detail, the
image name and registry, and base image choice within the distroless
constraint.
