# Vagrant Registry — Design and Contract

**Status:** proposed design, approved for planning
**Date:** 2026-11-13
**Scope:** a catalog-only box index compatible with `VAGRANT_SERVER_URL`.
No box binary hosting, proxying, or Vagrant Cloud feature parity.

---

## 1. Intent

Vagrant’s default box server (`DEFAULT_SERVER_URL`,
`lib/vagrant/shared_helpers.rb:16`) is being phased out. We need a
self-hosted replacement for the part Vagrant actually uses when resolving
boxes: **the version/provider catalog**.

The registry answers “which versions and providers exist for `org/box`, and
where are their `.box` files?”. The `.box` files remain in existing
artifact storage (S3, GitHub Releases, an internal file server, a CDN).
Vagrant downloads them directly from the URLs in the catalog.

This is intentionally not a Vagrant Cloud replacement. It does not host
box files, implement accounts, or support the `vagrant cloud` command
family.

---

## 2. What Vagrant actually calls

This section is the compatibility contract. It is derived from the code in
this repository and is the source of truth for every plan.

### 2.1 Discovery paths

When a user runs `vagrant box add acme/base`, or has
`config.vm.box = "acme/base"`, Vagrant expands the shorthand
(`lib/vagrant/action/builtin/box_add.rb:68-93`):

```text
{server}/api/v2/vagrant/acme/base     # preferred
{server}/acme/base                    # legacy fallback
```

`{server}` is resolved by `Vagrant.server_url`:

1. `VAGRANT_SERVER_URL` environment variable, unless it is empty;
2. `config.vm.box_server_url` (Vagrantfile path only — see 2.4);
3. `DEFAULT_SERVER_URL` (`lib/vagrant/shared_helpers.rb:16,93-97`).

Vagrant probes both expanded URLs with `HEAD`. A URL counts as catalog
metadata when the final response’s `Content-Type` matches
`application/json` (`box_add.rb:614-663`). If both are valid, the API path
wins. If only one is valid, that one is used. Non-2xx responses are errors
because curl runs with `--fail`.

After a valid catalog is found, Vagrant downloads it with `GET` and
`Accept: application/json`, selects a version and provider, then downloads
the provider’s `url` with a plain `GET` (`box_add.rb:208-398`).

### 2.2 Required routes

The registry must serve these two routes, both with `GET` and `HEAD`, and
return the same bytes:

```text
/api/v2/vagrant/{org}/{box}
/{org}/{box}
```

Nothing else is required for box resolution. No version-specific endpoint,
search endpoint, upload endpoint, or `/api/v1` is used by `box add`,
`vagrant up`, `vagrant box update`, or `vagrant box outdated`.

### 2.3 Catalog JSON

The response is the document parsed by `Vagrant::BoxMetadata`
(`lib/vagrant/box_metadata.rb`):

```json
{
  "name": "acme/base",
  "description": "Acme base image",
  "versions": [
    {
      "version": "1.2.0",
      "providers": [
        {
          "name": "virtualbox",
          "url": "https://downloads.example.com/acme-base-1.2.0-vbox-amd64.box",
          "checksum_type": "sha256",
          "checksum": "0123...",
          "architecture": "amd64",
          "default_architecture": true
        },
        {
          "name": "virtualbox",
          "url": "https://downloads.example.com/acme-base-1.2.0-vbox-arm64.box",
          "checksum_type": "sha256",
          "checksum": "4567...",
          "architecture": "arm64",
          "default_architecture": false
        },
        {
          "name": "libvirt",
          "url": "https://downloads.example.com/acme-base-1.2.0-libvirt.box",
          "architecture": "amd64",
          "default_architecture": true
        }
      ]
    }
  ]
}
```

Required top-level fields: `name`, `versions`. `description` is optional.
Each version requires `version` and a non-empty `providers` array. Each
provider requires `name` and `url`; checksum fields and architecture fields
are optional.

`providers[].url` may point anywhere reachable by the client. It may be a
presigned URL: provider URLs are only fetched with `GET`, never probed with
`HEAD`.

### 2.4 Client configuration

Global, works for both the CLI and Vagrantfiles:

```bash
export VAGRANT_SERVER_URL="https://registry.example.com"
```

Per project:

```ruby
config.vm.box = "acme/base"
config.vm.box_server_url = "https://registry.example.com"
```

Important detail: `config.vm.box_server_url` is passed only through the
`up`/`box` action path (`lib/vagrant/action/builtin/handle_box.rb:88-91`).
The CLI `vagrant box add acme/base` does **not** read it
(`plugins/commands/box/command/add.rb:88-90`). Use `VAGRANT_SERVER_URL` for
CLI shorthand adds, or use a full catalog URL.

A full catalog URL also works without any shorthand support:

```ruby
config.vm.box = "acme/base"
config.vm.box_url = "https://registry.example.com/catalogs/acme/base.json"
```

or:

```bash
vagrant box add https://registry.example.com/catalogs/acme/base.json
```

This is the escape hatch for purely static hosts that cannot set a JSON
`Content-Type` on an extensionless path.

### 2.5 Matching rules Vagrant enforces

These rules decide whether a correct-looking catalog still fails:

1. **Name must match the requested box.** For a Vagrantfile add,
   `env[:box_name]` is `config.vm.box`, and a catalog whose `name` differs
   raises `BoxAddNameMismatch` (`box_add.rb:255-259`). The CLI without
   `--name` uses the catalog’s `name`, so aliases are possible for the CLI
   but not for a Vagrantfile that uses a different name.
2. **Versions must parse as `Gem::Version`.** The accepted shape is
   `^[0-9]+(\.[0-9A-Za-z]+)*$`. `v1.2.0`, `1.2.0+build`, and empty strings
   are invalid. Version constraints (`config.vm.box_version`,
   `--box-version`) are `Gem::Requirement` expressions such as
   `">= 1.0, < 2.0"`.
3. **Provider names must match the provider’s box format.** The Vagrantfile
   path uses `machine.provider_options[:box_format] || provider_name`
   (`handle_box.rb:74-75`). Typical values: `virtualbox`, `vmware_desktop`,
   `hyperv`, `libvirt`, `docker`, `parallels`.
4. **Architecture matching.** With the default `box_architecture = :auto`,
   Vagrant matches the host architecture (`BoxMetadata::Version#provider`,
   `box_metadata.rb:165-219`). Two compatibility traps:
   - A provider entry that has `architecture` set but no
     `default_architecture` key is treated as **not** architecture-aware by
     the final legacy fallback. If it is the only provider for that name,
     Vagrant may select it on the wrong architecture. Always emit
     `"default_architecture": false` for non-default architecture entries.
   - A legacy entry with neither `architecture` nor `default_architecture`
     is matched only when there is a single provider for that name. Use
     this for single-arch/legacy boxes.
5. **Checksums are optional but validated when present.** Supported types:
   `md5`, `sha1`, `sha256`, `sha384`, `sha512`. The checksum is computed on
   the downloaded bytes, after redirects.
6. **The catalog URL is persisted.** `BoxCollection#add` writes the catalog
   URL to `~/.vagrant.d/boxes/<name>/metadata_url`
   (`lib/vagrant/box_collection.rb:208-212`). `vagrant box update` and
   `vagrant box outdated` re-fetch that URL. Keep routes stable; do not
   require a query string that expires.
7. **Updates only move forward.** `Box#has_update?` asks for `> current`
   and `compatible_version_update?` rejects downgrades, missing providers,
   and known-architecture → unknown-architecture transitions
   (`lib/vagrant/box.rb:189-208`; `box_metadata.rb:122-143`).

### 2.6 What we deliberately do not implement

- `/api/v1` or any `vagrant cloud` command endpoints (`search`, `publish`,
  `login`, `whoami`, provider upload, etc.).
- Binary upload, storage, proxying, or resizing.
- User accounts, teams, SSO, download counters.
- Box file validation or repackaging. The catalog may point at a broken
  box; that is the artifact owner’s problem.
- Automatic deletion/yanking semantics beyond removing entries from the
  catalog.

---

## 3. Source format

Options A and B use one YAML file per box. Option C accepts the equivalent
JSON through its admin API and can import the same YAML.

Layout:

```text
boxes/
  acme/
    base.yaml
    other.yaml
```

`boxes/acme/base.yaml`:

```yaml
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

      - name: virtualbox
        url: https://downloads.example.com/acme-base-1.2.0-vbox-arm64.box
        checksum_type: sha256
        checksum: 456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123
        architecture: arm64
        default_architecture: false

      - name: libvirt
        url: https://downloads.example.com/acme-base-1.2.0-libvirt.box
        architecture: amd64
        default_architecture: true

  - version: "1.1.0"
    providers:
      - name: virtualbox
        url: https://downloads.example.com/acme-base-1.1.0-vbox.box
        # No architecture keys: legacy/single-arch entry.
```

Rules:

- **Always quote versions.** YAML parses an unquoted `1.0` as a float. The
  loader must still preserve the original scalar text and reject invalid
  forms; quoting is the documented convention.
- The relative path without the extension must equal `name` when loaded via
  `LoadDir`. A file at `boxes/acme/base.yaml` may only declare
  `name: acme/base`.
- Versions and providers within a version are unordered; Vagrant sorts
  versions itself.

---

## 4. Shared Go package

All plans use Go 1.22+ so the standard library’s method-aware `ServeMux` is
available. A and B share this package; C uses the same types and validation
but persists them in SQLite.

```go
package catalog

type Catalog struct {
    Name        string    `yaml:"name" json:"name"`
    Description string    `yaml:"description,omitempty" json:"description,omitempty"`
    Versions    []Version `yaml:"versions" json:"versions"`
}

type Version struct {
    Version   string     `yaml:"version" json:"version"`
    Providers []Provider `yaml:"providers" json:"providers"`
}

type Provider struct {
    Name                string `yaml:"name" json:"name"`
    URL                 string `yaml:"url" json:"url"`
    Checksum            string `yaml:"checksum,omitempty" json:"checksum,omitempty"`
    ChecksumType        string `yaml:"checksum_type,omitempty" json:"checksum_type,omitempty"`
    Architecture        string `yaml:"architecture,omitempty" json:"architecture,omitempty"`
    DefaultArchitecture *bool  `yaml:"default_architecture,omitempty" json:"default_architecture,omitempty"`
}

func LoadDir(root string) ([]Catalog, error)   // recursive *.yaml / *.yml
func LoadFile(path string) (Catalog, error)
func Validate(c Catalog) error                 // aggregated errors
func Marshal(c Catalog) ([]byte, error)        // deterministic JSON + "\n"
func ValidGemVersion(s string) bool
```

`DefaultArchitecture` is a pointer because the JSON must distinguish:

- absent (legacy arch-less box),
- `false` (architecture-aware, not the default),
- `true` (architecture-aware default).

The loader must preserve the YAML scalar text for `version` (see the
`1.0` float trap). Implement a custom `UnmarshalYAML` for `Version` that
reads the scalar node value rather than decoding into a string field.

### 4.1 Validation rules

| Field | Rule |
|---|---|
| `name` | `^[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*$`; exactly one slash; no `..` segment; `LoadDir` additionally requires it to equal the source path without extension (`LoadFile` validates the document itself only) |
| `description` | optional; max 4096 bytes |
| `versions` | at least 1; versions unique; each valid `Gem::Version` |
| `version` | `^[0-9]+(\.[0-9A-Za-z]+)*$` |
| `providers` | at least 1 per version; at most 100 |
| provider `name` | `^[a-z][a-z0-9_]*$` |
| provider `url` | absolute `http` or `https` URL with a host; no userinfo (no `user:pass@`) |
| `checksum` / `checksum_type` | both present or both absent; type one of `md5`, `sha1`, `sha256`, `sha384`, `sha512`; checksum is hex of the matching length (32/40/64/96/128), case-insensitive |
| `architecture` | optional; `^[A-Za-z0-9_]+$`; the string `unknown` is allowed |
| provider identity | `(name, architecture)` unique within a version; an empty architecture is its own key |
| `default_architecture` | may be true only when `architecture` is set; at most one `true` per provider name within a version |
| path safety | rejected names must never be used to build a file path |

Validation returns **all** errors for a file, not just the first, so CI can
show a complete report.

### 4.2 Rendering rules

- Use `json.MarshalIndent` with two spaces and append one newline.
- Struct field order is the JSON field order; do not use maps for the
  top-level document.
- Omit empty `description`, checksum fields, and `architecture`.
- Emit `default_architecture` whenever `architecture` is non-empty:
  - pointer `true` → `true`
  - pointer `false` or `nil` → `false`
  - no `architecture` → omit entirely (legacy entry)
- Reject invalid catalogs before rendering; never publish a partial tree.
- Write to a temporary directory and swap it into place so a failed
  generation cannot leave a half-updated catalog.

---

## 5. Options

### Option A — Git-declared catalogs, static generation

A Go CLI reads `boxes/**/*.yaml`, validates, and writes a static tree.
Production has no application runtime:

```text
public/
  api/v2/vagrant/acme/base     # application/json, no extension
  acme/base                    # same bytes
```

Deployment targets:

- **S3 + CloudFront** (recommended for `VAGRANT_SERVER_URL`): S3 stores the
  `Content-Type` per object, so extensionless keys work. CloudFront provides
  HTTPS and a stable hostname. Publishing is `aws s3 sync` plus a
  CloudFront invalidation.
- **GitHub Pages**: cannot set `Content-Type` on extensionless files and
  has no header/rewrite configuration. It works only for the escape hatch:
  generate `catalogs/<org>/<box>.json` and use `config.vm.box_url` or a
  full catalog URL. It cannot serve `VAGRANT_SERVER_URL` shorthand.

Pros: no runtime, cheap, auditable in git, easy rollback (revert + sync).
Cons: publish latency equals a CI/deploy run; no admin UI; every catalog
change is a commit.

### Option B — Declarative files + read-only service

The same YAML files live on disk. A small Go service loads them into an
immutable snapshot, validates the whole tree, and serves both routes. A
reload (SIGHUP or ticker) builds a new snapshot and swaps it atomically;
invalid files leave the previous snapshot serving.

Pros: publishing is “write files + reload”, no redeploy; no database;
same authoring model as A; easy container/systemd deployment.
Cons: one service to run and monitor; catalogs must be copied to the
service host; no queryable history or UI.

### Option C — SQLite + admin API and UI

A Go service stores boxes, versions, and providers in SQLite. Public routes
render the same JSON from the database. Admin routes provide:

- `PUT /api/v1/admin/boxes/{org}/{box}` with a full catalog document
  (atomic replacement, ideal for CI),
- individual version add/delete endpoints,
- a minimal server-rendered admin UI for maintainers.

Pros: immediate publishing, queryable records, UI for non-engineers,
audit-friendly `created_at`/`updated_at`.
Cons: database, migrations, backups, auth, CSRF, and more code. SQLite is
single-node; horizontal scaling is out of scope.

### Comparison

| | A static | B file service | C database |
|---|---|---|---|
| Authoring | Git YAML | Git YAML copied to host | API or UI |
| Publish latency | CI/deploy minutes | seconds (reload) | immediate |
| Runtime | none (CDN only) | one small service | service + SQLite |
| Ops burden | lowest | low | medium |
| Multi-editor | via Git | via Git | yes |
| Query/audit | Git history | Git history | DB timestamps |
| Failure mode | stale deploy | bad reload keeps last good | DB/migration issues |
| Best for | small, slow-moving catalog | CI publish without redeploy | UI/many maintainers |

### Recommendation

**A** is the default for the stated goal. The catalog is small and changes
infrequently; a static tree on S3 + CloudFront has the smallest attack and
operations surface, and exactly satisfies `VAGRANT_SERVER_URL`.

Choose **B** when CI must publish a new version without waiting for a
static-site deploy, or when catalogs live outside the registry repository.
Choose **C** only when a browser UI or many non-engineer editors are a real
requirement; otherwise the database and auth surface are unjustified.

### Migration path

- A → B: point B at the same `boxes/` directory; switch DNS/`VAGRANT_SERVER_URL`.
- B → C: import the YAML with `PUT /api/v1/admin/boxes/{org}/{box}` for each
  catalog; keep the same public routes and contract.
- All options share the same validation rules, so a catalog that passes A
  also passes B/C.

---

## 6. Security and operations

- **TLS everywhere.** `VAGRANT_SERVER_URL` should be `https://`. Checksums
  protect artifacts; TLS protects the catalog that supplies them.
- **No credentials in provider URLs.** Reject `user:pass@` in catalog URLs.
  Use presigned URLs or artifact-store credentials on the client side.
- **Public catalogs are public.** Do not put private artifact URLs in a
  public catalog. If the catalog is private, put the whole registry behind
  auth; provider URLs must be independently accessible.
- **Admin surface (C only).** Bearer token for API, Basic auth for UI,
  CSRF protection on UI POSTs, `SameSite=Strict` cookies, body-size limits,
  and constant-time token comparison. If `REGISTRY_ADMIN_TOKEN` is unset,
  admin routes return 404.
- **Path traversal.** Validate names before using them in filesystem paths.
- **Caching.** Use a short TTL (`public, max-age=60`) or no-cache for the
  catalog. Vagrant itself caches outdated checks client-side for one hour.
- **Observability.** Structured logs (`slog`), request method/path/status,
  reload success/failure (B), migration and admin actions (C). Health
  endpoint must not require auth but must not expose secrets.
- **Backups (C).** SQLite WAL mode; back up with `sqlite3 registry.db
  ".backup backup.db"` or `VACUUM INTO` on a schedule. Document restore.

---

## 7. Decisions taken in this design

- Go 1.22+ for all runtime/generator code; standard library HTTP and
  templates; `gopkg.in/yaml.v3` for YAML; `modernc.org/sqlite` for C.
- One YAML file per box; path must match `name`.
- Public contract is the two routes only; no `/api/v1` public API.
- Options A and B use the same catalog package and validation.
- Option C’s full-document `PUT` is the canonical write path; individual
  version endpoints and the UI are conveniences on top of it.
- Version strings are quoted in docs and preserved from YAML scalar text.
- `default_architecture` is emitted as `false` for non-default,
  architecture-aware providers to avoid Vagrant’s legacy fallback.

## 8. Review focus seeds

These are the failures most likely to slip through because no single task
naturally tests them. Each plan adds the matching test in the task that owns
the behavior.

1. Catalog `name` does not match the file path or the requested shorthand.
2. Architecture-aware provider without an explicit
   `default_architecture: false`.
3. Unquoted YAML version such as `1.0` losing precision.
4. Removed boxes/versions remaining in a previously generated output tree.
5. Static upload without `Content-Type: application/json` on `HEAD`.
6. Invalid reload replacing a known-good snapshot (B).
7. `HEAD` returning a body or the wrong `Content-Length` (B/C).
8. Path traversal in a URL segment (B/C).
9. Non-atomic admin `PUT` leaving partial versions (C).
10. Admin auth/CSRF bypass (C).
