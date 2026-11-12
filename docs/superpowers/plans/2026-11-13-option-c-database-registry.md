# Option C — Database Registry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run a registry with SQLite-backed boxes, versions, and providers; a public catalog API compatible with `VAGRANT_SERVER_URL`; an admin API for CI; and a minimal admin UI for maintainers.

**Architecture:** A single Go service stores normalized records in SQLite. The public routes render the same catalog JSON as Options A/B. Admin writes use a full-document `PUT` (atomic replacement) plus individual version endpoints for the UI. A server-rendered admin UI is protected by HTTP Basic auth and a double-submit CSRF cookie. No binaries are hosted or proxied.

**Tech Stack:** Go 1.22+, `gopkg.in/yaml.v3`, `modernc.org/sqlite` (pure Go), standard library `net/http` and `html/template`, `log/slog`; SQLite WAL mode; Docker.

**Spec:** `../specs/2026-11-13-vagrant-registry-design.md`

## Global Constraints

- Go 1.22 or newer; module path `github.com/rmoriz/vagrant-registry`.
- Dependencies: `gopkg.in/yaml.v3`, `modernc.org/sqlite`. No web framework.
- SQLite is the only database; single-node deployment. Postgres is out of scope.
- Public routes require no auth. Admin routes require `REGISTRY_ADMIN_TOKEN`.
- If `REGISTRY_ADMIN_TOKEN` is empty, admin routes are not mounted and return 404.
- All admin responses use `Cache-Control: no-store`.
- `PUT /api/v1/admin/boxes/{org}/{box}` is atomic: either the whole document replaces the box or nothing changes.
- Version strings are validated as `Gem::Version`; names are validated before any database or filesystem use.
- `default_architecture` is reconstructed only for architecture-aware providers; legacy entries keep it absent.
- If the directory is not already a Git repository, run `git init` in Task 1.

## Review Focus

- A failed full-document `PUT` must leave the previous box/versions untouched.
- Admin auth must not reveal whether the token is unset; unset means 404, wrong token means 401.
- UI POSTs without a valid CSRF token must be rejected.
- Deleting a box must cascade to versions and providers.
- Concurrent upserts must not corrupt the database or return `database is locked` under normal load.

---

### Task 1: Module and catalog core

**Files:**
- Create: `go.mod`
- Create: `.gitignore`
- Create: `internal/catalog/catalog.go`
- Create: `internal/catalog/version.go`
- Create: `internal/catalog/load.go`
- Create: `internal/catalog/validate.go`
- Create: `internal/catalog/render.go`
- Create: `internal/catalog/version_test.go`
- Create: `internal/catalog/load_test.go`
- Create: `internal/catalog/validate_test.go`
- Create: `internal/catalog/render_test.go`

**Interfaces:**
- Consumes: nothing.
- Produces: the shared package from spec §4: types, `LoadDir`, `LoadFile`, `Validate`, `Marshal`, `ValidGemVersion`.

- [ ] **Step 1: Initialize the module**

```bash
cd /path/to/vagrant-registry
git init  # only if not already a repository
go mod init github.com/rmoriz/vagrant-registry
printf 'bin/\n*.db\n*.db-shm\n*.db-wal\n.DS_Store\n' > .gitignore
go get gopkg.in/yaml.v3@v3.0.1
go get modernc.org/sqlite@latest
```

- [ ] **Step 2: Write the failing catalog tests**

Create the same tests as Option B, Task 1:

- `TestValidGemVersion`.
- `TestLoadDirLoadsAndSortsCatalogs`.
- `TestLoadFilePreservesUnquotedVersionText`.
- `TestLoadDirRejectsNamePathMismatch` (a file at `acme/base.yaml` declaring `acme/wrong`).
- `TestValidateAcceptsValidCatalog` and `TestValidateRejects` covering every spec §4.1 row.
- `TestMarshalEmitsDefaultArchitectureFalseForNonDefault`.
- `TestMarshalOmitsDefaultArchitectureForLegacyProvider`.

- [ ] **Step 3: Run tests and verify failure**

Run: `go test ./internal/catalog -v`
Expected: FAIL with undefined types/functions.

- [ ] **Step 4: Implement the shared package**

Implement the decisions from spec §4:

- Structs exactly as specified, with `DefaultArchitecture *bool`.
- `ValidGemVersion` regex `^[0-9]+(?:\.[0-9A-Za-z]+)*$`.
- `Version.UnmarshalYAML` preserves the scalar node value.
- `LoadDir` walks `*.yaml`/`*.yml`, enforces path/name equality, sorts by name.
- `Validate` aggregates all errors and implements every spec §4.1 row.
- `Marshal` validates, normalizes `DefaultArchitecture` to `false` for architecture-aware non-defaults, appends a newline.

- [ ] **Step 5: Run tests and commit**

Run: `go test ./internal/catalog -v`
Expected: PASS.

```bash
git add go.mod go.sum .gitignore internal/catalog
git commit -m "feat: add shared catalog types, loader, validation, rendering"
```

---

### Task 2: SQLite store and migrations

**Files:**
- Create: `internal/store/store.go`
- Create: `internal/store/sqlite.go`
- Create: `internal/store/migrations/001_init.sql`
- Create: `internal/store/sqlite_test.go`

**Interfaces:**
- Consumes: `catalog.Catalog`, `catalog.Validate`.
- Produces:

```go
package store

var (
	ErrNotFound = errors.New("not found")
	ErrConflict = errors.New("conflict")
)

type BoxSummary struct {
	Org         string
	Box         string
	Name        string
	Description string
	Versions    int
	UpdatedAt   time.Time
}

type Store interface {
	GetCatalog(ctx context.Context, org, box string) (*catalog.Catalog, error)
	ListBoxes(ctx context.Context) ([]BoxSummary, error)
	UpsertCatalog(ctx context.Context, c catalog.Catalog) error
	DeleteCatalog(ctx context.Context, org, box string) error
	AddVersion(ctx context.Context, org, box string, v catalog.Version) error
	DeleteVersion(ctx context.Context, org, box, version string) error
	Ping(ctx context.Context) error
	Close() error
}

func Open(path string) (*SQLiteStore, error)
```

- [ ] **Step 1: Write the failing store tests**

Create `internal/store/sqlite_test.go`:

```go
package store

import (
	"context"
	"errors"
	"path/filepath"
	"sync"
	"testing"

	"github.com/rmoriz/vagrant-registry/internal/catalog"
)

func testCatalog(name string) catalog.Catalog {
	def := true
	return catalog.Catalog{
		Name: name,
		Versions: []catalog.Version{{
			Version: "1.0.0",
			Providers: []catalog.Provider{{
				Name: "virtualbox", URL: "https://e/x.box",
				Architecture: "amd64", DefaultArchitecture: &def,
			}},
		}},
	}
}

func openTestStore(t *testing.T) *SQLiteStore {
	t.Helper()
	s, err := Open(filepath.Join(t.TempDir(), "registry.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = s.Close() })
	return s
}

func TestUpsertAndGetCatalog(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()
	if err := s.UpsertCatalog(ctx, testCatalog("acme/base")); err != nil {
		t.Fatal(err)
	}
	c, err := s.GetCatalog(ctx, "acme", "base")
	if err != nil {
		t.Fatal(err)
	}
	if c.Name != "acme/base" || len(c.Versions) != 1 {
		t.Fatalf("unexpected catalog: %+v", c)
	}
	if c.Versions[0].Providers[0].DefaultArchitecture == nil || !*c.Versions[0].Providers[0].DefaultArchitecture {
		t.Fatal("default architecture not preserved")
	}
}

func TestUpsertIsAtomicOnInvalidCatalog(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()
	if err := s.UpsertCatalog(ctx, testCatalog("acme/base")); err != nil {
		t.Fatal(err)
	}
	bad := testCatalog("acme/base")
	bad.Versions[0].Version = "nope"
	if err := s.UpsertCatalog(ctx, bad); err == nil {
		t.Fatal("expected validation error")
	}
	c, err := s.GetCatalog(ctx, "acme", "base")
	if err != nil {
		t.Fatal(err)
	}
	if c.Versions[0].Version != "1.0.0" {
		t.Fatalf("old catalog changed: %+v", c.Versions[0])
	}
}

func TestDeleteCatalogCascades(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()
	if err := s.UpsertCatalog(ctx, testCatalog("acme/base")); err != nil {
		t.Fatal(err)
	}
	if err := s.DeleteCatalog(ctx, "acme", "base"); err != nil {
		t.Fatal(err)
	}
	if _, err := s.GetCatalog(ctx, "acme", "base"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("err = %v, want ErrNotFound", err)
	}
	var count int
	if err := s.db.QueryRow("SELECT COUNT(*) FROM providers").Scan(&count); err != nil {
		t.Fatal(err)
	}
	if count != 0 {
		t.Fatalf("providers = %d, want 0", count)
	}
}

func TestAddAndDeleteVersion(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()
	if err := s.UpsertCatalog(ctx, testCatalog("acme/base")); err != nil {
		t.Fatal(err)
	}
	v := catalog.Version{
		Version:   "2.0.0",
		Providers: []catalog.Provider{{Name: "libvirt", URL: "https://e/y.box"}},
	}
	if err := s.AddVersion(ctx, "acme", "base", v); err != nil {
		t.Fatal(err)
	}
	if err := s.AddVersion(ctx, "acme", "base", v); !errors.Is(err, ErrConflict) {
		t.Fatalf("err = %v, want ErrConflict", err)
	}
	if err := s.DeleteVersion(ctx, "acme", "base", "2.0.0"); err != nil {
		t.Fatal(err)
	}
	if err := s.DeleteVersion(ctx, "acme", "base", "2.0.0"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("err = %v, want ErrNotFound", err)
	}
}

func TestConcurrentUpserts(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()
	var wg sync.WaitGroup
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			if err := s.UpsertCatalog(ctx, testCatalog("acme/base")); err != nil {
				t.Errorf("upsert: %v", err)
			}
		}()
	}
	wg.Wait()
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `go test ./internal/store -v`
Expected: FAIL with `undefined: Open`.

- [ ] **Step 3: Write the migration**

Create `internal/store/migrations/001_init.sql`:

```sql
CREATE TABLE boxes (
    id          INTEGER PRIMARY KEY,
    org         TEXT NOT NULL,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (org, name)
);

CREATE TABLE versions (
    id       INTEGER PRIMARY KEY,
    box_id   INTEGER NOT NULL REFERENCES boxes(id) ON DELETE CASCADE,
    version  TEXT NOT NULL,
    UNIQUE (box_id, version)
);

CREATE TABLE providers (
    id                    INTEGER PRIMARY KEY,
    version_id            INTEGER NOT NULL REFERENCES versions(id) ON DELETE CASCADE,
    name                  TEXT NOT NULL,
    url                   TEXT NOT NULL,
    checksum              TEXT NOT NULL DEFAULT '',
    checksum_type         TEXT NOT NULL DEFAULT '',
    architecture          TEXT NOT NULL DEFAULT '',
    default_architecture  INTEGER NOT NULL DEFAULT 0,
    UNIQUE (version_id, name, architecture)
);

CREATE INDEX idx_versions_box ON versions(box_id);
CREATE INDEX idx_providers_version ON providers(version_id);
```

- [ ] **Step 4: Implement the store**

Create `internal/store/store.go` with the interface and errors.

Create `internal/store/sqlite.go`:

- `type SQLiteStore struct { db *sql.DB }`, `Open(path string) (*SQLiteStore, error)`:
  - Blank-import `modernc.org/sqlite` in this file so the `sqlite` driver is registered.
  - `sql.Open("sqlite", path)`.
  - `db.SetMaxOpenConns(1)` (simple and safe for a small single-node registry).
  - Execute `PRAGMA journal_mode=WAL`, `PRAGMA foreign_keys=ON`, `PRAGMA busy_timeout=5000`.
  - Call `Migrate(db)`; close and return on error.
- `Migrate(db *sql.DB) error`:
  - `CREATE TABLE IF NOT EXISTS schema_migrations (version TEXT PRIMARY KEY, applied_at TEXT NOT NULL)`.
  - Embed `migrations/*.sql` with `//go:embed`.
  - Sort filenames; skip applied versions; run each in a transaction and record it.
  - Test applying twice (add `TestMigrateIdempotent`).
- `GetCatalog`:
  - `SELECT id, description FROM boxes WHERE org=? AND name=?`; `sql.ErrNoRows` → `ErrNotFound`.
  - `SELECT id, version FROM versions WHERE box_id=? ORDER BY id`; scan **all** rows into a slice and close the rows before querying providers. With `SetMaxOpenConns(1)`, nested queries while rows are open will deadlock.
  - For each collected version, `SELECT name, url, checksum, checksum_type, architecture, default_architecture FROM providers WHERE version_id=? ORDER BY name, architecture`.
  - Reconstruct `DefaultArchitecture`: if `architecture == ""` leave nil; otherwise set a pointer to `default_architecture != 0`.
- `ListBoxes`: left join versions, group by box, order by org, name.
- `UpsertCatalog`:
  - `catalog.Validate(c)` first; return the error.
  - Split `c.Name` at the single slash.
  - Begin transaction; `INSERT INTO boxes ... ON CONFLICT(org,name) DO UPDATE SET description=excluded.description, updated_at=excluded.updated_at`; `SELECT id`.
  - `DELETE FROM versions WHERE box_id=?` (providers cascade).
  - Insert versions and providers; convert `DefaultArchitecture` to 0/1.
  - Commit; roll back on any error.
- `DeleteCatalog`: `DELETE FROM boxes WHERE org=? AND name=?`; rows affected 0 → `ErrNotFound`.
- `AddVersion`: transaction; find box id (`ErrNotFound` if absent); insert version; `ErrConflict` if the unique constraint fails (detect via `sqlite` error code or just check `SELECT COUNT` first inside the tx); insert providers.
- `DeleteVersion`: `DELETE FROM versions WHERE box_id=(SELECT id...) AND version=?`; rows 0 → `ErrNotFound`.
- `Ping`: `SELECT 1`.
- `Close`: `db.Close()`.

Add a test `TestMigrateIdempotent` that calls `Migrate` twice.

- [ ] **Step 5: Run tests and commit**

Run:

```bash
go test ./internal/store -v
go test -race ./internal/store
```

Expected: PASS, no race reports.

```bash
git add internal/store
git commit -m "feat: add SQLite store and migrations"
```

---

### Task 3: Public catalog handler

**Files:**
- Create: `internal/httpapi/catalog.go`
- Create: `internal/httpapi/catalog_test.go`

**Interfaces:**
- Consumes: `store.Store`, `catalog.Marshal`.
- Produces:

```go
type Options struct {
	CacheControl string
}

func NewPublicHandler(s store.Store, opts Options) http.Handler
```

- [ ] **Step 1: Write the failing tests**

Create `internal/httpapi/catalog_test.go` with a fake store that returns a fixed catalog:

- `TestPublicServesBothRoutes`: `GET /api/v2/vagrant/acme/base` and `GET /acme/base` return 200, `Content-Type: application/json`, identical bodies.
- `TestPublicHead`: `HEAD /acme/base` returns 200, no body, non-empty `Content-Length`.
- `TestPublicNotFound`: missing box → 404.
- `TestPublicTraversal`: `/../etc/passwd`, `/acme/%2e%2e`, `/api/v2/vagrant/acme/%2e%2e` → 404.
- `TestPublicMethodNotAllowed`: `POST /acme/base` → 405 with `Allow: GET, HEAD`.

- [ ] **Step 2: Run tests and verify failure**

Run: `go test ./internal/httpapi -v`
Expected: FAIL with `undefined: NewPublicHandler`.

- [ ] **Step 3: Implement the handler**

Create `internal/httpapi/catalog.go`:

- Wrap the mux in a top-level handler that rejects traversal before the mux
  can clean or redirect it. Inspect `r.URL.EscapedPath()`; if it contains
  `..`, `%2e%2e`, `%2f`, or `%5c` (case-insensitive), return 404.
- Use `http.NewServeMux` with `GET`/`HEAD` patterns for both routes.
- Validate `{org}` and `{box}` with the spec name regex and reject `.`/`..`/slashes/backslashes.
- `store.GetCatalog`; `ErrNotFound` → 404 text/plain; other → 500.
- Marshal; set `Content-Type`, `Content-Length`, `Cache-Control` (if non-empty), `X-Content-Type-Options: nosniff`.
- `HEAD` writes headers only.
- Add `GET /healthz` that calls `store.Ping`; 200 `ok` or 503.

- [ ] **Step 4: Run tests and commit**

Run: `go test ./internal/httpapi -v`
Expected: PASS.

```bash
git add internal/httpapi
git commit -m "feat: serve public catalog from SQLite"
```

---

### Task 4: Admin authentication and API

**Files:**
- Create: `internal/httpapi/admin.go`
- Create: `internal/httpapi/admin_test.go`

**Interfaces:**
- Consumes: `store.Store`, `catalog.Validate`, `catalog.Marshal`.
- Produces:

```go
type AdminOptions struct {
	Token string // empty disables admin routes
}

func NewAdminHandler(s store.Store, opts AdminOptions) http.Handler
```

- [ ] **Step 1: Write the failing tests**

Create `internal/httpapi/admin_test.go`:

- `TestAdminDisabledReturns404`: empty token, `GET /api/v1/admin/boxes` → 404.
- `TestAdminRejectsMissingToken`: token set, no header → 401.
- `TestAdminRejectsWrongToken`: `Bearer wrong` → 401.
- `TestAdminAcceptsBearer`: `Bearer test-token` → 200.
- `TestAdminAcceptsBasic`: `admin:test-token` → 200.
- `TestAdminPutReplacesCatalog`: `PUT /api/v1/admin/boxes/acme/base` with valid JSON → 200; subsequent `GET` returns the new versions.
- `TestAdminPutRejectsNameMismatch`: URL `acme/base`, body name `acme/wrong` → 400; store unchanged.
- `TestAdminPutRejectsInvalidBody`: invalid JSON → 400; invalid catalog → 400; store unchanged.
- `TestAdminDelete`: `DELETE` → 204; subsequent `GET` → 404.
- `TestAdminAddVersion`: `POST .../versions` → 201; duplicate → 409.
- `TestAdminDeleteVersion`: `DELETE .../versions/1.0.0` → 204; missing → 404.
- `TestAdminBodyLimit`: body larger than 1 MiB → 400/413.

- [ ] **Step 2: Run tests and verify failure**

Run: `go test ./internal/httpapi -run TestAdmin -v`
Expected: FAIL with `undefined: NewAdminHandler`.

- [ ] **Step 3: Implement auth**

- If `opts.Token == ""`, return an `http.Handler` that always returns 404.
- Middleware:
  - `Authorization: Bearer <token>` → constant-time compare with `opts.Token`.
  - Otherwise `r.BasicAuth()`; username must be `admin` and password must match via constant-time compare.
  - On failure, set `WWW-Authenticate: Basic realm="vagrant-registry"` and return 401.
- Only mount admin routes after auth.

- [ ] **Step 4: Implement the API**

Routes:

```text
GET    /api/v1/admin/boxes
GET    /api/v1/admin/boxes/{org}/{box}
PUT    /api/v1/admin/boxes/{org}/{box}
DELETE /api/v1/admin/boxes/{org}/{box}
POST   /api/v1/admin/boxes/{org}/{box}/versions
DELETE /api/v1/admin/boxes/{org}/{box}/versions/{version}
```

Behavior:

- `PUT`: `http.MaxBytesReader` 1 MiB; `json.Decoder` with `DisallowUnknownFields`; decode into `catalog.Catalog`; require `c.Name == org+"/"+box`; `catalog.Validate`; `store.UpsertCatalog`; respond 200 with the canonical `catalog.Marshal` bytes.
- `GET` one: `store.GetCatalog`; 404 on `ErrNotFound`; canonical JSON.
- `GET` list: `store.ListBoxes`; JSON array of `{name, description, versions, updated_at}`.
- `DELETE` box: 204.
- `POST` version: decode `catalog.Version`; build `catalog.Catalog{Name: name, Versions: []catalog.Version{v}}` and validate; `store.AddVersion`; 201; `ErrConflict` → 409.
- `DELETE` version: 204; `ErrNotFound` → 404.
- All admin responses: `Cache-Control: no-store`, `Content-Type: application/json` except 204.
- Error response body: `{"error":"..."}`; never include SQL text or tokens.

- [ ] **Step 5: Run tests and commit**

Run: `go test ./internal/httpapi -run TestAdmin -v`
Expected: PASS.

```bash
git add internal/httpapi
git commit -m "feat: add authenticated admin API"
```

---

### Task 5: Admin UI and CSRF protection

**Files:**
- Create: `internal/web/ui.go`
- Create: `internal/web/csrf.go`
- Create: `internal/web/templates/layout.html`
- Create: `internal/web/templates/index.html`
- Create: `internal/web/templates/box.html`
- Create: `internal/web/ui_test.go`
- Create: `internal/web/csrf_test.go`

**Interfaces:**
- Consumes: `store.Store`, `store.BoxSummary`, `catalog.Version`, `catalog.Validate`.
- Produces:

```go
type Options struct {
	AdminToken   string
	CookieSecure bool
}

func NewHandler(s store.Store, opts Options) http.Handler

type csrf struct{ secure bool }

func newCSRF(secure bool) *csrf
func (c *csrf) Ensure(w http.ResponseWriter, r *http.Request) string
func (c *csrf) Validate(r *http.Request) bool
```

- [ ] **Step 1: Write the failing CSRF tests**

`internal/web/csrf_test.go`:

- `TestCSRFEnsureCreatesCookie`: `newCSRF(true).Ensure(...)` sets `registry_csrf` with `HttpOnly`, `SameSite=Strict`, `Secure` true.
- `TestCSRFValidateAcceptsMatchingToken`.
- `TestCSRFValidateRejectsMissing`, `RejectsMismatch`, `RejectsEmptyCookie`.

- [ ] **Step 2: Write the failing UI tests**

`internal/web/ui_test.go`:

- `TestUIIndexRequiresAuth`: no Basic auth → 401.
- `TestUIIndexListsBoxes`: with auth, body contains the box name.
- `TestUIPostWithoutCSRFReturns403`.
- `TestUIPostWithCSRFAddsVersion`: seed the store with a box; GET the box page, extract the `csrf` value and cookie, POST the form, assert the version is in the store.
- `TestUIEscapesDescription`: seed a box whose description contains `<script>`; the rendered page contains the escaped text and not the raw tag.
- `TestUISecurityHeaders`: `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`.

- [ ] **Step 3: Run tests and verify failure**

Run: `go test ./internal/web -v`
Expected: FAIL with undefined types/functions.

- [ ] **Step 4: Implement CSRF**

`internal/web/csrf.go`:

- `type csrf struct{ secure bool }`; `newCSRF(secure bool)` returns it.
- Cookie name `registry_csrf`; 32 random bytes from `crypto/rand`, `base64.RawURLEncoding`.
- `Ensure(w, r) string`: if the cookie exists, return its value; otherwise generate, set `Path=/admin`, `HttpOnly: true`, `SameSite: http.SameSiteStrictMode`, `Secure: c.secure`, `MaxAge: 8h`, and return the value.
- `Validate(r) bool`: `r.ParseForm()`; read the cookie; compare `r.FormValue("csrf")` to the cookie value with `subtle.ConstantTimeCompare`; require both non-empty.
- No HMAC is needed for double-submit because the cookie is random and `SameSite=Strict`; the hidden field proves the form came from a page that could read the cookie value from the server-rendered HTML.

- [ ] **Step 5: Implement the UI**

`internal/web/ui.go`:

- If `opts.AdminToken == ""`, return an `http.Handler` that always returns 404 (the caller should also avoid mounting it).
- Build `c := newCSRF(opts.CookieSecure)` and use `c.Ensure`/`c.Validate`.
- Embed templates with `//go:embed templates/*.html`.
- Routes (all behind Basic auth, using the same `admin:<token>` check as the API):
  - `GET /admin` — list boxes (`store.ListBoxes`).
  - `GET /admin/boxes/{org}/{box}` — show versions/providers; render a “delete box” form and an “add version” form.
  - `POST /admin/boxes/{org}/{box}/versions` — parse `version` and `providers_json` (a JSON array of providers); build `catalog.Version`; validate via a one-version `catalog.Catalog`; `store.AddVersion`; redirect to the box page (303).
  - `POST /admin/boxes/{org}/{box}/delete` — `store.DeleteCatalog`; redirect to `/admin`.
- All POSTs call `csrf.Validate`; failure → 403.
- `GET` handlers call `c.Ensure(w, r)` and pass the returned token to the template as `csrf` so the form can carry it.
- Templates are `html/template` (auto-escaping). No inline JavaScript.
- Add a middleware that sets:
  - `X-Content-Type-Options: nosniff`
  - `X-Frame-Options: DENY`
  - `Referrer-Policy: no-referrer`
  - `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; form-action 'self'`
- Keep CSS in a `<style>` block; no external assets.

- [ ] **Step 6: Run tests and commit**

Run:

```bash
go test ./internal/web -v
go test -race ./internal/web
```

Expected: PASS.

```bash
git add internal/web
git commit -m "feat: add admin UI with CSRF protection"
```

---

### Task 6: Configuration and service lifecycle

**Files:**
- Create: `internal/config/config.go`
- Create: `internal/config/config_test.go`
- Create: `cmd/registry/main.go`
- Create: `cmd/registry/main_test.go`

**Interfaces:**
- Consumes: `store.Open`, `httpapi.NewPublicHandler`, `httpapi.NewAdminHandler`, `web.NewHandler`.
- Produces: `func run(ctx context.Context, args []string) int`; `Config` with `Addr`, `DBPath`, `CacheControl`, `CookieSecure`, `AdminToken`.

- [ ] **Step 1: Write the failing config tests**

`internal/config/config_test.go`:

- `TestParseDefaults`: `Addr == "127.0.0.1:8080"`, `DBPath == "./registry.db"`, `CacheControl == "public, max-age=60"`, `CookieSecure == true`.
- `TestParseFlags`: `-addr :9000 -db /tmp/x.db -cache-control no-store -cookie-secure=false`.
- `TestAdminTokenFromEnv`: set `REGISTRY_ADMIN_TOKEN`, assert `Config.AdminToken`.
- `TestMissingAdminTokenIsAllowed`: empty env, `AdminToken == ""`.

- [ ] **Step 2: Run tests and verify failure**

Run: `go test ./internal/config -v`
Expected: FAIL with `undefined: Parse`.

- [ ] **Step 3: Implement config**

`internal/config/config.go`:

- `flag.NewFlagSet("registry", flag.ContinueOnError)`.
- `-addr` default `127.0.0.1:8080`.
- `-db` default `./registry.db`.
- `-cache-control` default `public, max-age=60`.
- `-cookie-secure` default `true`.
- `AdminToken` from `os.Getenv("REGISTRY_ADMIN_TOKEN")`; do not accept it as a flag.
- Return parse errors.

- [ ] **Step 4: Write the failing main tests**

`cmd/registry/main_test.go`:

- `TestRunFailsOnUnwritableDB`: `-db /does/not/exist/registry.db` → non-zero.
- `TestRunServesHealthAndShutsDown`: temp DB, `-addr 127.0.0.1:0`, context timeout; expect 0.
- `TestAdminDisabledWhenTokenEmpty`: start with empty `REGISTRY_ADMIN_TOKEN`; `GET /api/v1/admin/boxes` → 404.
- `TestAdminEnabledWithToken`: set token; `GET` without auth → 401.

For the HTTP tests, factor a `newServer(ctx, cfg)` helper that returns `*httptest.Server` so tests do not need a fixed port.

- [ ] **Step 5: Implement `main`**

`cmd/registry/main.go`:

- `run(ctx, args) int` parses config, opens the store (migrates), builds a mux:
  - public handler at `/`,
  - admin API at `/api/v1/admin/`,
  - admin UI on both `/admin` and `/admin/` if the token is non-empty (mounting only `/admin/` would make the index unreachable).
- If the token is empty, log `admin disabled: REGISTRY_ADMIN_TOKEN not set` at warn level; do not mount admin routes.
- Use `http.Server` with `ReadHeaderTimeout: 5s`, `IdleTimeout: 60s`.
- Graceful shutdown on `SIGINT`/`SIGTERM` or `ctx.Done()` with a 10-second timeout.
- Log method/path/status/duration with `slog`.
- `/healthz` pings the DB; 200 or 503.

- [ ] **Step 6: Run tests and commit**

Run:

```bash
gofmt -l .
go vet ./...
go test ./...
go test -race ./...
```

Expected: no `gofmt` output, all tests pass.

```bash
git add internal/config cmd/registry
git commit -m "feat: add registry lifecycle, config, and health"
```

---

### Task 7: Import command for migration

**Files:**
- Create: `cmd/registry-import/main.go`
- Create: `cmd/registry-import/main_test.go`

**Interfaces:**
- Consumes: `catalog.LoadDir`, `store.Open`, `store.UpsertCatalog`.
- Produces: `registry-import -src ./boxes -db ./registry.db`, which imports every YAML catalog and prints the count.

- [ ] **Step 1: Write the failing test**

`cmd/registry-import/main_test.go`:

- `TestImportLoadsCatalogs`: temp YAML dir and temp DB; run `run([]string{"-src", dir, "-db", db}, &out, &errb)`; assert code 0, then open the DB with `store.Open`, call `GetCatalog("acme","base")`, and close it.
- `TestImportFailsOnInvalidCatalog`: invalid YAML; code 1; DB unchanged.
- `TestImportIsIdempotent`: run twice; code 0 both times; one box remains.

- [ ] **Step 2: Run test and verify failure**

Run: `go test ./cmd/registry-import -v`
Expected: FAIL with `undefined: run`.

- [ ] **Step 3: Implement the importer**

`cmd/registry-import/main.go`:

- Flags `-src` (default `./boxes`), `-db` (default `./registry.db`).
- `catalog.LoadDir`; if it fails, print all errors and return 1.
- `store.Open`; import each catalog with `UpsertCatalog`.
- Print `imported N catalogs into <db>`.
- Return 0; close the store on all paths.

- [ ] **Step 4: Run test and commit**

Run: `go test ./cmd/registry-import -v`
Expected: PASS.

```bash
git add cmd/registry-import
git commit -m "feat: add YAML import command"
```

---

### Task 8: Container, backups, and smoke test

**Files:**
- Create: `Dockerfile`
- Create: `.dockerignore`
- Create: `docker-compose.yml`
- Create: `deploy/backup.md`
- Create: `scripts/smoke.sh`
- Create: `docs/usage.md` (Option C usage; keep the plan index in `README.md` and add a link)
- Create: `boxes/acme/base.yaml` (copy the example from spec §3)

**Interfaces:**
- Consumes: the `registry` binary, a SQLite database path, and `REGISTRY_ADMIN_TOKEN`.
- Produces: a deployable container and a repeatable smoke test.

- [ ] **Step 1: Write the Dockerfile**

```dockerfile
FROM golang:1.22 AS build
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/registry ./cmd/registry
RUN mkdir -p /out/data

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/registry /registry
COPY --from=build --chown=65532:65532 /out/data /data
VOLUME ["/data"]
USER nonroot:nonroot
ENTRYPOINT ["/registry"]
CMD ["-db", "/data/registry.db", "-addr", ":8080"]
```

`.dockerignore`: `.git`, `bin`, `*.db`, `*.db-shm`, `*.db-wal`, `docs`.

- [ ] **Step 2: Write docker-compose**

`docker-compose.yml`:

```yaml
services:
  registry:
    build: .
    ports:
      - "127.0.0.1:8080:8080"
    environment:
      REGISTRY_ADMIN_TOKEN: ${REGISTRY_ADMIN_TOKEN:?set REGISTRY_ADMIN_TOKEN}
    volumes:
      - registry-data:/data
    restart: unless-stopped

volumes:
  registry-data:
```

- [ ] **Step 3: Write backup instructions**

`deploy/backup.md`:

- Back up with `sqlite3 /data/registry.db ".backup '/backup/registry-$(date +%F).db'"`.
- Or `VACUUM INTO '/backup/registry.db'`.
- Quiesce writes or use the backup command; do not copy the `.db` file while WAL is active without also copying `-wal`/`-shm`.
- Restore: stop the service, replace `registry.db`, remove stale `-wal`/`-shm`, start the service.
- Test restore on a schedule.

- [ ] **Step 4: Write the smoke script**

`scripts/smoke.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

base="${1:-http://127.0.0.1:8080}"
token="${REGISTRY_ADMIN_TOKEN:-}"

status=$(curl -sS -o /dev/null -w '%{http_code}' -I "$base/api/v2/vagrant/acme/base")
[ "$status" = "200" ] || { echo "public HEAD returned $status" >&2; exit 1; }

ctype=$(curl -sSI "$base/api/v2/vagrant/acme/base" | tr -d '\r' | awk -F': ' 'tolower($1)=="content-type" {print $2}')
case "$ctype" in application/json*) ;; *) echo "content-type '$ctype'" >&2; exit 1 ;; esac

body=$(curl -sS "$base/acme/base")
printf '%s' "$body" | grep -Eq '"name"[[:space:]]*:[[:space:]]*"acme/base"'

if [ -n "$token" ]; then
  code=$(curl -sS -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $token" "$base/api/v1/admin/boxes")
  [ "$code" = "200" ] || { echo "admin GET returned $code" >&2; exit 1; }
fi

echo "ok: $base"
```

Make it executable: `chmod +x scripts/smoke.sh`.

- [ ] **Step 5: Write `docs/usage.md` and link it from `README.md`

Document:

- Build and run: `go build -o bin/registry ./cmd/registry`; `REGISTRY_ADMIN_TOKEN=... ./bin/registry -db ./registry.db`.
- Import existing YAML: `go run ./cmd/registry-import -src ./boxes -db ./registry.db`.
- Publish via API:

```bash
curl -X PUT "https://registry.example.com/api/v1/admin/boxes/acme/base" \
  -H "Authorization: Bearer $REGISTRY_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  --data-binary @catalog.json
```

- Client: `export VAGRANT_SERVER_URL=https://registry.example.com`.
- Admin UI: `https://registry.example.com/admin`, Basic auth `admin` / `REGISTRY_ADMIN_TOKEN`.
- Security: TLS required; keep the admin token secret; do not expose `/admin` without TLS.
- Backups: link to `deploy/backup.md`.
- Non-goals: no binary hosting, no Vagrant Cloud API v1, single-node SQLite.

- [ ] **Step 6: Verify and commit**

Run:

```bash
gofmt -l .
go vet ./...
go test ./...
go test -race ./...
go build -o bin/registry ./cmd/registry
go build -o bin/registry-import ./cmd/registry-import
rm -f /tmp/registry-smoke.db*
./bin/registry-import -src ./boxes -db /tmp/registry-smoke.db
REGISTRY_ADMIN_TOKEN=test-token ./bin/registry -db /tmp/registry-smoke.db -addr 127.0.0.1:8080 &
pid=$!
sleep 0.5
REGISTRY_ADMIN_TOKEN=test-token ./scripts/smoke.sh
kill "$pid"
```

Expected: `ok: http://127.0.0.1:8080`.

Optionally build the container: `docker build -t vagrant-registry:dev .`.

```bash
git add Dockerfile .dockerignore docker-compose.yml deploy scripts README.md docs/usage.md boxes
git commit -m "ops: add container, backups, and smoke test"
```

---

## Self-review checklist

- [ ] Spec §2 routes, headers, and matching rules are covered.
- [ ] Full-document `PUT` atomicity has a test that leaves old data on failure.
- [ ] Admin disabled (404) and wrong-token (401) behaviors are tested.
- [ ] CSRF is tested for missing, mismatched, and valid tokens.
- [ ] Cascade delete is tested.
- [ ] Concurrent upserts pass under `go test -race`.
- [ ] Import is idempotent and validates catalogs.
- [ ] Backup/restore runbook avoids copying a live WAL database incorrectly.
