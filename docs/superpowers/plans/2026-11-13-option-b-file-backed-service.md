# Option B — File-Backed Service Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run a small read-only Go service that serves validated catalog JSON from YAML files on disk, with atomic reloads, no database, and full `VAGRANT_SERVER_URL` compatibility.

**Architecture:** The service loads `boxes/**/*.yaml` into an immutable snapshot and serves `GET`/`HEAD` on `/api/v2/vagrant/{org}/{box}` and `/{org}/{box}`. A SIGHUP or a periodic reload rebuilds the snapshot; if any file is invalid, the previous snapshot keeps serving. Artifact binaries are never proxied.

**Tech Stack:** Go 1.22+, `gopkg.in/yaml.v3`, standard library `net/http`, `log/slog`; Docker or systemd; nginx for TLS termination (optional).

**Spec:** `../specs/2026-11-13-vagrant-registry-design.md`

## Global Constraints

- Go 1.22 or newer; module path `github.com/rmoriz/vagrant-registry`.
- Only dependency: `gopkg.in/yaml.v3`. No database, no web framework.
- Startup fails if the catalog directory is invalid. A later reload failure keeps the last known-good snapshot and reports the error on `/healthz`.
- Public routes only; no admin API, no auth, no binary hosting.
- All responses set `Content-Type: application/json` for catalogs and `Cache-Control` from configuration.
- `HEAD` returns the same headers as `GET`, including `Content-Length`, and no body.
- Names are validated before use in filesystem paths; `..` and encoded traversal must 404.
- The service is single-node; file publication is “write files, then reload”.
- If the directory is not already a Git repository, run `git init` in Task 1.

## Review Focus

- An invalid reload must not replace the serving snapshot.
- `HEAD` must return no body with the correct `Content-Length`.
- Both the preferred and legacy routes must work with identical bytes.
- Path traversal in `{org}` or `{box}` must 404, never read outside the catalog root.
- Concurrent reads during reload must not observe a partially built snapshot (`go test -race`).

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
printf 'bin/\n*.db\n.DS_Store\n' > .gitignore
go get gopkg.in/yaml.v3@v3.0.1
```

- [ ] **Step 2: Write the failing tests**

Create the four test files with the same assertions as Option A, Task 1–3:

- `TestValidGemVersion` (valid/invalid table from Option A).
- `TestLoadDirLoadsAndSortsCatalogs`.
- `TestLoadFilePreservesUnquotedVersionText` (`version: 1.0` → `"1.0"`).
- `TestLoadDirRejectsNamePathMismatch` (a file at `acme/base.yaml` declaring `acme/wrong`).
- `TestValidateAcceptsValidCatalog`, `TestValidateRejects` (all spec §4.1 rows).
- `TestMarshalEmitsDefaultArchitectureFalseForNonDefault`.
- `TestMarshalOmitsDefaultArchitectureForLegacyProvider`.

Do not create the static-tree tests from Option A; B renders per request.

- [ ] **Step 3: Run tests and verify failure**

Run: `go test ./internal/catalog -v`
Expected: FAIL with undefined types/functions.

- [ ] **Step 4: Implement the shared package**

Copy the implementation decisions from spec §4 and Option A, Tasks 1–3:

- Structs exactly as in spec §4.
- `ValidGemVersion` regex `^[0-9]+(?:\.[0-9A-Za-z]+)*$`.
- `Version.UnmarshalYAML` preserving the scalar node’s `Value`.
- `LoadDir` walks `*.yaml`/`*.yml`, enforces relative-path/name equality, sorts by name.
- `Validate` aggregates all errors and implements every spec §4.1 row.
- `Marshal` validates, normalizes `DefaultArchitecture` to `false` for architecture-aware non-defaults, emits `json.MarshalIndent` + newline.
- No `WriteTree` is needed in B.

- [ ] **Step 5: Run tests and commit**

Run: `go test ./internal/catalog -v`
Expected: PASS.

```bash
git add go.mod go.sum .gitignore internal/catalog
git commit -m "feat: add shared catalog types, loader, validation, rendering"
```

---

### Task 2: Atomic file-backed store

**Files:**
- Create: `internal/store/store.go`
- Create: `internal/store/file.go`
- Create: `internal/store/file_test.go`

**Interfaces:**
- Consumes: `catalog.LoadDir`, `catalog.Catalog`.
- Produces:

```go
package store

var ErrNotFound = errors.New("catalog not found")

type Store interface {
	Catalog(ctx context.Context, org, box string) (*catalog.Catalog, error)
}

type FileStore struct { /* unexported */ }

func NewFileStore(dir string) (*FileStore, error)
func (s *FileStore) Reload() error
func (s *FileStore) Catalog(ctx context.Context, org, box string) (*catalog.Catalog, error)
func (s *FileStore) LastReload() (time.Time, error)
```

`Catalog` returns a copy or a value that callers must not mutate. The easiest safe implementation is to store pointers to immutable catalogs and never mutate them after publication.

- [ ] **Step 1: Write the failing store tests**

Create `internal/store/file_test.go`:

```go
package store

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"sync"
	"testing"
)

func TestFileStoreLoadsCatalog(t *testing.T) {
	dir := t.TempDir()
	writeCatalog(t, dir, "acme", "base", "1.0.0")
	s, err := NewFileStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	c, err := s.Catalog(context.Background(), "acme", "base")
	if err != nil {
		t.Fatal(err)
	}
	if c.Name != "acme/base" {
		t.Fatalf("name = %q", c.Name)
	}
	if _, err := s.Catalog(context.Background(), "acme", "missing"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("err = %v, want ErrNotFound", err)
	}
}

func TestFileStoreInvalidReloadKeepsPreviousSnapshot(t *testing.T) {
	dir := t.TempDir()
	writeCatalog(t, dir, "acme", "base", "1.0.0")
	s, err := NewFileStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	writeFile(t, filepath.Join(dir, "acme", "bad.yaml"), "name: acme/bad\n")
	if err := s.Reload(); err == nil {
		t.Fatal("expected reload error")
	}
	if _, err := s.Catalog(context.Background(), "acme", "base"); err != nil {
		t.Fatalf("previous snapshot lost: %v", err)
	}
}

func TestFileStoreReloadPublishesNewCatalog(t *testing.T) {
	dir := t.TempDir()
	writeCatalog(t, dir, "acme", "base", "1.0.0")
	s, err := NewFileStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	writeCatalog(t, dir, "acme", "other", "1.0.0")
	if err := s.Reload(); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Catalog(context.Background(), "acme", "other"); err != nil {
		t.Fatal(err)
	}
}

func TestFileStoreConcurrentReadsDuringReload(t *testing.T) {
	dir := t.TempDir()
	writeCatalog(t, dir, "acme", "base", "1.0.0")
	s, err := NewFileStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	var wg sync.WaitGroup
	for i := 0; i < 20; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < 100; j++ {
				if _, err := s.Catalog(context.Background(), "acme", "base"); err != nil {
					t.Errorf("read failed: %v", err)
					return
				}
			}
		}()
	}
	for i := 0; i < 20; i++ {
		if err := s.Reload(); err != nil {
			t.Fatal(err)
		}
	}
	wg.Wait()
}
```

Helpers:

```go
func writeCatalog(t *testing.T, root, org, box, version string) {
	t.Helper()
	writeFile(t, filepath.Join(root, org, box+".yaml"), "name: "+org+"/"+box+"\nversions:\n  - version: \""+version+"\"\n    providers:\n      - name: virtualbox\n        url: https://example.com/"+box+".box\n")
}

func writeFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `go test ./internal/store -v`
Expected: FAIL with `undefined: NewFileStore`.

- [ ] **Step 3: Implement the store**

Create `internal/store/store.go` with the interface and `ErrNotFound`.

Create `internal/store/file.go`:

- `FileStore` holds `dir string`, `mu sync.RWMutex`, `snapshot map[string]*catalog.Catalog`, `loadedAt time.Time`, `lastErr error`.
- Key is `org + "/" + box`.
- `NewFileStore` calls `Reload`; if it fails, return `nil, err` (fail startup).
- `Reload` calls `catalog.LoadDir(dir)`; on error, store `lastErr` under lock and return the error **without** replacing `snapshot` or `loadedAt`.
- On success, build a new map, set `lastErr = nil`, and swap it under the write lock.
- `Catalog` takes a read lock, looks up the key, returns `ErrNotFound` if absent, and returns the catalog pointer. Catalogs are immutable after publication.
- `LastReload` returns the last successful load time and `lastErr`.

- [ ] **Step 4: Run tests, including race detector, and commit**

Run:

```bash
go test ./internal/store -v
go test -race ./internal/store
```

Expected: PASS with no race reports.

```bash
git add internal/store
git commit -m "feat: add atomic file-backed catalog store"
```

---

### Task 3: Public HTTP handler

**Files:**
- Create: `internal/httpapi/handler.go`
- Create: `internal/httpapi/handler_test.go`

**Interfaces:**
- Consumes: `store.Store`, `store.ErrNotFound`, `catalog.Marshal`.
- Produces:

```go
type Options struct {
	CacheControl string           // e.g. "public, max-age=60"; empty means no header
	Health       func() error     // optional; nil means always healthy
}

func NewHandler(s store.Store, opts Options) http.Handler
```

- [ ] **Step 1: Write the failing handler tests**

Create `internal/httpapi/handler_test.go` with a fake store:

```go
package httpapi

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/rmoriz/vagrant-registry/internal/catalog"
	"github.com/rmoriz/vagrant-registry/internal/store"
)

type fakeStore struct {
	c *catalog.Catalog
}

func (f fakeStore) Catalog(ctx context.Context, org, box string) (*catalog.Catalog, error) {
	if f.c != nil && org+"/"+box == f.c.Name {
		return f.c, nil
	}
	return nil, store.ErrNotFound
}

func TestHandlerServesBothRoutes(t *testing.T) {
	c := &catalog.Catalog{Name: "acme/base", Versions: []catalog.Version{{
		Version: "1.0.0",
		Providers: []catalog.Provider{{Name: "virtualbox", URL: "https://e/x.box"}},
	}}}
	h := NewHandler(fakeStore{c: c}, Options{CacheControl: "public, max-age=60"})
	for _, path := range []string{"/api/v2/vagrant/acme/base", "/acme/base"} {
		req := httptest.NewRequest(http.MethodGet, path, nil)
		rec := httptest.NewRecorder()
		h.ServeHTTP(rec, req)
		if rec.Code != http.StatusOK {
			t.Fatalf("%s: code = %d", path, rec.Code)
		}
		if ct := rec.Header().Get("Content-Type"); ct != "application/json" {
			t.Fatalf("%s: content-type = %q", path, ct)
		}
		if !strings.Contains(rec.Body.String(), `"name": "acme/base"`) {
			t.Fatalf("%s: unexpected body %s", path, rec.Body.String())
		}
	}
}

func TestHandlerHeadHasHeadersAndNoBody(t *testing.T) {
	c := &catalog.Catalog{Name: "acme/base", Versions: []catalog.Version{{
		Version: "1.0.0", Providers: []catalog.Provider{{Name: "virtualbox", URL: "https://e/x.box"}},
	}}}
	h := NewHandler(fakeStore{c: c}, Options{})
	req := httptest.NewRequest(http.MethodHead, "/acme/base", nil)
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusOK {
		t.Fatalf("code = %d", rec.Code)
	}
	if rec.Body.Len() != 0 {
		t.Fatalf("HEAD body length = %d", rec.Body.Len())
	}
	if rec.Header().Get("Content-Length") == "" {
		t.Fatal("missing Content-Length")
	}
}

func TestHandlerNotFound(t *testing.T) {
	h := NewHandler(fakeStore{}, Options{})
	req := httptest.NewRequest(http.MethodGet, "/acme/missing", nil)
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusNotFound {
		t.Fatalf("code = %d, want 404", rec.Code)
	}
}

func TestHandlerRejectsTraversal(t *testing.T) {
	h := NewHandler(fakeStore{}, Options{})
	for _, path := range []string{"/../etc/passwd", "/acme/%2e%2e", "/api/v2/vagrant/acme/%2e%2e"} {
		req := httptest.NewRequest(http.MethodGet, path, nil)
		rec := httptest.NewRecorder()
		h.ServeHTTP(rec, req)
		if rec.Code != http.StatusNotFound {
			t.Fatalf("%s: code = %d, want 404", path, rec.Code)
		}
	}
}

func TestHandlerMethodNotAllowed(t *testing.T) {
	h := NewHandler(fakeStore{}, Options{})
	req := httptest.NewRequest(http.MethodPost, "/acme/base", nil)
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusMethodNotAllowed {
		t.Fatalf("code = %d, want 405", rec.Code)
	}
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `go test ./internal/httpapi -v`
Expected: FAIL with `undefined: NewHandler`.

- [ ] **Step 3: Implement the handler**

Create `internal/httpapi/handler.go`:

- Wrap the mux in a top-level handler that rejects traversal before the mux
  can clean or redirect it. Inspect `r.URL.EscapedPath()`; if it contains
  `..`, `%2e%2e`, `%2f`, or `%5c` (case-insensitive), return 404.
- Use `http.NewServeMux` with Go 1.22 patterns:

```go
mux.HandleFunc("GET /api/v2/vagrant/{org}/{box}", h.catalog)
mux.HandleFunc("HEAD /api/v2/vagrant/{org}/{box}", h.catalog)
mux.HandleFunc("GET /{org}/{box}", h.catalog)
mux.HandleFunc("HEAD /{org}/{box}", h.catalog)
mux.HandleFunc("GET /healthz", h.health)
mux.HandleFunc("HEAD /healthz", h.health)
```

- `catalog` handler:
  - `org := r.PathValue("org")`, `box := r.PathValue("box")`.
  - Reject if either is `.`, `..`, contains `/`, `\`, or fails the name regex from the spec.
  - `c, err := s.Catalog(r.Context(), org, box)`.
  - `ErrNotFound` → 404 with `text/plain` “not found”; other errors → 500.
  - `body, err := catalog.Marshal(*c)`; 500 on error.
  - Set `Content-Type: application/json`, `Content-Length`, `Cache-Control` if non-empty.
  - `HEAD`: `WriteHeader(200)` and return; `GET`: write body.
- `health`: if `opts.Health` is non-nil, call it; 200 `ok` on nil, 503 with the error text on failure. If `opts.Health` is nil, return 200 `ok`. B’s `main` passes a closure that calls `store.LastReload()`.
- Set `X-Content-Type-Options: nosniff` on all responses.

- [ ] **Step 4: Run tests and commit**

Run: `go test ./internal/httpapi -v`
Expected: PASS.

```bash
git add internal/httpapi
git commit -m "feat: serve catalog JSON on both Vagrant routes"
```

---

### Task 4: Configuration and service lifecycle

**Files:**
- Create: `internal/config/config.go`
- Create: `internal/config/config_test.go`
- Create: `cmd/registry/main.go`
- Create: `cmd/registry/main_test.go`

**Interfaces:**
- Consumes: `store.NewFileStore`, `httpapi.NewHandler`.
- Produces:

```go
type Config struct {
	Addr           string
	Catalogs       string
	CacheControl   string
	ReloadInterval time.Duration
}

func Parse(args []string) (Config, error)
```

- [ ] **Step 1: Write the failing config test**

Create `internal/config/config_test.go`:

```go
package config

import (
	"testing"
	"time"
)

func TestParseDefaults(t *testing.T) {
	c, err := Parse(nil)
	if err != nil {
		t.Fatal(err)
	}
	if c.Addr != "127.0.0.1:8080" || c.Catalogs != "./boxes" {
		t.Fatalf("unexpected defaults: %+v", c)
	}
	if c.CacheControl != "public, max-age=60" {
		t.Fatalf("cache control = %q", c.CacheControl)
	}
}

func TestParseOverrides(t *testing.T) {
	c, err := Parse([]string{
		"-addr", ":9000",
		"-catalogs", "/srv/catalogs",
		"-cache-control", "no-store",
		"-reload-interval", "2s",
	})
	if err != nil {
		t.Fatal(err)
	}
	if c.Addr != ":9000" || c.Catalogs != "/srv/catalogs" || c.CacheControl != "no-store" || c.ReloadInterval != 2*time.Second {
		t.Fatalf("unexpected config: %+v", c)
	}
}

func TestParseRejectsBadInterval(t *testing.T) {
	if _, err := Parse([]string{"-reload-interval", "soon"}); err == nil {
		t.Fatal("expected error")
	}
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `go test ./internal/config -v`
Expected: FAIL with `undefined: Parse`.

- [ ] **Step 3: Implement config**

Create `internal/config/config.go` using `flag.NewFlagSet("registry", flag.ContinueOnError)`:

- `-addr` default `127.0.0.1:8080`
- `-catalogs` default `./boxes`
- `-cache-control` default `public, max-age=60`
- `-reload-interval` default `5s` (0 disables the ticker)
- Return parse errors.

- [ ] **Step 4: Write the failing main test**

Create `cmd/registry/main_test.go`:

```go
package main

import (
	"context"
	"testing"
	"time"
)

func TestRunReturnsErrorForMissingCatalogs(t *testing.T) {
	if code := run(context.Background(), []string{"-catalogs", "/does/not/exist"}); code == 0 {
		t.Fatal("expected non-zero exit")
	}
}

func TestRunServesAndShutsDown(t *testing.T) {
	dir := t.TempDir()
	writeCatalog(t, dir)
	ctx, cancel := context.WithTimeout(context.Background(), 200*time.Millisecond)
	defer cancel()
	if code := run(ctx, []string{"-catalogs", dir, "-addr", "127.0.0.1:0"}); code != 0 {
		t.Fatalf("code = %d", code)
	}
}
```

`writeCatalog` is a local helper that writes `acme/base.yaml`.

- [ ] **Step 5: Implement `main`**

Create `cmd/registry/main.go`:

- `func run(ctx context.Context, args []string) int`.
- Parse config, create `FileStore` (fail on error), create handler with `httpapi.Options{CacheControl: cfg.CacheControl, Health: func() error { _, err := fs.LastReload(); return err }}`, start `http.Server` with timeouts (`ReadHeaderTimeout: 5s`, `IdleTimeout: 60s`).
- Listen on the configured address; log the actual address, including the ephemeral port, with `slog`.
- Start a reload ticker if `ReloadInterval > 0`; each tick calls `store.Reload()` and logs success/failure. Do not exit on reload failure.
- `SIGHUP` triggers `Reload()` immediately.
- `SIGINT`/`SIGTERM` or `ctx.Done()` triggers `Shutdown` with a 10-second timeout; return 0 on clean shutdown.
- `/healthz` returns 200 `ok` when the last reload succeeded, and 503 with the last reload error text when it failed (but the service still serves the previous snapshot). This comes from the `Health` option above.
- Log method, path, status, and duration for each request with a small middleware.

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
git commit -m "feat: add registry service lifecycle and health"
```

---

### Task 5: Container, systemd, reverse proxy, and smoke test

**Files:**
- Create: `Dockerfile`
- Create: `.dockerignore`
- Create: `deploy/systemd/vagrant-registry.service`
- Create: `deploy/nginx.conf`
- Create: `scripts/smoke.sh`
- Create: `docs/usage.md` (Option B usage; keep the plan index in `README.md` and add a link)
- Create: `boxes/acme/base.yaml` (copy the example from spec §3)

**Interfaces:**
- Consumes: the `registry` binary and a catalog directory.
- Produces: a deployable container and a repeatable smoke test.

- [ ] **Step 1: Write the Dockerfile**

```dockerfile
FROM golang:1.22 AS build
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/registry ./cmd/registry

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/registry /registry
USER nonroot:nonroot
ENTRYPOINT ["/registry"]
CMD ["-catalogs", "/catalogs", "-addr", ":8080"]
```

`.dockerignore`: `.git`, `bin`, `public`, `*.db`, `docs`.

- [ ] **Step 2: Write the systemd unit**

`deploy/systemd/vagrant-registry.service`:

```ini
[Unit]
Description=Vagrant catalog registry
After=network-online.target
Wants=network-online.target

[Service]
User=vagrant-registry
Group=vagrant-registry
ExecStart=/usr/local/bin/registry -catalogs /etc/vagrant-registry/boxes -addr 127.0.0.1:8080
Restart=on-failure
RestartSec=2s
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadOnlyPaths=/etc/vagrant-registry/boxes
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 3: Write the nginx TLS config**

`deploy/nginx.conf`:

```nginx
server {
    listen 443 ssl;
    http2 on;
    server_name registry.example.com;

    ssl_certificate     /etc/letsencrypt/live/registry.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/registry.example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_read_timeout 30s;
    }
}
```

Note in the README: nginx forwards `HEAD` unchanged; do not add a rewrite that turns `HEAD` into `GET`.

- [ ] **Step 4: Write the smoke script**

`scripts/smoke.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

base="${1:-http://127.0.0.1:8080}"
path="/api/v2/vagrant/acme/base"

status=$(curl -sS -o /dev/null -w '%{http_code}' -I "$base$path")
if [ "$status" != "200" ]; then
  echo "HEAD $path returned $status" >&2
  exit 1
fi

ctype=$(curl -sSI "$base$path" | tr -d '\r' | awk -F': ' 'tolower($1)=="content-type" {print $2}')
case "$ctype" in
  application/json*) ;;
  *) echo "content-type is '$ctype', want application/json" >&2; exit 1 ;;
esac

body=$(curl -sS "$base$path")
printf '%s' "$body" | grep -Eq '"name"[[:space:]]*:[[:space:]]*"acme/base"'
legacy=$(curl -sS "$base/acme/base")
[ "$body" = "$legacy" ]
echo "ok: $base"
```

Make it executable: `chmod +x scripts/smoke.sh`.

- [ ] **Step 5: Write `docs/usage.md` and link it from `README.md`

Document:

- Build: `go build -o bin/registry ./cmd/registry`.
- Run: `./bin/registry -catalogs ./boxes -addr 127.0.0.1:8080`.
- Reload: `kill -HUP <pid>` after copying new YAML files; or rely on `-reload-interval`.
- Publish new catalogs atomically: write to a temp directory, start the service against it on a scratch port to prove it loads, then `rsync` into place and SIGHUP. If a reload fails, the old snapshot keeps serving and `/healthz` reports the error.
- Health: `curl http://127.0.0.1:8080/healthz`.
- Client: `export VAGRANT_SERVER_URL=https://registry.example.com`; mention `config.vm.box_server_url` is Vagrantfile-only.
- Docker: build and run with `-v "$PWD/boxes:/catalogs:ro"`.
- Non-goals: no binary hosting, no admin UI, no auth.

- [ ] **Step 6: Verify and commit**

Run:

```bash
gofmt -l .
go vet ./...
go test ./...
go build -o bin/registry ./cmd/registry
./bin/registry -catalogs ./boxes -addr 127.0.0.1:8080 &
pid=$!
sleep 0.5
./scripts/smoke.sh
kill "$pid"
```

Expected: `ok: http://127.0.0.1:8080`.

Optionally build the container: `docker build -t vagrant-registry:dev .`.

```bash
git add Dockerfile .dockerignore deploy scripts README.md docs/usage.md boxes
git commit -m "ops: add container, systemd, nginx, and smoke test"
```

---

## Self-review checklist

- [ ] Spec §2 routes and headers are implemented and tested.
- [ ] Both `GET` and `HEAD` are tested on both routes.
- [ ] Invalid reload keeps the previous snapshot; race test passes.
- [ ] Traversal attempts return 404.
- [ ] `go test -race ./...` passes.
- [ ] Smoke test asserts `Content-Type` and identical bytes on both routes.
- [ ] README states that the service never proxies binaries.
