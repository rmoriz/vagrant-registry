# Option D — Cloudflare Workers + D1 Registry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run the same catalog registry as Option C — public catalog API, admin API, admin UI, validation, and YAML import — as a Rust Cloudflare Worker backed by D1, without hosting box binaries.

**Architecture:** One Rust Worker serves both public catalog routes from D1. A pure `vagrant-registry-core` crate owns the catalog model, YAML/JSON parsing, validation, rendering, auth/cookie/CSRF helpers, and server-rendered HTML. The Worker crate is a thin adapter over `worker` + `d1`. Full-document `PUT` replaces a box in one `D1Database::batch` transaction. Admin auth is a Worker secret; the UI adds a double-submit CSRF cookie.

**Tech Stack:** Rust 1.91, `worker = "0.9"` (`d1` feature), `serde`/`serde_json`/`serde_yaml_ng`, `maud`, `constant_time_eq`, `getrandom`, `base64`, `url`, `form_urlencoded`; Cloudflare Workers, D1, Wrangler 4, `worker-build` 0.9.

**Spec:** `../specs/2026-11-13-vagrant-registry-design.md`

## Global Constraints

- Rust 1.91 or newer; `wasm32-unknown-unknown` target; edition 2024.
- Crate layout: root package `vagrant-registry-worker` (`cdylib`), pure library `crates/core` (`vagrant-registry-core`).
- Cloudflare Workers + D1 only. Do not use Durable Objects, KV, R2, or a second database.
- Public contract, unchanged from the spec: `HEAD|GET /api/v2/vagrant/{org}/{box}` and `HEAD|GET /{org}/{box}`. `GET` on both routes returns identical bytes; `HEAD` returns the same status and headers without a body. Both use `Content-Type: application/json`.
- No binary hosting, proxying, upload, or Vagrant Cloud `/api/v1` compatibility.
- Admin token binding: `REGISTRY_ADMIN_TOKEN` (Wrangler secret in production, `.dev.vars` locally). Unset or empty → every admin route returns 404. Wrong token → 401.
- Admin auth accepts `Authorization: Bearer <token>` and Basic `admin:<token>`. Compare tokens in constant time.
- All admin responses use `Cache-Control: no-store`.
- Every admin write is behind auth. UI POSTs additionally require a valid CSRF token.
- CSRF cookie: `registry_csrf`, `Path=/admin`, `HttpOnly`, `SameSite=Strict`, `Secure` when the request scheme is HTTPS.
- Full-document `PUT /api/v1/admin/boxes/{org}/{box}` is atomic. D1 `batch()` is a SQL transaction and rolls the whole sequence back on error.
- Request bodies are limited to 1 MiB (1,048,576 bytes).
- Versions must be quoted YAML strings. Unquoted numeric versions are rejected with an actionable error instead of being silently normalized.
- D1 binding name `DB`; database name `vagrant-registry`; migrations live in `migrations/`.
- Public responses use `Cache-Control: public, max-age=60` and `X-Content-Type-Options: nosniff`.
- Cloudflare may omit `Content-Length` or report `0` on `HEAD`; the required contract is the status code and `Content-Type`. Vagrant does not use `Content-Length` for catalog discovery.
- Provider URLs stay in D1 and point at external artifact storage.

## Review Focus

- **Partial replacement.** A full-document `PUT` that fails validation or hits a D1 error mid-batch must leave the previous box, versions, and providers untouched. D1 `batch()` rolls back the whole sequence; the smoke test proves it.
- **Admin disclosure.** An unset or empty `REGISTRY_ADMIN_TOKEN` must produce 404, while a wrong token produces 401. The response must not reveal whether the binding exists.
- **CSRF bypass.** A UI POST with valid Basic auth but a missing or mismatched CSRF token must return 403 and must not write anything.
- **Concurrent writes.** Two maintainers publishing the same box at once must not produce a mixed catalog. D1 serializes batches; the last complete batch wins, and every response is either the old or the new document.
- **HEAD semantics.** `HEAD` must return 200 and `Content-Type: application/json` without a body. Cloudflare may omit `Content-Length` or report `0`; Vagrant does not use it for discovery.

## File Structure

```text
.
├── Cargo.toml                         # worker package + workspace root
├── rust-toolchain.toml                # 1.91 + wasm32 target
├── wrangler.toml                      # Worker, D1 binding, migrations dir
├── package.json                       # Wrangler devDependency and scripts
├── .dev.vars.example                  # local admin token
├── migrations/
│   └── 0001_init.sql
├── src/
│   ├── lib.rs                         # #[event(fetch)] + Router
│   ├── response.rs                    # JSON/HTML/empty response helpers
│   ├── store.rs                       # D1 queries and batch writes
│   ├── public.rs                      # public catalog handler
│   ├── admin.rs                       # admin auth + JSON API
│   └── ui.rs                          # admin UI routes and cookies
├── crates/core/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── model.rs                   # Catalog, Version, Provider
│       ├── version.rs                 # Gem::Version validation
│       ├── validate.rs                # aggregated validation
│       ├── render.rs                  # canonical JSON
│       ├── yaml.rs                    # YAML import parsing
│       ├── http.rs                    # auth, cookies, CSRF, forms, path guard
│       └── view.rs                    # maud pages
├── boxes.test/
│   └── acme/base.yaml                 # import fixture
├── scripts/
│   ├── import.sh
│   └── smoke.sh
├── docs/usage.md
└── .github/workflows/ci.yml
```

---

### Task 1: Workspace, catalog model, validation, rendering

**Files:**
- Create: `Cargo.toml`
- Create: `crates/core/Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `.gitignore`
- Create: `crates/core/src/lib.rs`
- Create: `crates/core/src/model.rs`
- Create: `crates/core/src/yaml.rs`
- Create: `crates/core/src/version.rs`
- Create: `crates/core/src/validate.rs`
- Create: `crates/core/src/render.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:

```rust
// model.rs
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub versions: Vec<Version>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    #[serde(deserialize_with = "crate::yaml::deserialize_version")]
    pub version: String,
    pub providers: Vec<Provider>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub checksum: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub checksum_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub architecture: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_architecture: Option<bool>,
}

// version.rs
pub fn valid_gem_version(s: &str) -> bool;

// validate.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}
pub fn validate(c: &Catalog) -> Vec<ValidationError>;
pub fn valid_box_name(s: &str) -> bool;
pub fn valid_provider_name(s: &str) -> bool;
pub fn valid_architecture(s: &str) -> bool;

// render.rs
#[derive(Debug)]
pub enum RenderError {
    Invalid(Vec<ValidationError>),
    Json(serde_json::Error),
}
pub fn normalize(c: &Catalog) -> Catalog;
pub fn to_json(c: &Catalog) -> Result<String, RenderError>;
```

`Version::version` uses `crate::yaml::deserialize_version`. Task 1 creates that deserializer together with the model so the crate compiles before YAML document parsing exists. Keep the public field type `String`.

- [ ] **Step 1: Create the workspace skeleton**

`Cargo.toml`:

```toml
[package]
name = "vagrant-registry-worker"
version = "0.1.0"
edition = "2024"
rust-version = "1.91"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
vagrant-registry-core = { path = "crates/core" }
worker = { version = "0.9", features = ["d1"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
js-sys = "0.3"

[workspace]
members = ["crates/core"]
resolver = "2"
```

`crates/core/Cargo.toml`:

```toml
[package]
name = "vagrant-registry-core"
version = "0.1.0"
edition = "2024"
rust-version = "1.91"
publish = false

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml_ng = "0.10"
maud = "0.27"
url = "2"
base64 = "0.22"
constant_time_eq = "0.6"
form_urlencoded = "1"
getrandom = { version = "0.4", features = ["wasm_js"] }
```

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.91"
targets = ["wasm32-unknown-unknown"]
```

`.gitignore`:

```text
/target
/build
/node_modules
/.wrangler
/.dev.vars
/dist
*.db
*.db-shm
*.db-wal
.DS_Store
```

Create `crates/core/src/lib.rs` with:

```rust
pub mod model;
pub mod render;
pub mod validate;
pub mod version;
pub mod yaml;
```

Create `src/lib.rs` as an empty placeholder; Task 4 fills it.

Create `crates/core/src/yaml.rs` with only the version deserializer in this task: `pub fn deserialize_version<'de, D>(deserializer: D) -> Result<String, D::Error>` using `deserialize_any` and a `Visitor` that accepts strings and rejects numeric scalars with `version must be a quoted string; write version: "1.0"`. Task 2 adds `parse_yaml` and its tests.

Run:

```bash
cd /path/to/vagrant-registry
cargo check -p vagrant-registry-core
```

Expected: PASS with empty modules.

- [ ] **Step 2: Write the failing core tests**

In `crates/core/src/version.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::valid_gem_version;

    #[test]
    fn accepts_gem_versions() {
        for v in ["0", "1", "1.0", "1.2.0", "1.0.0.beta1", "10.20.30"] {
            assert!(valid_gem_version(v), "{v} should be valid");
        }
    }

    #[test]
    fn rejects_non_gem_versions() {
        for v in ["", "v1.2.0", "1.2.0+build", "1..0", ".1", "1.", "1-2", "1.0 "] {
            assert!(!valid_gem_version(v), "{v} should be invalid");
        }
    }
}
```

In `crates/core/src/validate.rs`, add a `base()` helper and table-driven tests that cover every row of spec §4.1:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Catalog, Provider, Version};

    fn provider() -> Provider {
        Provider {
            name: "virtualbox".into(),
            url: "https://downloads.example.com/x.box".into(),
            checksum: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            checksum_type: "sha256".into(),
            architecture: "amd64".into(),
            default_architecture: Some(true),
        }
    }

    fn base() -> Catalog {
        Catalog {
            name: "acme/base".into(),
            description: "Acme base image".into(),
            versions: vec![Version { version: "1.0.0".into(), providers: vec![provider()] }],
        }
    }

    #[test]
    fn accepts_valid_catalog() {
        assert!(validate(&base()).is_empty());
    }

    #[test]
    fn rejects_invalid_names() {
        for name in ["acme", "acme/base/extra", "/base", "acme/", ".acme/base", "acme/..", "ac me/base"] {
            let mut c = base();
            c.name = name.into();
            assert!(!validate(&c).is_empty(), "{name} should be invalid");
        }
    }

    #[test]
    fn aggregates_all_errors() {
        let mut c = base();
        c.name = "bad".into();
        c.versions[0].version = "v1".into();
        c.versions[0].providers[0].url = "ftp://example.com/x.box".into();
        assert!(validate(&c).len() >= 3);
    }

    // Add one test per remaining rule: description length, empty/duplicate versions,
    // empty/oversized provider lists, provider name, URL userinfo, checksum pairing,
    // checksum type and length, hex characters, architecture charset, duplicate
    // (name, architecture), default_architecture without architecture, and more than
    // one default per provider name.
}
```

In `crates/core/src/render.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Catalog, Provider, Version};

    fn catalog(default: Option<bool>, architecture: &str) -> Catalog {
        Catalog {
            name: "acme/base".into(),
            description: String::new(),
            versions: vec![Version {
                version: "1.0".into(),
                providers: vec![Provider {
                    name: "virtualbox".into(),
                    url: "https://e/x.box".into(),
                    checksum: String::new(),
                    checksum_type: String::new(),
                    architecture: architecture.into(),
                    default_architecture: default,
                }],
            }],
        }
    }

    #[test]
    fn renders_two_spaces_and_trailing_newline() {
        let json = to_json(&catalog(Some(true), "amd64")).unwrap();
        assert!(json.ends_with("}\n"));
        assert!(json.contains("\n  \"name\": \"acme/base\""));
    }

    #[test]
    fn emits_false_when_architecture_has_no_pointer() {
        let json = to_json(&catalog(None, "amd64")).unwrap();
        assert!(json.contains("\"default_architecture\": false"));
    }

    #[test]
    fn omits_default_architecture_for_legacy_provider() {
        let json = to_json(&catalog(None, "")).unwrap();
        assert!(!json.contains("default_architecture"));
    }

    #[test]
    fn rejects_invalid_catalog() {
        let mut c = catalog(Some(true), "amd64");
        c.name = "bad".into();
        assert!(matches!(to_json(&c), Err(RenderError::Invalid(_))));
    }
}
```

- [ ] **Step 3: Run tests to verify failure**

Run: `cargo test -p vagrant-registry-core`

Expected: FAIL with `cannot find function` / missing `deserialize_version`.

- [ ] **Step 4: Implement the model, version validator, validation, and renderer**

`model.rs`: define the structs exactly as in Interfaces.

`version.rs`:

```rust
pub fn valid_gem_version(s: &str) -> bool {
    let mut parts = s.split('.');
    let Some(first) = parts.next() else { return false };
    if first.is_empty() || !first.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    parts.all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}
```

`validate.rs`:

- `valid_box_name`: exactly one `/`; each segment non-empty; first byte ASCII alphanumeric; remaining bytes `[A-Za-z0-9._-]`; reject `.` and `..` segments.
- `valid_provider_name`: first byte `a..=z`; remaining bytes `[a-z0-9_]`.
- `valid_architecture`: non-empty, all bytes `[A-Za-z0-9_]`; call it only when `architecture` is non-empty, because the field itself is optional.
- `validate` pushes `ValidationError { field, message }` and never returns early.
- URL: `url::Url::parse`; require `http`/`https`, `host().is_some()`, empty username, no password.
- Checksums: both empty or both non-empty; type in `md5|sha1|sha256|sha384|sha512`; expected hex length from the mapping `{md5:32, sha1:40, sha256:64, sha384:96, sha512:128}`; all bytes ASCII hex.
- Versions: at least one; unique set; `valid_gem_version`.
- Providers: at least one and at most 100 per version; identity set of `(name, architecture)`; `default_architecture == Some(true)` requires non-empty architecture; at most one `true` per provider name.
- Description: `description.len() <= 4096` (bytes).

`render.rs`:

- `normalize(c)` clones `c`; for each provider, if `architecture` is empty set `default_architecture = None`, else if `None` set `Some(false)`.
- `to_json`: call `validate(c)`; return `RenderError::Invalid` if non-empty; `serde_json::to_string_pretty(&normalize(c))`; append `"\n"`.

- [ ] **Step 5: Run tests to verify they pass**

Run:

```bash
cargo test -p vagrant-registry-core
cargo fmt --all -- --check
```

Expected: PASS, no formatting diff.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/core rust-toolchain.toml .gitignore src/lib.rs
git commit -m "feat: add catalog model, validation, and canonical JSON"
```

---

### Task 2: YAML parsing for import

**Files:**
- Modify: `crates/core/src/yaml.rs` (add document parsing next to the Task 1 deserializer)

**Interfaces:**
- Consumes: `Catalog`, `Version`, `Provider`, `deserialize_version`.
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
}
pub fn parse_yaml(input: &str) -> Result<Catalog, ParseError>;
```

- [ ] **Step 1: Write the failing tests**

Add to `crates/core/src/yaml.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
name: acme/base
description: Acme base image
versions:
  - version: "1.2.0"
    providers:
      - name: virtualbox
        url: https://downloads.example.com/acme-base-1.2.0-vbox-amd64.box
        checksum_type: sha256
        checksum: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
        architecture: amd64
        default_architecture: true
      - name: libvirt
        url: https://downloads.example.com/acme-base-1.2.0-libvirt.box
        architecture: amd64
        default_architecture: false
  - version: "1.1.0"
    providers:
      - name: virtualbox
        url: https://downloads.example.com/acme-base-1.1.0-vbox.box
"#;

    #[test]
    fn parses_spec_example() {
        let c = parse_yaml(EXAMPLE).unwrap();
        assert_eq!(c.name, "acme/base");
        assert_eq!(c.versions.len(), 2);
        assert_eq!(c.versions[0].providers[0].default_architecture, Some(true));
        assert_eq!(c.versions[1].providers[0].default_architecture, None);
    }

    #[test]
    fn preserves_quoted_version_text() {
        let yaml = "name: acme/base\nversions:\n  - version: \"1.10\"\n    providers:\n      - name: virtualbox\n        url: https://e/x.box\n";
        assert_eq!(parse_yaml(yaml).unwrap().versions[0].version, "1.10");
    }

    #[test]
    fn rejects_unquoted_numeric_version() {
        let yaml = "name: acme/base\nversions:\n  - version: 1.0\n    providers:\n      - name: virtualbox\n        url: https://e/x.box\n";
        let err = parse_yaml(yaml).unwrap_err();
        assert!(err.message.contains("quote"), "{}", err.message);
    }

    #[test]
    fn rejects_unknown_fields() {
        let yaml = "name: acme/base\nname_typo: x\nversions: []\n";
        assert!(parse_yaml(yaml).is_err());
    }

    #[test]
    fn rejects_invalid_yaml() {
        assert!(parse_yaml("name: [").is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p vagrant-registry-core yaml`

Expected: FAIL with missing `parse_yaml`.

- [ ] **Step 3: Implement parsing**

- `parse_yaml` calls `serde_yaml_ng::from_str::<Catalog>(input)` and maps the error to `ParseError { message: err.to_string() }`.
- The `deserialize_version` visitor from Task 1 already provides the quoted-string behavior; do not duplicate it.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p vagrant-registry-core`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/yaml.rs
git commit -m "feat: parse YAML catalogs with quoted version enforcement"
```

---

### Task 3: HTTP utilities and server-rendered views

**Files:**
- Create: `crates/core/src/http.rs`
- Create: `crates/core/src/view.rs`
- Modify: `crates/core/src/lib.rs`

**Interfaces:**
- Consumes: `Catalog`, `ValidationError`, `valid_box_name`.
- Produces:

```rust
// http.rs
pub enum AdminConfig {
    Disabled,
    Enabled(String),
}
pub fn admin_config(binding: Option<&str>) -> AdminConfig;
pub fn admin_authorized(header: Option<&str>, token: &str) -> bool;
pub fn cookie_value(header: Option<&str>, name: &str) -> Option<String>;
pub fn csrf_matches(cookie: Option<&str>, form: Option<&str>) -> bool;
pub fn random_hex(bytes: usize) -> String;
pub fn parse_form(body: &str) -> std::collections::HashMap<String, String>;
pub fn suspicious_path(path: &str) -> bool;

// view.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxSummary {
    pub org: String,
    pub name: String,
    pub description: String,
    pub versions: i32,
    pub updated_at: String,
}
impl BoxSummary {
    pub fn full_name(&self) -> String;
}
pub fn index_page(boxes: &[BoxSummary]) -> String;
pub fn box_page(catalog: &Catalog, csrf: &str) -> String;
pub fn error_page(status: u16, messages: &[String]) -> String;
```

- [ ] **Step 1: Write the failing HTTP utility tests**

Add to `crates/core/src/http.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_config_disables_missing_or_empty() {
        assert!(matches!(admin_config(None), AdminConfig::Disabled));
        assert!(matches!(admin_config(Some("")), AdminConfig::Disabled));
        assert!(matches!(admin_config(Some("x")), AdminConfig::Enabled(_)));
    }

    #[test]
    fn accepts_bearer_and_basic() {
        assert!(admin_authorized(Some("Bearer test-token"), "test-token"));
        assert!(admin_authorized(Some("bearer test-token"), "test-token"));
        // base64("admin:test-token")
        assert!(admin_authorized(Some("Basic YWRtaW46dGVzdC10b2tlbg=="), "test-token"));
    }

    #[test]
    fn rejects_wrong_or_missing_credentials() {
        assert!(!admin_authorized(None, "test-token"));
        assert!(!admin_authorized(Some("Bearer wrong"), "test-token"));
        assert!(!admin_authorized(Some("Basic YWRtaW46d3Jvbmc="), "test-token"));
        assert!(!admin_authorized(Some("Basic dXNlcjp0ZXN0LXRva2Vu"), "test-token"));
    }

    #[test]
    fn reads_named_cookie_only() {
        let header = Some("a=1; registry_csrf=abc; registry_csrf_extra=no");
        assert_eq!(cookie_value(header, "registry_csrf"), Some("abc".into()));
        assert_eq!(cookie_value(header, "missing"), None);
    }

    #[test]
    fn csrf_requires_equal_nonempty_values() {
        assert!(csrf_matches(Some("abc"), Some("abc")));
        assert!(!csrf_matches(None, Some("abc")));
        assert!(!csrf_matches(Some("abc"), None));
        assert!(!csrf_matches(Some(""), Some("")));
        assert!(!csrf_matches(Some("abc"), Some("abd")));
    }

    #[test]
    fn parses_urlencoded_forms() {
        let form = parse_form("csrf=a%2Bb&version=2.0.0&providers_json=%5B%5D");
        assert_eq!(form.get("csrf").unwrap(), "a+b");
        assert_eq!(form.get("version").unwrap(), "2.0.0");
        assert_eq!(form.get("providers_json").unwrap(), "[]");
    }

    #[test]
    fn random_hex_has_requested_length() {
        let s = random_hex(32);
        assert_eq!(s.len(), 64);
        assert!(s.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn flags_encoded_traversal() {
        assert!(suspicious_path("/acme/%2e%2e"));
        assert!(suspicious_path("/../etc/passwd"));
        assert!(!suspicious_path("/acme/base"));
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p vagrant-registry-core http`

Expected: FAIL with missing functions.

- [ ] **Step 3: Implement the HTTP utilities**

- `admin_config`: empty/missing → `Disabled`; otherwise `Enabled(token.to_owned())`.
- `admin_authorized`:
  - Split the header at the first space. Scheme comparison is ASCII case-insensitive.
  - `Bearer`: constant-time compare the supplied token with `token`.
  - `Basic`: `base64::engine::general_purpose::STANDARD.decode`; split the decoded bytes at the first `:`; username must be `admin`; constant-time compare the password.
- `cookie_value`: split on `;`, trim, `split_once('=')`, exact name match.
- `csrf_matches`: both `Some`, non-empty, `constant_time_eq::constant_time_eq`.
- `random_hex`: `getrandom::fill` into `vec![0u8; bytes]`, lowercase hex.
- `parse_form`: `form_urlencoded::parse(body.as_bytes()).into_owned().collect()`.
- `suspicious_path`: lowercase the path; true if it contains `..`, `%2e`, `%2f`, or `%5c`.

- [ ] **Step 4: Write the failing view tests**

Add to `crates/core/src/view.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Catalog, Provider, Version};

    fn summary() -> BoxSummary {
        BoxSummary {
            org: "acme".into(),
            name: "base".into(),
            description: "<script>alert(1)</script>".into(),
            versions: 1,
            updated_at: "2026-11-13T00:00:00.000Z".into(),
        }
    }

    fn catalog() -> Catalog {
        Catalog {
            name: "acme/base".into(),
            description: "<script>alert(1)</script>".into(),
            versions: vec![Version {
                version: "1.0.0".into(),
                providers: vec![Provider {
                    name: "virtualbox".into(),
                    url: "https://e/x.box?q=<script>".into(),
                    checksum: String::new(),
                    checksum_type: String::new(),
                    architecture: String::new(),
                    default_architecture: None,
                }],
            }],
        }
    }

    #[test]
    fn index_escapes_description() {
        let html = index_page(&[summary()]);
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert(1)</script>"));
    }

    #[test]
    fn box_page_contains_csrf_and_escapes_url() {
        let html = box_page(&catalog(), "csrf-token");
        assert!(html.contains(r#"name="csrf" value="csrf-token""#));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>alert(1)</script>"));
    }

    #[test]
    fn error_page_escapes_messages() {
        let html = error_page(400, &["bad <tag>".into()]);
        assert!(html.contains("bad &lt;tag&gt;"));
    }
}
```

- [ ] **Step 5: Run tests to verify failure**

Run: `cargo test -p vagrant-registry-core view`

Expected: FAIL with missing view functions.

- [ ] **Step 6: Implement the maud views**

- `index_page`: `DOCTYPE`, `<html lang="en">`, `<meta charset="utf-8">`, a `<title>`, a small inline `<style>`, an `<h1>Boxes</h1>`, and a list/table of `org/box`, description, version count, and `updated_at`. Each name links to `/admin/boxes/{org}/{name}`.
- `box_page`: show name and description; a table of versions and providers; an add-version form with `method="post"` and `action="/admin/boxes/{org}/{box}/versions"`, a hidden `csrf` input, a `version` input, and a `providers_json` textarea prefilled with `[{"name":"virtualbox","url":"https://example.com/box.box"}]`; a delete form with `method="post"` and `action="/admin/boxes/{org}/{box}/delete"` plus the same hidden `csrf` input. Emit the hidden input as `input type="hidden" name="csrf" value=(csrf);` so the smoke test’s extraction stays stable.
- `error_page`: a minimal page with the status and escaped messages.
- No inline JavaScript. All HTML is produced with `maud::html!`, which escapes text by default.
- Export `pub mod http;` and `pub mod view;` from `lib.rs`.

- [ ] **Step 7: Run tests to verify they pass**

Run:

```bash
cargo test -p vagrant-registry-core
cargo fmt --all -- --check
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/http.rs crates/core/src/view.rs crates/core/src/lib.rs
git commit -m "feat: add auth, CSRF, form helpers, and admin views"
```

---

### Task 4: Worker scaffold, Wrangler config, D1 migration, health route

**Files:**
- Create: `wrangler.toml`
- Create: `package.json`
- Create: `.dev.vars.example`
- Create: `migrations/0001_init.sql`
- Create: `src/lib.rs`
- Create: `src/response.rs`
- Create: `scripts/smoke.sh`
- Modify: `.gitignore` (already covers build output)

**Interfaces:**
- Consumes: `worker` crate.
- Produces:

```rust
// src/response.rs
pub fn json_response(status: u16, body: String, cache: &str) -> worker::Result<worker::Response>;
pub fn head_json_response(status: u16, cache: &str) -> worker::Result<worker::Response>;
pub fn empty_response(status: u16, cache: &str) -> worker::Result<worker::Response>;
pub fn html_response(status: u16, body: String) -> worker::Result<worker::Response>;
pub fn text_response(status: u16, body: &str) -> worker::Result<worker::Response>;
```

- [ ] **Step 1: Create the Wrangler and package files**

`wrangler.toml`:

```toml
name = "vagrant-registry"
main = "build/index.js"
compatibility_date = "2026-11-13"

[build]
command = "worker-build --release"

[[d1_databases]]
binding = "DB"
database_name = "vagrant-registry"
database_id = "00000000-0000-0000-0000-000000000000"
migrations_dir = "migrations"

[observability]
enabled = true
```

Replace the placeholder `database_id` after `wrangler d1 create vagrant-registry`.

`package.json`:

```json
{
  "name": "vagrant-registry",
  "private": true,
  "scripts": {
    "dev": "wrangler dev",
    "deploy": "wrangler deploy",
    "db:migrate:local": "wrangler d1 migrations apply vagrant-registry --local",
    "db:migrate:remote": "wrangler d1 migrations apply vagrant-registry --remote"
  },
  "devDependencies": {
    "wrangler": "^4.0.0"
  }
}
```

`.dev.vars.example`:

```text
REGISTRY_ADMIN_TOKEN=dev-token
```

- [ ] **Step 2: Write the D1 migration**

`migrations/0001_init.sql`:

```sql
CREATE TABLE boxes (
    org         TEXT NOT NULL,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (org, name)
);

CREATE TABLE versions (
    org      TEXT NOT NULL,
    box      TEXT NOT NULL,
    version  TEXT NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (org, box, version),
    FOREIGN KEY (org, box) REFERENCES boxes (org, name) ON DELETE CASCADE
);

CREATE TABLE providers (
    org                  TEXT NOT NULL,
    box                  TEXT NOT NULL,
    version              TEXT NOT NULL,
    position             INTEGER NOT NULL,
    name                 TEXT NOT NULL,
    url                  TEXT NOT NULL,
    checksum             TEXT NOT NULL DEFAULT '',
    checksum_type        TEXT NOT NULL DEFAULT '',
    architecture         TEXT NOT NULL DEFAULT '',
    default_architecture INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (org, box, version, name, architecture),
    FOREIGN KEY (org, box, version) REFERENCES versions (org, box, version) ON DELETE CASCADE
);
```

Do not add separate indexes; the composite primary keys already cover the lookups. Do not add `PRAGMA foreign_keys`; D1 enforces foreign keys by default.

- [ ] **Step 3: Write the failing health smoke test**

`scripts/smoke.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

base="${1:-http://127.0.0.1:8787}"
token="${REGISTRY_ADMIN_TOKEN:-test-token}"

fail() { echo "FAIL: $*" >&2; exit 1; }
status() { curl -sS -o /dev/null -w '%{http_code}' "$@"; }

[ "$(status "$base/healthz")" = "200" ] || fail "healthz"

echo "ok: $base"
```

Make it executable: `chmod +x scripts/smoke.sh`.

- [ ] **Step 4: Install tools and verify the failure**

Replace the placeholder `src/lib.rs` with a temporary handler so the Worker builds:

```rust
use worker::*;

#[event(fetch)]
async fn fetch(_req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    Response::error("not found", 404)
}
```

Run:

```bash
cargo install worker-build --version 0.9.0 --locked
npm install
cp .dev.vars.example .dev.vars
npx wrangler d1 migrations apply vagrant-registry --local
npx wrangler dev --port 8787
```

In a second terminal: `./scripts/smoke.sh`.

Expected: FAIL (`healthz` returns 404), because no route is registered yet.

- [ ] **Step 5: Implement the Worker entry and response helpers**

`src/lib.rs`:

```rust
use worker::*;

mod response;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    Router::new()
        .get_async("/healthz", health)
        .run(req, env)
        .await
}

async fn health(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let result = match ctx.env.d1("DB") {
        Ok(db) => db.prepare("SELECT 1 AS ok").first::<i32>(Some("ok")).await,
        Err(err) => Err(err),
    };
    match result {
        Ok(Some(_)) => response::text_response(200, "ok"),
        Ok(None) | Err(_) => response::text_response(503, "unhealthy"),
    }
}
```

Log the error with `worker::console_error!` before returning 503; never include the error text in the response body.

`src/response.rs` sets `Content-Type`, `Cache-Control`, and `X-Content-Type-Options: nosniff`; never set `Content-Length` manually because Cloudflare derives it from the body. Use `Response::from_bytes(...).with_status(status).with_headers(headers)` for JSON/HTML, and `Response::empty()` for empty bodies.

- [ ] **Step 6: Run the health test again**

With `npx wrangler dev --port 8787` running:

```bash
./scripts/smoke.sh
```

Expected: `ok: http://127.0.0.1:8787`.

- [ ] **Step 7: Commit**

```bash
git add wrangler.toml package.json package-lock.json .dev.vars.example migrations src scripts
git commit -m "feat: scaffold Cloudflare Worker with D1 and health check"
```

---

### Task 5: D1 store, admin auth, full-document PUT, public catalog

**Files:**
- Create: `src/store.rs`
- Create: `src/public.rs`
- Create: `src/admin.rs`
- Modify: `src/lib.rs`
- Modify: `scripts/smoke.sh`

**Interfaces:**
- Consumes: `vagrant_registry_core::{model::Catalog, http::*, render::*}`; `worker::d1::{D1Database, D1Type}`.
- Produces:

```rust
// src/store.rs
pub struct Store { /* D1Database */ }
impl Store {
    pub fn new(db: D1Database) -> Self;
    pub async fn ping(&self) -> Result<(), StoreError>;
    pub async fn get_catalog(&self, org: &str, box_name: &str) -> Result<Option<Catalog>, StoreError>;
    pub async fn put_catalog(&self, c: &Catalog) -> Result<(), StoreError>;
}
pub enum StoreError { Db(String) }

// src/admin.rs
// Ok(None) = allowed, Ok(Some(response)) = reject with that response.
async fn check_admin(req: &Request, env: &Env) -> worker::Result<Option<Response>>;
async fn put_catalog(req: Request, ctx: RouteContext<()>) -> Result<Response>;
```

- [ ] **Step 1: Extend the smoke test and verify failure**

Add to `scripts/smoke.sh` a seed catalog and these checks:

```bash
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

cat > "$tmp/base.json" <<'JSON'
{
  "name": "acme/base",
  "description": "Acme base image",
  "versions": [
    {
      "version": "1.0.0",
      "providers": [
        {
          "name": "virtualbox",
          "url": "https://downloads.example.com/acme-base-1.0.0-vbox.box",
          "architecture": "amd64",
          "default_architecture": true
        }
      ]
    }
  ]
}
JSON

want() { [ "$1" = "$2" ] || fail "$3: got $1, want $2"; }

seed() {
  want "$(status -X PUT "$base/api/v1/admin/boxes/acme/base" \
    -H "Authorization: Bearer $token" \
    -H "Content-Type: application/json" \
    --data-binary @"$tmp/base.json")" 200 "admin PUT"
}

seed

curl -sS "$base/api/v2/vagrant/acme/base" -o "$tmp/a"
curl -sS "$base/acme/base" -o "$tmp/b"
cmp -s "$tmp/a" "$tmp/b" || fail "public route bodies differ"
grep -q '"name": "acme/base"' "$tmp/a" || fail "catalog name missing"
curl -sSI "$base/acme/base" | tr -d '\r' | grep -qi '^content-type: application/json' || fail "HEAD content-type"
want "$(status -I "$base/acme/base")" 200 "HEAD status"
want "$(status "$base/acme/missing")" 404 "missing box"
want "$(status --path-as-is "$base/api/v2/vagrant/acme/%2e%2e")" 404 "traversal"
want "$(status -X POST "$base/acme/base")" 405 "public method"
curl -sS -D - -o /dev/null -X POST "$base/acme/base" | tr -d '\r' | grep -qi '^allow: GET, HEAD' || fail "Allow header"
want "$(status -X PUT "$base/api/v1/admin/boxes/acme/base" -H 'Content-Type: application/json' --data-binary @"$tmp/base.json")" 401 "admin without auth"
want "$(status -X PUT "$base/api/v1/admin/boxes/acme/base" -H 'Authorization: Bearer wrong' -H 'Content-Type: application/json' --data-binary @"$tmp/base.json")" 401 "admin wrong token"
```

Run `./scripts/smoke.sh`. Expected: FAIL at the first admin PUT (404).

- [ ] **Step 2: Implement the store read/write path**

`src/store.rs`:

- `Store::new(db)` wraps `D1Database`.
- `ping`: `SELECT 1 AS ok`, `first::<i32>(Some("ok"))`.
- `get_catalog`:
  1. `SELECT description FROM boxes WHERE org = ?1 AND name = ?2`; `first::<String>(Some("description"))`; `None` → `Ok(None)`.
  2. `SELECT version FROM versions WHERE org = ?1 AND box = ?2 ORDER BY position`; `all()`; `results::<VersionRow>()`.
  3. `SELECT version, name, url, checksum, checksum_type, architecture, default_architecture FROM providers WHERE org = ?1 AND box = ?2 ORDER BY version, position`; group rows into `HashMap<String, Vec<Provider>>` by `version`; map `default_architecture` 0/1 to `None` when `architecture` is empty, otherwise `Some(value != 0)`.
  4. Build `Catalog { name: format!("{org}/{box_name}"), description, versions }` in the version order from step 2.
- `put_catalog`:
  1. `validate(c)`; any error → `StoreError::Db("invalid catalog".into())` (handlers validate first; this is a guard).
  2. Normalize and split `c.name` at the single `/`.
  3. `now = js_sys::Date::new_0().to_iso_string().into()`.
  4. Build one `Vec<D1PreparedStatement>` in this exact order:
     - `DELETE FROM providers WHERE org = ?1 AND box = ?2`
     - `DELETE FROM versions WHERE org = ?1 AND box = ?2`
     - `INSERT INTO boxes (org, name, description, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4) ON CONFLICT(org, name) DO UPDATE SET description = excluded.description, updated_at = excluded.updated_at`
     - One insert per version: `INSERT INTO versions (org, box, version, position) VALUES (?1, ?2, ?3, ?4)`
     - One insert per provider: `INSERT INTO providers (org, box, version, position, name, url, checksum, checksum_type, architecture, default_architecture) VALUES (?1, ..., ?10)`
  5. `db.batch(statements).await` and map errors to `StoreError::Db`.
- Bind with `D1Type::Text(&str)` and `D1Type::Integer(i32)` via `stmt.bind_refs([...].iter())`.
- `StoreError` implements `From<worker::Error>` and never includes SQL or tokens in client responses.

- [ ] **Step 3: Implement admin auth and the PUT handler**

`src/admin.rs`:

- `admin_binding(env) -> Option<String>`: `env.secret("REGISTRY_ADMIN_TOKEN").ok().map(|s| s.to_string())`.
- `check_admin` returns `Ok(None)` when allowed and `Ok(Some(response))` when rejected:
  - `Disabled` → `Ok(Some(text_response(404, "not found")?))`
  - `Enabled(token)` and `admin_authorized(req.headers().get("authorization").ok().flatten().as_deref(), &token)` → `Ok(None)`
  - otherwise → `Ok(Some(401 response with WWW-Authenticate: Basic realm="vagrant-registry" and Cache-Control: no-store))`
- Each protected handler starts with:
  ```rust
  if let Some(rejection) = check_admin(&req, &ctx.env).await? {
      return Ok(rejection);
  }
  ```
- `put_catalog`:
  1. `check_admin`.
  2. Reject a `Content-Length` greater than `1_048_576` with 413.
  3. `req.bytes().await`; reject > 1 MiB with 413; `String::from_utf8` → 400 on failure.
  4. `serde_json::from_str::<Catalog>` → 400 on error.
  5. Factor the read/limit/decode sequence into a `read_json_body(req, limit)` helper in `src/admin.rs`; Task 6 reuses it for the version POST.
  6. Validate URL params with `valid_box_name`; compute `expected = format!("{org}/{box}")`; if `catalog.name != expected` → 400.
  7. `validate(&catalog)`; non-empty → 400 with `{"error": "..."}`.
  8. `store.put_catalog(&catalog)`; on error log with `worker::console_error!` and return 500.
  9. Return 200 with `to_json(&catalog)`.
- The handler must accept names from `ctx.param("org")`/`ctx.param("box")` and return 400 if either is missing.

- [ ] **Step 4: Implement the public handler and wire the router**

`src/public.rs`:

- Reject `suspicious_path(&req.path())` with 404.
- Only `GET` and `HEAD`; anything else → 405 with `Allow: GET, HEAD`.
- Validate `org/box` with `valid_box_name`; invalid → 404 (do not echo input).
- `store.get_catalog`; `None` → 404 with `X-Content-Type-Options: nosniff`.
- `GET`: `to_json`; render with `json_response(200, body, "public, max-age=60")`.
- `HEAD`: render an empty response with `Content-Type: application/json`, `Cache-Control: public, max-age=60`, and `X-Content-Type-Options: nosniff`; do not set `Content-Length`. Cloudflare may report `0` or omit it (see Global Constraints).
- Add `head_json_response(status, cache)` to `src/response.rs` for the HEAD case.
- Refactor `health` to `Store::new(ctx.env.d1("DB")?).ping().await`; 200 `ok` on success, 503 `unhealthy` on error.

`src/lib.rs` router:

```rust
Router::new()
    .get_async("/healthz", health)
    .on_async("/api/v2/vagrant/:org/:box", public::catalog)
    .on_async("/:org/:box", public::catalog)
    .put_async("/api/v1/admin/boxes/:org/:box", admin::put_catalog)
    .run(req, env)
    .await
```

Use `on_async` for the public routes so the handler owns method dispatch and can return `Allow` on 405.

- [ ] **Step 5: Run the smoke test**

Run: `./scripts/smoke.sh`

Expected: `ok: http://127.0.0.1:8787` with all checks passing.

If the dev server is not running: `npx wrangler dev --port 8787` in another terminal.

- [ ] **Step 6: Commit**

```bash
git add src/store.rs src/admin.rs src/public.rs src/lib.rs scripts/smoke.sh
git commit -m "feat: serve public catalogs and atomic admin PUT from D1"
```

---

### Task 6: Complete the admin JSON API

**Files:**
- Modify: `src/store.rs`
- Modify: `src/admin.rs`
- Modify: `src/lib.rs`
- Modify: `scripts/smoke.sh`

**Interfaces:**
- Consumes: the Task 5 `Store`, `check_admin`, `json_response`, `empty_response`; `vagrant_registry_core::{model::Version, view::BoxSummary}`.
- Produces:

```rust
// store.rs
pub enum AddVersionOutcome {
    Created,
    BoxNotFound,
    Conflict,
}
impl Store {
    pub async fn list_boxes(&self) -> Result<Vec<BoxSummary>, StoreError>;
    pub async fn delete_catalog(&self, org: &str, box_name: &str) -> Result<bool, StoreError>;
    pub async fn add_version(&self, org: &str, box_name: &str, v: &Version) -> Result<AddVersionOutcome, StoreError>;
    pub async fn delete_version(&self, org: &str, box_name: &str, version: &str) -> Result<bool, StoreError>;
}

// admin.rs handlers
async fn list_boxes(req: Request, ctx: RouteContext<()>) -> Result<Response>;
async fn get_box(req: Request, ctx: RouteContext<()>) -> Result<Response>;
async fn delete_box(req: Request, ctx: RouteContext<()>) -> Result<Response>;
async fn add_version(req: Request, ctx: RouteContext<()>) -> Result<Response>;
async fn delete_version(req: Request, ctx: RouteContext<()>) -> Result<Response>;
```

- [ ] **Step 1: Extend the smoke test and verify failure**

Add (start with `seed` so the section is independent of earlier runs; Task 7 re-seeds after the delete check):

```bash
seed
# list
want "$(status "$base/api/v1/admin/boxes" -H "Authorization: Bearer $token")" 200 "admin list"
want "$(status "$base/api/v1/admin/boxes" -u "admin:$token")" 200 "admin basic"
# get one
want "$(status "$base/api/v1/admin/boxes/acme/base" -H "Authorization: Bearer $token")" 200 "admin get one"
# add version
cat > "$tmp/v2.json" <<'JSON'
{"version":"2.0.0","providers":[{"name":"libvirt","url":"https://e/y.box"}]}
JSON
want "$(status -X POST "$base/api/v1/admin/boxes/acme/base/versions" \
  -H "Authorization: Bearer $token" -H "Content-Type: application/json" \
  --data-binary @"$tmp/v2.json")" 201 "add version"
want "$(status -X POST "$base/api/v1/admin/boxes/acme/base/versions" \
  -H "Authorization: Bearer $token" -H "Content-Type: application/json" \
  --data-binary @"$tmp/v2.json")" 409 "duplicate version"
# delete version
want "$(status -X DELETE "$base/api/v1/admin/boxes/acme/base/versions/2.0.0" \
  -H "Authorization: Bearer $token")" 204 "delete version"
want "$(status -X DELETE "$base/api/v1/admin/boxes/acme/base/versions/2.0.0" \
  -H "Authorization: Bearer $token")" 404 "delete missing version"
# atomic invalid PUT
printf '%s' '{"name":"acme/base","versions":[{"version":"bad","providers":[{"name":"virtualbox","url":"https://e/x.box"}]}]}' > "$tmp/bad.json"
want "$(status -X PUT "$base/api/v1/admin/boxes/acme/base" \
  -H "Authorization: Bearer $token" -H "Content-Type: application/json" \
  --data-binary @"$tmp/bad.json")" 400 "invalid PUT"
curl -sS "$base/acme/base" | grep -q '"version": "1.0.0"' || fail "old catalog lost after invalid PUT"
# concurrent full-document PUTs: all succeed and the final catalog is whole
for i in 1 2 3 4 5; do
  (status -X PUT "$base/api/v1/admin/boxes/acme/base" \
    -H "Authorization: Bearer $token" -H "Content-Type: application/json" \
    --data-binary @"$tmp/base.json" > "$tmp/put-$i") &
done
wait
for i in 1 2 3 4 5; do
  want "$(cat "$tmp/put-$i")" 200 "concurrent PUT $i"
done
curl -sS "$base/acme/base" | grep -q '"name": "acme/base"' || fail "concurrent PUT left a broken catalog"
# name mismatch
printf '%s' '{"name":"acme/wrong","versions":[{"version":"1.0.0","providers":[{"name":"virtualbox","url":"https://e/x.box"}]}]}' > "$tmp/mismatch.json"
want "$(status -X PUT "$base/api/v1/admin/boxes/acme/base" \
  -H "Authorization: Bearer $token" -H "Content-Type: application/json" \
  --data-binary @"$tmp/mismatch.json")" 400 "name mismatch"
# body limit
python3 -c 'print("a" * 1100000)' > "$tmp/big.json"
want "$(status -X PUT "$base/api/v1/admin/boxes/acme/base" \
  -H "Authorization: Bearer $token" -H "Content-Type: application/json" \
  --data-binary @"$tmp/big.json")" 413 "body limit"
# delete box
want "$(status -X DELETE "$base/api/v1/admin/boxes/acme/base" \
  -H "Authorization: Bearer $token")" 204 "delete box"
want "$(status "$base/acme/base")" 404 "deleted box still public"
```

Run `./scripts/smoke.sh`. Expected: FAIL at the list check (404).

- [ ] **Step 2: Implement the store methods**

- `list_boxes`:
  ```sql
  SELECT b.org, b.name, b.description, COUNT(v.version) AS versions, b.updated_at
  FROM boxes b
  LEFT JOIN versions v ON v.org = b.org AND v.box = b.name
  GROUP BY b.org, b.name
  ORDER BY b.org, b.name
  ```
  Map to `BoxSummary { org, name, description, versions: i32, updated_at }`.
- `delete_catalog`: batch `DELETE FROM providers ...`, `DELETE FROM versions ...`, `DELETE FROM boxes ...`; return `changes > 0` from the last result's `meta()`.
- `add_version`:
  1. Check the box exists (`SELECT 1 AS ok FROM boxes ...`); missing → `BoxNotFound`.
  2. Check the version is absent (`SELECT 1 AS ok FROM versions ...`); present → `Conflict`.
  3. Batch: insert the version at `position = (SELECT COALESCE(MAX(position) + 1, 0) FROM versions WHERE org = ?1 AND box = ?2)`, insert its providers with positions `0..`, and `UPDATE boxes SET updated_at = ?3 WHERE org = ?1 AND name = ?2`.
  4. If the batch fails with an error containing `UNIQUE constraint failed`, return `Conflict`; otherwise `StoreError::Db`.
- `delete_version`: batch `DELETE FROM providers ...`, `DELETE FROM versions ...`, and `UPDATE boxes SET updated_at = ?4 ...`; return `changes > 0` from the versions delete result.

- [ ] **Step 3: Implement the admin handlers and routes**

Behavior:

- `GET /api/v1/admin/boxes` → 200 with a JSON array of `{name, description, versions, updated_at}`; `name` is `org/box`.
- `GET /api/v1/admin/boxes/{org}/{box}` → 200 canonical catalog JSON; 404 if missing.
- `DELETE /api/v1/admin/boxes/{org}/{box}` → 204; 404 if missing.
- `POST /api/v1/admin/boxes/{org}/{box}/versions` → decode a `Version` with the same 1 MiB limit; wrap it in `Catalog { name, description: String::new(), versions: vec![version] }`; validate; call `add_version`; 201 on created, 404 on missing box, 409 on conflict.
- `DELETE /api/v1/admin/boxes/{org}/{box}/versions/{version}` → 204; 404 if missing.
- All handlers call `check_admin` first and validate `org`/`box` with `valid_box_name`; invalid → 400. All responses use `Cache-Control: no-store`. Error bodies are `{"error":"..."}` and never contain SQL, D1 messages, or tokens.
- Add the routes:
  ```rust
  .get_async("/api/v1/admin/boxes", admin::list_boxes)
  .get_async("/api/v1/admin/boxes/:org/:box", admin::get_box)
  .delete_async("/api/v1/admin/boxes/:org/:box", admin::delete_box)
  .post_async("/api/v1/admin/boxes/:org/:box/versions", admin::add_version)
  .delete_async("/api/v1/admin/boxes/:org/:box/versions/:version", admin::delete_version)
  ```
  Keep the Task 5 `put_async` route.

- [ ] **Step 4: Run the smoke test**

Run: `./scripts/smoke.sh`

Expected: PASS, including invalid-PUT atomicity, duplicate 409, delete 204, and body limit 413.

- [ ] **Step 5: Commit**

```bash
git add src/store.rs src/admin.rs src/lib.rs scripts/smoke.sh
git commit -m "feat: add admin list, delete, and version endpoints"
```

---

### Task 7: Admin UI and CSRF protection

**Files:**
- Create: `src/ui.rs`
- Modify: `src/lib.rs`
- Modify: `scripts/smoke.sh`

**Interfaces:**
- Consumes: `Store`, `check_admin`, `view::*`, `http::*`, `catalog::Version`.
- Produces:

```rust
async fn index(req: Request, ctx: RouteContext<()>) -> Result<Response>;
async fn show_box(req: Request, ctx: RouteContext<()>) -> Result<Response>;
async fn add_version(req: Request, ctx: RouteContext<()>) -> Result<Response>;
async fn delete_box(req: Request, ctx: RouteContext<()>) -> Result<Response>;
```

- [ ] **Step 1: Extend the smoke test and verify failure**

Add:

```bash
seed
jar="$tmp/cookies.txt"
# UI requires auth
want "$(status "$base/admin")" 401 "UI without auth"
# UI page and security headers
html=$(curl -sS -c "$jar" -u "admin:$token" "$base/admin")
printf '%s' "$html" | grep -q 'acme/base' || fail "UI index missing box"
headers=$(curl -sSI -u "admin:$token" "$base/admin" | tr -d '\r')
printf '%s' "$headers" | grep -qi '^x-frame-options: DENY' || fail "X-Frame-Options"
printf '%s' "$headers" | grep -qi '^x-content-type-options: nosniff' || fail "nosniff"
printf '%s' "$headers" | grep -qi '^content-security-policy:' || fail "CSP"
# CSRF: POST without token
want "$(status -b "$jar" -u "admin:$token" -X POST \
  "$base/admin/boxes/acme/base/versions" \
  --data-urlencode 'version=2.0.0' \
  --data-urlencode 'providers_json=[{"name":"libvirt","url":"https://e/y.box"}]')" 403 "CSRF missing"
# CSRF: fetch the form token, then POST
page=$(curl -sS -b "$jar" -c "$jar" -u "admin:$token" "$base/admin/boxes/acme/base")
csrf=$(printf '%s' "$page" | sed -n 's/.*name="csrf" value="\([^"]*\)".*/\1/p' | head -1)
[ -n "$csrf" ] || fail "csrf token not found"
want "$(status -b "$jar" -u "admin:$token" -X POST \
  "$base/admin/boxes/acme/base/versions" \
  --data-urlencode "csrf=$csrf" \
  --data-urlencode 'version=2.0.0' \
  --data-urlencode 'providers_json=[{"name":"libvirt","url":"https://e/y.box"}]')" 303 "UI add version"
curl -sS "$base/acme/base" | grep -q '"version": "2.0.0"' || fail "UI version not stored"
```

Run `./scripts/smoke.sh`. Expected: FAIL at `UI without auth` (404).

- [ ] **Step 2: Implement the UI routes**

`src/ui.rs`:

- `check_admin` on every route; on failure return the 401/404 response.
- `GET /admin`: `store.list_boxes`, `view::index_page`, `html_response(200, html)`.
- `GET /admin/boxes/:org/:box`: `store.get_catalog`; missing → `error_page(404, ...)`; ensure the CSRF cookie:
  - Read `req.headers().get("cookie")`, `cookie_value(..., "registry_csrf")`.
  - If absent, `random_hex(32)`, set `Set-Cookie: registry_csrf=<token>; Path=/admin; HttpOnly; SameSite=Strict` plus `; Secure` when `req.url()?.scheme() == "https"`.
  - Render `view::box_page(&catalog, &token)`.
- `POST /admin/boxes/:org/:box/versions`:
  1. Require `Content-Type: application/x-www-form-urlencoded`; otherwise 415.
  2. Read the body (≤ 1 MiB); `parse_form`.
  3. `csrf_matches(cookie_value(cookie_header, "registry_csrf").as_deref(), form.get("csrf").map(String::as_str))`; false → 403.
  4. Parse `version` and `providers_json` (a JSON array of `Provider`); build a one-version catalog; `validate`.
  5. `store.add_version`; `Created` → 303 to `/admin/boxes/{org}/{box}`; `BoxNotFound` → 404; `Conflict` → 409; validation → `error_page(400, ...)`.
- `POST /admin/boxes/:org/:box/delete`: same CSRF check; `store.delete_catalog`; 303 to `/admin`; missing → 404.
- Every UI response gets `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, and `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; form-action 'self'`.
- Set `Cache-Control: no-store` on every UI response.
- Register:
  ```rust
  .get_async("/admin", ui::index)
  .get_async("/admin/boxes/:org/:box", ui::show_box)
  .post_async("/admin/boxes/:org/:box/versions", ui::add_version)
  .post_async("/admin/boxes/:org/:box/delete", ui::delete_box)
  ```

- [ ] **Step 3: Run the smoke test**

Run: `./scripts/smoke.sh`

Expected: PASS. Then manually confirm in a browser that `http://127.0.0.1:8787/admin` prompts for Basic auth and the add-version form works.

- [ ] **Step 4: Commit**

```bash
git add src/ui.rs src/lib.rs scripts/smoke.sh
git commit -m "feat: add admin UI with CSRF protection"
```

---

### Task 8: YAML import endpoint and script

**Files:**
- Create: `src/admin_import.rs` or add to `src/admin.rs`
- Create: `boxes.test/acme/base.yaml`
- Create: `scripts/import.sh`
- Modify: `src/lib.rs`
- Modify: `scripts/smoke.sh`

**Interfaces:**
- Consumes: `vagrant_registry_core::yaml::parse_yaml`, `Store::put_catalog`, `check_admin`.
- Produces: `PUT /api/v1/admin/import/{org}/{box}` with a YAML body.

- [ ] **Step 1: Write the failing import test and fixture**

`boxes.test/acme/base.yaml`: copy the spec §3 example.

`scripts/import.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

root="${1:?usage: import.sh <boxes-dir> [base-url]}"
base="${2:-http://127.0.0.1:8787}"
token="${REGISTRY_ADMIN_TOKEN:?set REGISTRY_ADMIN_TOKEN}"
root="${root%/}"
count=0

while IFS= read -r -d '' file; do
  rel="${file#"$root"/}"
  rel="${rel%.*}"
  case "$rel" in
    */*/*) echo "nested catalogs are not supported: $file" >&2; exit 1 ;;
    */*) ;;
    *) echo "catalogs must live at <org>/<box>.yaml: $file" >&2; exit 1 ;;
  esac
  curl -fsS -X PUT "$base/api/v1/admin/import/$rel" \
    -H "Authorization: Bearer $token" \
    -H "Content-Type: application/yaml" \
    --data-binary @"$file" \
    -o /dev/null
  count=$((count + 1))
done < <(find "$root" -type f \( -name '*.yaml' -o -name '*.yml' \) -print0)

echo "imported $count catalogs into $base"
```

Add to the smoke test:

```bash
REGISTRY_ADMIN_TOKEN="$token" ./scripts/import.sh boxes.test "$base" >/dev/null
curl -sS "$base/acme/base" | grep -q '"name": "acme/base"' || fail "import did not publish"
# path/name mismatch
mkdir -p "$tmp/boxes/acme"
printf '%s\n' 'name: acme/wrong' 'versions:' '  - version: "1.0.0"' '    providers:' '      - name: virtualbox' '        url: https://e/x.box' > "$tmp/boxes/acme/base.yaml"
if REGISTRY_ADMIN_TOKEN="$token" ./scripts/import.sh "$tmp/boxes" "$base" >/dev/null 2>&1; then
  fail "import accepted a path/name mismatch"
fi
```

Run `./scripts/smoke.sh`. Expected: FAIL at the import step (endpoint 404).

- [ ] **Step 2: Implement the import handler**

- Route: `PUT /api/v1/admin/import/:org/:box`.
- `check_admin`; 1 MiB body limit; `Content-Type` must be `application/yaml`, `text/yaml`, or `application/x-yaml` (ignore parameters after `;`); otherwise 415.
- `parse_yaml(&body)`; parse errors → 400 `{"error":"..."}`.
- Require `catalog.name == "{org}/{box}"`; mismatch → 400.
- `validate`; errors → 400.
- `store.put_catalog`; success → 200 canonical JSON.
- Register `.put_async("/api/v1/admin/import/:org/:box", admin::import_yaml)`.

- [ ] **Step 3: Run the smoke test**

Run: `./scripts/smoke.sh`

Expected: PASS, including the path/name mismatch rejection.

- [ ] **Step 4: Commit**

```bash
git add boxes.test scripts/import.sh scripts/smoke.sh src/admin.rs src/lib.rs
git commit -m "feat: add YAML import endpoint and bulk import script"
```

---

### Task 9: Usage documentation and deployment

**Files:**
- Create: `docs/usage.md`
- Modify: `README.md`

**Interfaces:**
- Consumes: the complete Worker.
- Produces: a runbook a new maintainer can follow.

- [ ] **Step 1: Write `docs/usage.md`**

Cover:

- Prerequisites: Rust 1.91, `rustup target add wasm32-unknown-unknown`, Node 20+, `cargo install worker-build --version 0.9.0 --locked`, Cloudflare account.
- Local development:
  ```bash
  npm install
  cp .dev.vars.example .dev.vars
  npx wrangler d1 migrations apply vagrant-registry --local
  npx wrangler dev --port 8787
  ./scripts/smoke.sh
  ```
- First deployment:
  ```bash
  npx wrangler d1 create vagrant-registry
  # paste database_id into wrangler.toml
  npx wrangler d1 migrations apply vagrant-registry --remote
  npx wrangler secret put REGISTRY_ADMIN_TOKEN
  npx wrangler deploy
  ```
- Import:
  ```bash
  REGISTRY_ADMIN_TOKEN=... ./scripts/import.sh ./boxes https://registry.example.com
  ```
- Publish one catalog via JSON:
  ```bash
  curl -X PUT "https://registry.example.com/api/v1/admin/boxes/acme/base" \
    -H "Authorization: Bearer $REGISTRY_ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    --data-binary @catalog.json
  ```
- Client:
  ```bash
  export VAGRANT_SERVER_URL="https://registry.example.com"
  ```
- Admin UI: `https://registry.example.com/admin`, Basic auth `admin` / `REGISTRY_ADMIN_TOKEN`.
- Backups and recovery: `npx wrangler d1 export vagrant-registry --remote --output backup.sql`; D1 Time Travel retention is 7 days on Free and 30 days on Paid; test restores.
- Limits: 1 MiB request body, 100 providers per version, D1 10 GB (Paid) / 500 MB (Free), 1,000 queries per invocation on Paid / 50 on Free.
- Differences from Options A/B/C: no filesystem at runtime; YAML import is an HTTP endpoint; versions must be quoted YAML strings; Cloudflare may omit `Content-Length` on `HEAD` requests, but Vagrant only inspects `Content-Type` and the status code.
- Non-goals: no binary hosting, no Vagrant Cloud API v1, no accounts.

- [ ] **Step 2: Update `README.md`**

- Add Option D to the read-order list.
- Add a "Which option?" row: pick D when you want Option C's API/UI but do not want to run a server, and Cloudflare Workers + D1 is acceptable.
- Change "All three plans" to "Options A/B/C use Go 1.22+; Option D uses Rust 1.91."
- Add the Option D plan to the repository layout tree.

- [ ] **Step 3: Verify the deploy build**

Run:

```bash
cargo fmt --all -- --check
cargo test -p vagrant-registry-core
cargo check -p vagrant-registry-worker --target wasm32-unknown-unknown
npx wrangler deploy --dry-run --outdir dist
```

Expected: all pass; `wrangler` prints a dry-run summary and writes `dist/`.

- [ ] **Step 4: Commit**

```bash
git add README.md docs/usage.md
git commit -m "docs: add Option D usage and deployment runbook"
```

---

### Task 10: CI and full verification

**Files:**
- Create: `.github/workflows/ci.yml`
- Modify: `scripts/smoke.sh` if any check is still missing

**Interfaces:**
- Consumes: all previous tasks.
- Produces: a CI pipeline that compiles the Worker and runs the HTTP smoke tests against local D1.

- [ ] **Step 1: Write the CI workflow**

`.github/workflows/ci.yml`:

```yaml
name: ci

on:
  push:
  pull_request:

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: "1.91"
          targets: wasm32-unknown-unknown
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all -- --check
      - run: cargo clippy -p vagrant-registry-core --all-targets -- -D warnings
      - run: cargo test -p vagrant-registry-core
      - run: cargo clippy -p vagrant-registry-worker --target wasm32-unknown-unknown -- -D warnings
      - run: cargo check -p vagrant-registry-worker --target wasm32-unknown-unknown
      - uses: actions/setup-node@v4
        with:
          node-version: "22"
          cache: npm
      - run: npm ci
      - run: cargo install worker-build --version 0.9.0 --locked
      - run: npx wrangler deploy --dry-run --outdir dist

  smoke:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: "1.91"
          targets: wasm32-unknown-unknown
      - uses: Swatinem/rust-cache@v2
      - uses: actions/setup-node@v4
        with:
          node-version: "22"
          cache: npm
      - run: npm ci
      - run: cargo install worker-build --version 0.9.0 --locked
      - run: printf 'REGISTRY_ADMIN_TOKEN=test-token\n' > .dev.vars
      - run: npx wrangler d1 migrations apply vagrant-registry --local
      - run: |
          npx wrangler dev --port 8787 > /tmp/wrangler.log 2>&1 &
          echo $! > /tmp/wrangler.pid
          for i in $(seq 1 60); do
            if curl -fsS http://127.0.0.1:8787/healthz >/dev/null; then exit 0; fi
            sleep 1
          done
          cat /tmp/wrangler.log
          exit 1
      - run: ./scripts/smoke.sh
      - if: always()
        run: kill "$(cat /tmp/wrangler.pid)" || true
```

- [ ] **Step 2: Run the full local verification**

```bash
cargo fmt --all -- --check
cargo clippy -p vagrant-registry-core --all-targets -- -D warnings
cargo test -p vagrant-registry-core
cargo clippy -p vagrant-registry-worker --target wasm32-unknown-unknown -- -D warnings
cargo check -p vagrant-registry-worker --target wasm32-unknown-unknown
npx wrangler d1 migrations apply vagrant-registry --local
npx wrangler dev --port 8787 &
./scripts/smoke.sh
```

Expected: no formatting or clippy output; all tests pass; smoke prints `ok: http://127.0.0.1:8787`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml scripts/smoke.sh
git commit -m "ci: build the Worker and run D1 smoke tests"
```

---

## Self-review checklist

- [ ] Spec §2 routes, methods, `Content-Type`, and name matching are covered by Task 5 and its smoke checks.
- [ ] Every spec §4.1 validation row has a test in Task 1.
- [ ] `default_architecture` is emitted as `false` for architecture-aware non-defaults and omitted for legacy providers (Task 1).
- [ ] Unquoted YAML versions are rejected with an actionable error (Task 2).
- [ ] Admin token unset → 404, wrong token → 401, Bearer and Basic both work (Tasks 3, 5).
- [ ] Full-document `PUT` is one D1 batch; an invalid replacement leaves the old catalog intact (Tasks 5, 6).
- [ ] Concurrent full-document PUTs all succeed and leave a whole catalog (Task 6).
- [ ] UI POST without a valid CSRF token returns 403 (Task 7).
- [ ] Path traversal in a URL segment returns 404 (Task 5).
- [ ] `HEAD` returns 200 with `Content-Type: application/json` and no body; the runbook explains Cloudflare's `Content-Length` behavior (Tasks 5, 9).
- [ ] Deleting a box removes its versions and providers; re-creating the box works (Task 6).
- [ ] Import enforces path/name equality and validates every document (Task 8).
- [ ] Deployment and backup steps are documented (Task 9).
- [ ] CI compiles the Worker and runs the smoke suite against local D1 (Task 10).
