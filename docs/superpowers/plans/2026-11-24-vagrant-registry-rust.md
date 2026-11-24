# Read-Only Vagrant Registry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve a validated, build-time-embedded catalog of Vagrant boxes from one core handler behind a Cloudflare Worker and an axum container, with no write path.

**Architecture:** Catalogs are JSON files in `data/<org>/<box>.json`. `crates/core/build.rs` validates and renders every document at compile time into a static `&[(&str, &[u8])]` table that the core crate includes. `core::http::handle(method, raw_path) -> Reply` owns every dispatch, status, and header rule. `crates/worker` and `crates/server` only translate to and from the runtime.

**Tech Stack:** Rust 1.91, edition 2024, `serde`, `serde_json`, `url`, `worker` 0.9, `axum` 0.8, `tokio`, `tower-http`, Wrangler 4, `worker-build` 0.9, Docker (`distroless/cc`).

**Spec:** `../specs/2026-11-24-vagrant-registry-rust-design.md`, with the shared contract in `../specs/2026-11-13-vagrant-registry-design.md`.

## Global Constraints

- Rust 1.91 or newer, edition 2024; `wasm32-unknown-unknown` for the Worker.
- Workspace: virtual root `Cargo.toml`; crates `vagrant-registry-core` (pure, no runtime crate in its dependency tree), `vagrant-registry-worker` (`cdylib`), `vagrant-registry-server` (binary). Every manifest declares `license = "MIT"`.
- Public contract: `HEAD|GET /api/v2/vagrant/{org}/{box}`, `HEAD|GET /{org}/{box}`, and `HEAD|GET /catalogs/{org}/{box}.json`. All three serve identical bytes with `Content-Type: application/json`. `HEAD` returns the same status and headers with an empty body.
- Only catalog `GET`/`HEAD` is cacheable (`public, max-age=60`); everything else is `no-store`. Every response sets `X-Content-Type-Options: nosniff`.
- `data/` paths must equal `name`; limits on **rendered** bytes: 1 MiB per document (`1_048_576`), 16 MiB total (`16_777_216`). Unknown fields are errors.
- `default_architecture` is emitted as `false` for architecture-aware non-defaults and omitted for legacy providers.
- Neither adapter sets `Content-Length`. Adapters add no headers and no routing decisions.
- No D1, KV, R2, Durable Objects, admin API, auth, or YAML.
- Production Worker `vagrant-registry`, staging Worker `vagrant-registry-staging`; both on workers.dev, no `routes` block.

## Review Focus

1. **A name with dots inside a segment.** `acme/a..b` is valid and must be served; only whole `.`/`..` segments 404. Covered in Task 5.
2. **Path decoding done twice or not at all.** `/acme%2Fbase` must resolve, `/api/v2/vagrant/acme/%2e%2e` must 404, and core must receive the raw path. Covered in Tasks 5 and 7–8.
3. **Method before path.** `POST /acme/nope` is 404, `DELETE /healthz` is 405 with `Allow: GET, HEAD`. Covered in Task 5.
4. **A catalog that gets through validation but breaks Vagrant** — architecture-aware provider without `default_architecture`, unquoted-version-shaped strings, name/path mismatch. Covered in Tasks 1–3.
5. **HEAD leaking a body or losing `Content-Type`**, including on the adapters. Covered in Tasks 5, 7–8 and the smoke script in Task 9.

---

### Task 1: Workspace skeleton, catalog model, validation

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `rustfmt.toml`
- Create: `crates/core/Cargo.toml`, `crates/core/src/lib.rs`, `crates/core/src/model.rs`, `crates/core/src/version.rs`, `crates/core/src/validate.rs`
- Test: inline `#[cfg(test)]` modules in `version.rs` and `validate.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:

```rust
// model.rs — all structs derive Debug, Clone, PartialEq, Eq, Serialize, Deserialize and carry #[serde(deny_unknown_fields)]
pub struct Catalog { pub name: String, pub description: String, pub versions: Vec<Version> }
pub struct Version { pub version: String, pub providers: Vec<Provider> }
pub struct Provider { pub name: String, pub url: String, pub checksum: String, pub checksum_type: String, pub architecture: String, pub default_architecture: Option<bool> }

// version.rs
pub fn valid_gem_version(s: &str) -> bool;

// validate.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError { pub field: String, pub message: String }
pub fn validate(c: &Catalog) -> Vec<ValidationError>;
pub fn valid_box_name(s: &str) -> bool;
pub fn valid_provider_name(s: &str) -> bool;
pub fn valid_architecture(s: &str) -> bool;
```

- [ ] **Step 1: Create the workspace files**

Root `Cargo.toml` is virtual and lists all three members. `crates/core/Cargo.toml` has `license = "MIT"`, edition 2024, `rust-version = "1.91"`, and dependencies `serde` (derive), `serde_json`, `url`, plus `[dev-dependencies] tempfile`.

`rust-toolchain.toml` pins channel `1.91` and target `wasm32-unknown-unknown`. `.gitignore` covers `/target`, `/build`, `/node_modules`, `/.wrangler`, `/.dev.vars`, `/dist`, `*.db*`, `.DS_Store`. `rustfmt.toml` sets `edition = "2024"`.

`crates/core/src/lib.rs` declares `model`, `validate`, and `version`; later tasks add their own module declarations.

Run: `cargo check -p vagrant-registry-core`
Expected: PASS with empty modules.

- [ ] **Step 2: Write the failing version and validation tests**

`version.rs` tests: accept `0`, `1`, `1.0`, `1.2.0`, `1.0.0.beta1`, `10.20.30`; reject `""`, `v1.2.0`, `1.2.0+build`, `1..0`, `.1`, `1.`, `1-2`, `1.0 `.

`validate.rs` tests use a `base()` fixture (name `acme/base`, one `1.0.0` version, one virtualbox/sha256/amd64/default provider) and cover every rule:

```rust
#[test] fn accepts_valid_catalog() { assert!(validate(&base()).is_empty()); }
#[test] fn aggregates_all_errors() { /* bad name + bad version + ftp url → len() >= 3 */ }
#[test] fn rejects_bad_names() { /* "acme", "acme/base/x", "/base", "acme/", ".acme/base", "acme/..", "ac me/base" */ }
#[test] fn rejects_long_description_and_duplicate_or_empty_versions() {}
#[test] fn rejects_empty_and_oversized_provider_lists() {}   // 0 providers, 101 providers
#[test] fn rejects_bad_provider_name_and_url_userinfo() {}    // "VirtualBox", "https://u:p@h/x.box", "ftp://", hostless
#[test] fn rejects_half_and_mismatched_checksums() {}         // checksum without type, type without checksum, wrong length, non-hex
#[test] fn rejects_bad_architecture_and_default_without_architecture() {}
#[test] fn rejects_duplicate_provider_identity() {}            // same (name, architecture) twice
#[test] fn rejects_two_defaults_for_one_provider_name() {}
#[test] fn accepts_legacy_provider_without_architecture_or_default() {}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p vagrant-registry-core`
Expected: FAIL, `validate` and `valid_gem_version` not found.

- [ ] **Step 4: Implement the model and validators**

`valid_gem_version`: split on `.`; first part non-empty all ASCII digits; every later part non-empty all ASCII alphanumeric.

`valid_box_name`: exactly one `/`; both segments non-empty, first byte ASCII alphanumeric, remaining bytes `[A-Za-z0-9._-]`, neither segment `.` or `..`.

`valid_provider_name`: first byte `a..=z`, rest `[a-z0-9_]`.

`valid_architecture`: non-empty, bytes `[A-Za-z0-9_]`.

`validate` never returns early; it pushes one `ValidationError` per failure, including: description over 4096 bytes; no versions; duplicate versions; invalid version strings; provider count 0 or >100; URL via `url::Url::parse` requiring `http`/`https`, a host, empty username, and no password; checksum pairing, allowed types (`md5|sha1|sha256|sha384|sha512`), hex length `{32,40,64,96,128}` and hex bytes; architecture charset; duplicate `(name, architecture)`; `default_architecture == Some(true)` without architecture; more than one `true` per provider name.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p vagrant-registry-core && cargo fmt --all -- --check`
Expected: PASS, no formatting diff.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml rust-toolchain.toml rustfmt.toml .gitignore crates/core
git commit -m "feat: add catalog model and validation"
```

---

### Task 2: Canonical rendering

**Files:**
- Create: `crates/core/src/render.rs`
- Modify: `crates/core/src/lib.rs`

**Interfaces:**
- Consumes: `model::*`, `validate::*`.
- Produces:

```rust
#[derive(Debug)]
pub enum RenderError { Invalid(Vec<ValidationError>), Json(serde_json::Error) }
pub fn normalize(c: &Catalog) -> Catalog;
pub fn to_json(c: &Catalog) -> Result<String, RenderError>;
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn renders_two_spaces_and_trailing_newline()
#[test] fn emits_false_when_architecture_has_no_default()      // Some/None architecture + None default
#[test] fn emits_true_for_default_architecture()
#[test] fn omits_default_architecture_for_legacy_provider()     // empty architecture, None default
#[test] fn omits_empty_description_and_checksums()
#[test] fn rejects_invalid_catalog()                            // Err(RenderError::Invalid)
```

Each test builds a catalog with a helper taking `(architecture, default)`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p vagrant-registry-core render`
Expected: FAIL, missing functions.

- [ ] **Step 3: Implement `normalize` and `to_json`**

`normalize` clones; per provider it sets `default_architecture = None` when `architecture` is empty, otherwise fills `None` with `Some(false)`. `to_json` validates first, returns `RenderError::Invalid` when non-empty, then `serde_json::to_string_pretty` of the normalized catalog plus one `\n`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p vagrant-registry-core render`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/render.rs crates/core/src/lib.rs
git commit -m "feat: render catalogs as canonical JSON"
```

---

### Task 3: Directory loading, caps, and table generation

**Files:**
- Create: `crates/core/src/tree.rs`
- Modify: `crates/core/src/lib.rs`

**Interfaces:**
- Consumes: `model::Catalog`, `render::{normalize, to_json}`, `validate::ValidationError`.
- Produces:

```rust
pub const MAX_DOCUMENT_BYTES: usize = 1_048_576;
pub const MAX_TOTAL_BYTES: usize = 16_777_216;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError { pub file: String, pub errors: Vec<ValidationError> }

pub fn load_dir(root: &std::path::Path) -> Result<Vec<(String, Catalog)>, Vec<LoadError>>;
pub fn to_table(catalogs: &[(String, Catalog)]) -> Result<String, Vec<LoadError>>;
```

`to_table` output has this exact shape (byte-string literals, escaped, sorted by name):

```rust
pub static CATALOGS: &[(&str, &[u8])] = &[("acme/base", b"...")];
pub static INDEX: &[(&str, &str, usize)] = &[("acme/base", "Acme base image", 2)];
```

- [ ] **Step 1: Write the failing tests**

Use `tempfile::TempDir` fixtures. Cover: empty root → one `LoadError` with field `data`; wrong extension ignored; malformed JSON → error naming the file; a numeric `version` (JSON `1.0` unquoted) → error; an unknown top-level field → error (serde); path/name mismatch → error field `name`; duplicate name across files → error; `.`/`..` path segments rejected; a document over `MAX_DOCUMENT_BYTES` after rendering → error field `rendered_bytes`; combined total over `MAX_TOTAL_BYTES` → one error; valid tree returns entries sorted by name. `to_table` test asserts the literal prefix `pub static CATALOGS: &[(&str, &[u8])] = &[`, that entries are sorted, that `\` and `"` in descriptions are escaped, and that `to_table(&load_dir(valid)?)` output compiles as Rust.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p vagrant-registry-core tree`
Expected: FAIL, `load_dir` not found.

- [ ] **Step 3: Implement `load_dir` and `to_table`**

`load_dir` walks recursively with `std::fs::read_dir`, collects `*.json`, parses with `serde_json`, derives the expected name from the relative path without `.json`, checks equality, tracks duplicate names, runs `validate`, renders with `to_json`, and enforces both caps. Return all errors, sorted by file. `to_table` renders again into `b"..."` literals (`\\`, `\"`, `\n` escaped), sums nothing, and sorts by name.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p vagrant-registry-core tree`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/tree.rs crates/core/src/lib.rs
git commit -m "feat: load and validate the catalog directory"
```

---

### Task 4: Build step, included table, seed catalog

**Files:**
- Create: `crates/core/build.rs`, `crates/core/src/catalogs.rs`, `data/acme/base.json`, `data/README.md`, `crates/core/tests/data_dir.rs`
- Modify: `crates/core/Cargo.toml` (add `[build-dependencies] vagrant-registry-core = { path = "." }`), `crates/core/src/lib.rs`

**Interfaces:**
- Consumes: `tree::{load_dir, to_table}`.
- Produces: `catalogs::CATALOGS`, `catalogs::INDEX`.

- [ ] **Step 1: Write the failing test**

`tests/data_dir.rs`: load `concat!(env!("CARGO_MANIFEST_DIR"), "/../../data")`, assert `Ok`, assert not empty, assert every entry name equals its file path stem, and assert `CATALOGS` contains each name exactly once.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p vagrant-registry-core --test data_dir`
Expected: FAIL, `data/` missing.

- [ ] **Step 3: Add the seed catalog and the build script**

`data/acme/base.json` is the contract §2.3 example: two virtualbox architectures (`amd64` default true, `arm64` default false) and one libvirt entry, both virtualbox entries carrying distinct 64-character hex sha256 values. Add a second seed `data/acme/a..b.json` (name `acme/a..b`, one legacy arch-less provider) so the dotted-name case is exercised end to end. `data/README.md` states the path rule, quoting and limit rules, and the `cargo test` command.

`build.rs`: `println!("cargo:rerun-if-changed=../../data")`, run `load_dir`, on errors `panic!` with one `file: field: message` line per error, otherwise write `to_table` output to `OUT_DIR/catalogs.rs`. `catalogs.rs` includes that file and re-exports both statics; `lib.rs` declares the module. The self-referencing `[build-dependencies] vagrant-registry-core = { path = "." }` is intentional: Cargo compiles the library for the host and hands it to the build script.

- [ ] **Step 4: Verify the gate fails closed**

Append a copy of `data/acme/base.json` as `data/acme/broken.json` without editing its `name`, run `cargo test -p vagrant-registry-core --test data_dir`
Expected: FAIL, panic naming `data/acme/broken.json` and field `name`. Delete the copy.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p vagrant-registry-core && cargo build -p vagrant-registry-core`
Expected: PASS, generated table present.

- [ ] **Step 6: Commit**

```bash
git add crates/core/build.rs crates/core/src/catalogs.rs crates/core/tests/data_dir.rs crates/core/Cargo.toml crates/core/src/lib.rs data
git commit -m "feat: validate and embed catalogs at build time"
```

---

### Task 5: Path grammar, dispatch, methods, headers

**Files:**
- Create: `crates/core/src/http.rs`
- Modify: `crates/core/src/lib.rs`

**Interfaces:**
- Consumes: `validate::*`, `catalogs::CATALOGS`, `catalogs::INDEX`.
- Produces:

```rust
pub enum Body { Empty, Json(&'static [u8]), Html(String), Text(String) }
pub struct Reply { pub status: u16, pub headers: &'static [(&'static str, &'static str)], pub body: Body }
pub enum Target { Index, Health, Catalog(String), Missing }

pub fn percent_decode(s: &str) -> Option<String>;   // None on bad escape or invalid UTF-8
pub fn request_path(uri: &str) -> &str;             // drops "?query" and "#fragment"
pub fn parse_target(raw_path: &str) -> Target;
pub fn lookup(name: &str) -> Option<&'static [u8]>;
pub fn handle(method: &str, raw_path: &str) -> Reply;
pub fn html_escape(s: &str) -> String;
```

Path normalisation lives in core, not in the adapters, because the Worker crate only compiles for `wasm32` and host-side tests cannot reach it.

- [ ] **Step 1: Write the failing tests**

Table-driven over `handle`, asserting status, headers, and body kind:

- `/`, `/healthz`, `/acme/base`, `/api/v2/vagrant/acme/base`, `/catalogs/acme/base.json` → 200.
- The three catalog forms return the identical `&'static [u8]` pointer and `Content-Type: application/json`; `HEAD` gives the same status/headers and `Body::Empty` for all five 200s.
- `/acme/a..b` → 200 (seeded in Task 4); `/acme/..` and `/acme/.` → 404.
- `/acme%2Fbase` → 200; `/api/v2/vagrant/acme/%2e%2e` → 404; `/acme//base` and `/acme/base/` → 404; `//` → 404.
- `/catalogs/acme/base` → 404; `/catalogs/acme/base.json.json` → 404.
- `/acme/nope` → 404 with `Body::Text` equal to `not found`, no path echo.
- `POST /acme/nope` → 404; `POST /acme/base` → 405 with `Allow: GET, HEAD`; `DELETE /healthz` → 405.
- Every reply has `X-Content-Type-Options: nosniff`; `Cache-Control` is `public, max-age=60` only for catalog `GET`/`HEAD`, else `no-store`; `Body::Empty` has no `Content-Type`.
- `/healthz` body is `ok {catalog_count}\n`; `/` is HTML containing each name, its escaped description, and its version count.
- `percent_decode` rejects `%2`, `%zz`, and invalid UTF-8 bytes; `request_path` drops query and fragment and leaves encoding untouched.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p vagrant-registry-core http`
Expected: FAIL, missing items.

- [ ] **Step 3: Implement the grammar and the handler**

`percent_decode` walks bytes, turns `%XX` into a byte, then `String::from_utf8`. `request_path` splits at the first `?` and then at the first `#`.

`parse_target`: split on `/`, drop exactly one leading empty segment, decode each segment, then re-split any decoded `/`. Any empty segment → `Missing`. Zero segments → `Index`; one `healthz` → `Health`; five starting `api`,`v2`,`vagrant` → catalog when both name segments validate; three starting `catalogs` with the third ending in `.json` → catalog on the stripped name; two valid segments → catalog; else `Missing`. A catalog name that `lookup` misses is still `Catalog`, resolved later.

`handle`: build the target, then apply method resolution (`GET`/`HEAD` only, else 405 with `Allow`), then map to replies; resolve names through `lookup`; 404 body is `Text("not found")`. Header sets are `'static` constants, one per (body kind, cacheability) pair, chosen inside `handle`.

`html_escape` escapes `&`, `<`, `>`, `"`, `'`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p vagrant-registry-core http && cargo fmt --all -- --check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/http.rs crates/core/src/lib.rs
git commit -m "feat: add path grammar, dispatch, and reply headers"
```

---

### Task 6: Worker scaffold and adapter

**Files:**
- Create: `wrangler.toml`, `package.json`, `crates/worker/Cargo.toml`, `crates/worker/src/lib.rs`
- Modify: `Cargo.toml`, `.gitignore`

**Interfaces:**
- Consumes: `vagrant_registry_core::http::{request_path, handle, Reply, Body}`.
- Produces: the deployed Worker, `GET|HEAD` on all routes.

- [ ] **Step 1: Create the Wrangler and package files**

`wrangler.toml`: `name = "vagrant-registry"`, `main = "build/index.js"`, `compatibility_date = "2026-11-24"`, `[build] command = "sh -c 'cd crates/worker && worker-build --release'"`, `[observability] enabled = true`, and an `[env.staging]` block with `name = "vagrant-registry-staging"` plus its own `[env.staging.build]`. No `routes`.

`package.json`: private, scripts `dev` (`wrangler dev`), `deploy` (`wrangler deploy`), `deploy:staging` (`wrangler deploy --env staging`), devDependency `wrangler ^4.0.0`.

- [ ] **Step 2: Implement the adapter**

`#[event(fetch)]` builds `Router::new().on_async("/", handler).on_async("/*", handler)`; `handler` takes `req.method().to_string()` and `request_path(&req.path())`, calls `handle`, and maps `Body::{Empty,Json,Html,Text}` to `Response::empty()` or `Response::from_bytes` with the `Reply` status and headers. No header is added or removed, and the raw path string is passed on undecoded.

Run: `cargo check -p vagrant-registry-worker --target wasm32-unknown-unknown`
Expected: PASS.

- [ ] **Step 3: Run it against `wrangler dev`**

```bash
cargo install worker-build --version 0.9.0 --locked
npm install
npx wrangler dev --port 8787
```

`curl -si 'localhost:8787/acme/base?x=1' | head -1` → `HTTP/1.1 200`; `curl -sI localhost:8787/api/v2/vagrant/acme/base | grep -i content-type` → `application/json`; `curl -s -o /dev/null -w '%{http_code}' 'localhost:8787/api/v2/vagrant/acme/%2e%2e'` → `404`.

- [ ] **Step 4: Commit**

```bash
git add wrangler.toml package.json package-lock.json crates/worker Cargo.toml .gitignore
git commit -m "feat: serve the registry from a Cloudflare Worker"
```

---

### Task 7: Server adapter, healthcheck, image

**Files:**
- Create: `crates/server/Cargo.toml`, `crates/server/src/main.rs`, `Dockerfile`, `.dockerignore`
- Modify: `Cargo.toml`, `crates/core/Cargo.toml` (no change expected)

**Interfaces:**
- Consumes: `http::{request_path, handle, Reply, Body}`.
- Produces: `vagrant-registry-server` binary; `--healthcheck` flag; container image.

- [ ] **Step 1: Write the failing adapter test**

Native crate, so this runs on the host. Test: `handle_reply` maps a `Reply` with `Body::Empty` and `Content-Type: application/json` to an empty body carrying that header and no `Content-Length`; a `Reply` with `Body::Json(b"{}")` keeps the bytes byte-for-byte and the given status; `request_path` is applied to `OriginalUri` so `/acme/base?x=1` yields `/acme/base` while `/acme%2Fbase` stays encoded.

- [ ] **Step 2: Implement the adapter**

`Router::new().fallback(handler)`; `handler` extracts `Method` and `OriginalUri`, applies `request_path`, calls `handle`, builds the response from `Reply` in a `handle_reply(reply) -> Response` helper. Bind `0.0.0.0:$PORT` (default `8080`), `tower_http::trace::TraceLayer`, graceful shutdown on `SIGTERM`/`SIGINT`. `--healthcheck` performs a `std::net::TcpStream` request for `/healthz` on `127.0.0.1:$PORT`, prints the status line, exits `0` only on `200`.

- [ ] **Step 3: Write the Dockerfile**

Two stages: `rust:1.91-slim` builder (`cargo build --release -p vagrant-registry-server`), `gcr.io/distroless/cc-debian12:nonroot` runtime copying the binary to `/app/vagrant-registry-server`, `EXPOSE 8080`, `HEALTHCHECK CMD ["/app/vagrant-registry-server", "--healthcheck"]`. `.dockerignore` covers `/target`, `/node_modules`, `/.git`, `/docs`, `/dist`, `/.wrangler`, but **not** `/data` — `build.rs` needs it in the image build.

- [ ] **Step 4: Verify**

```bash
cargo test -p vagrant-registry-server
docker build -t vagrant-registry:dev .
docker run --rm -d -p 18080:8080 --read-only --name vr-dev vagrant-registry:dev
curl -si localhost:18080/acme/base | head -1
docker inspect --format '{{.State.Health.Status}}' vr-dev
curl -sI localhost:18080/catalogs/acme/base.json | grep -i content-type
docker rm -f vr-dev
```

Expected: 200, `healthy`, `application/json`, container removed.

- [ ] **Step 5: Commit**

```bash
git add crates/server Cargo.toml Dockerfile .dockerignore
git commit -m "feat: serve the registry from a distroless container"
```

---

### Task 8: Shared smoke script

**Files:**
- Create: `scripts/smoke.sh`, `scripts/container_smoke.sh`

**Interfaces:**
- Consumes: a running registry at `$1` (default `http://127.0.0.1:8787`).
- Produces: exit `0` on a passing run.

- [ ] **Step 1: Write `scripts/smoke.sh`**

`set -euo pipefail`; `fail()` prints to stderr and exits 1. Checks, using `curl -sS`:

- `/`, `/healthz`, `/acme/base`, `/api/v2/vagrant/acme/base`, `/catalogs/acme/base.json` are 200.
- `cmp` the three catalog bodies pairwise.
- `HEAD` on `/`, `/healthz` and each catalog form is 200; a catalog `HEAD` header block contains `content-type: application/json`; `curl -sSI -o /dev/null -w '%{size_download}'` is `0` for each.
- `/acme/a..b` 200; `/acme/nope` 404; `/api/v2/vagrant/acme/%2e%2e` 404; `/acme//base` 404; `/catalogs/acme/base` 404.
- `POST /acme/base` 405 with `allow: GET, HEAD`.
- Every 200/404 carries `x-content-type-options: nosniff`.

- [ ] **Step 2: Write `scripts/container_smoke.sh`**

Builds `vagrant-registry:smoke`, runs it on `18081:8080` with `--read-only`, polls `docker inspect` health for 60 seconds, runs `smoke.sh http://127.0.0.1:18081`, and removes the container in a `trap`.

- [ ] **Step 3: Verify against both targets**

```bash
npx wrangler dev --port 8787 &
./scripts/smoke.sh
./scripts/smoke.sh http://127.0.0.1:8787
./scripts/container_smoke.sh
```

Expected: `ok:` line from each, no failures. Make both scripts executable.

- [ ] **Step 4: Commit**

```bash
git add scripts/smoke.sh scripts/container_smoke.sh
git commit -m "test: add shared contract smoke script"
```

---

### Task 9: CI

**Files:**
- Create: `.github/workflows/test.yml`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: everything above.
- Produces: a red build on contract or catalog regressions; `test.yml` is reusable via `workflow_call` so deploys run the same gate.

- [ ] **Step 1: Write the reusable gate**

`.github/workflows/test.yml` with `on: workflow_call` and `workflow_dispatch`.

Job `test`: checkout, `dtolnay/rust-toolchain@stable` with `1.91`, `wasm32-unknown-unknown`, `rustfmt`, `clippy`; `Swatinem/rust-cache`; `cargo fmt --all -- --check`; `cargo clippy -p vagrant-registry-core -p vagrant-registry-server --all-targets -- -D warnings`; `cargo clippy -p vagrant-registry-worker --target wasm32-unknown-unknown -- -D warnings`; `cargo test -p vagrant-registry-core -p vagrant-registry-server`; `cargo check -p vagrant-registry-worker --target wasm32-unknown-unknown`; setup-node 22; `npm ci`; `cargo install worker-build --version 0.9.0 --locked`; `npx wrangler deploy --dry-run --outdir dist`.

The Worker crate is excluded from the host test and clippy steps because `worker` only compiles for `wasm32`.

Job `build-gate`: copy `data/acme/base.json` to `data/gate/gate.json` unchanged and expect `cargo build -p vagrant-registry-core` to fail on the name/path mismatch; then set its `name` to `gate/gate` and expect success; remove the copy afterwards.

Job `smoke-worker`: install `worker-build` and `npm ci`, start `npx wrangler dev --port 8787` in the background, wait for `/healthz`, run `./scripts/smoke.sh http://127.0.0.1:8787`, kill the server `if: always()`.

Job `smoke-container`: `./scripts/container_smoke.sh`, with `if: always()` teardown.

- [ ] **Step 2: Write the trigger workflow**

`.github/workflows/ci.yml`: `on: pull_request` and `push` with `branches-ignore: [main, staging]`; one job `gate` that does `uses: ./.github/workflows/test.yml`.

- [ ] **Step 3: Verify locally**

Run every command from jobs `test` and `build-gate` in order.
Expected: all pass; the gate fails exactly while the copied name is `acme/base` and passes after the rewrite.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/test.yml .github/workflows/ci.yml
git commit -m "ci: gate catalogs, tests, and both adapters"
```

---

### Task 10: Deploy workflows, docs, superseded banners

**Files:**
- Create: `.github/workflows/deploy.yml`, `.github/workflows/image.yml`, `docs/usage.md`, `docs/deployment.md`
- Modify: `README.md`, the four files in `docs/superpowers/plans/`, `docs/superpowers/specs/2026-11-24-vagrant-registry-rust-design.md` (status already `approved`)

**Interfaces:**
- Consumes: the finished system.
- Produces: `main` → production, `staging` → staging, `v*` tags → image; a runbook.

- [ ] **Step 1: Write `deploy.yml`**

`on: push: branches: [main, staging]` plus `workflow_dispatch`. One job `gate` doing `uses: ./.github/workflows/test.yml`, then two deploy jobs: `production` (`if: github.ref == 'refs/heads/main'`, environment `production`) and `staging` (`refs/heads/staging`, environment `staging`), each `cloudflare/wrangler-action@v3` with `apiToken: ${{ secrets.CLOUDFLARE_API_TOKEN }}`, `accountId: ${{ secrets.CLOUDFLARE_ACCOUNT_ID }}`, and `command: deploy --env staging` only for staging.

- [ ] **Step 2: Write `image.yml`**

`on: push: tags: ['v*']` plus `workflow_dispatch`; `permissions: contents: read, packages: write`; `docker/setup-qemu-action`, `setup-buildx-action`, `login-action` to `ghcr.io` with `GITHUB_TOKEN` and actor, `build-push-action` for `linux/amd64,linux/arm64` pushing `ghcr.io/rmoriz/vagrant-registry` tagged with the ref name and `sha`.

- [ ] **Step 3: Write the docs**

`docs/usage.md`: adding a box (copy, rename, path rule, `cargo test`), `VAGRANT_SERVER_URL` and `box_server_url` client setup, the `.json` escape hatch, local preview, catalog URL persistence, caching.

`docs/deployment.md`: Cloudflare secret creation per environment, the two workers.dev URLs, the `staging` branch, rollback (`npx wrangler rollback`), pulling `ghcr.io/rmoriz/vagrant-registry`, the container run command with `--read-only` and a front proxy that passes `Host` and sets TLS, the optional custom domain plus the "keep workers.dev on" caveat.

- [ ] **Step 4: Rewrite `README.md`, banner the old plans**

README: what it is, the three served paths, how to add a box, both deploy paths, limits, non-goals, license. Prepend to each `docs/superpowers/plans/2026-11-13-*.md`: `> **Superseded** by [the 2026-11-24 Rust design and its plan]. Kept as history.`

- [ ] **Step 5: Verify**

```bash
cargo test -p vagrant-registry-core -p vagrant-registry-server
npx wrangler deploy --dry-run --outdir dist
grep -L "Superseded" docs/superpowers/plans/2026-11-13-*.md
```

Expected: tests pass, dry run succeeds, `grep -L` prints nothing.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows README.md docs
git commit -m "docs: add deploy workflows, runbooks, and supersede the old plans"
```

---

## Self-review checklist

- [ ] Every spec §7.2–7.8 section maps to a task: data rules (3), caps (3), build gate (4), grammar and headers (5), both adapters (6, 7), healthcheck (7), smoke (8), CI (9), deploy/docs (10).
- [ ] Both traversal shapes (`a..b` served, `..` segment 404) are tested in Task 5 and smoke in Task 8.
- [ ] `%2F` resolution and `%2e%2e` rejection appear in Tasks 5, 7, 8.
- [ ] HEAD has no body and keeps `Content-Type` on every 200 (Task 5, 8).
- [ ] `default_architecture` false/omitted behaviour is tested in Task 2.
- [ ] Name/path mismatch fails the build (Tasks 3, 4, 9).
- [ ] No task may set `Content-Length`.
- [ ] Every commit message is imperative and scoped to one task.
