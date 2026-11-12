# Option A — Static Catalog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generate a validated static catalog tree from Git-tracked YAML and publish it to S3/CloudFront so Vagrant can use it as `VAGRANT_SERVER_URL`; also emit a GitHub Pages profile for direct `box_url` use.

**Architecture:** A Go CLI reads `boxes/**/*.yaml`, validates every catalog against the shared rules, and writes deterministic JSON to `public/api/v2/vagrant/{org}/{box}` and `public/{org}/{box}`. CI runs tests and validation, syncs the tree to S3 with explicit `Content-Type: application/json`, and invalidates CloudFront. A second profile writes `catalogs/{org}/{box}.json` for GitHub Pages, which cannot serve the extensionless `VAGRANT_SERVER_URL` routes.

**Tech Stack:** Go 1.22+, `gopkg.in/yaml.v3`, `encoding/json`, standard library; GitHub Actions; AWS CLI; S3 + CloudFront; GitHub Pages.

**Spec:** `../specs/2026-11-13-vagrant-registry-design.md`

## Global Constraints

- Go 1.22 or newer; module path `github.com/rmoriz/vagrant-registry`.
- Only dependency: `gopkg.in/yaml.v3`. No web framework, no AWS SDK.
- Every catalog must pass `catalog.Validate` before any output is written.
- Generated JSON is deterministic: two-space indent, one trailing newline, struct field order from the spec.
- Versions in YAML are always quoted in examples and docs.
- `default_architecture` is emitted as `false` for every architecture-aware provider that is not the default; omitted only for legacy arch-less providers.
- Output directory is replaced atomically; stale files must not survive.
- Production has no runtime service. The optional `registry-dev` server is for local verification only.
- Do not commit `public/`, `bin/`, or generated artifacts.
- If the directory is not already a Git repository, run `git init` in Task 1.

## Review Focus

- A catalog whose `name` does not match its file path or the requested alias must fail generation.
- An architecture-aware provider without `default_architecture: false` can be selected on the wrong architecture by Vagrant’s legacy fallback.
- An unquoted YAML version such as `1.0` must not silently become a float or lose its text.
- Removing a box/version from YAML must remove it from the generated tree.
- An S3 object uploaded without `Content-Type: application/json` must fail the deployment check.

---

### Task 1: Module, catalog types, and Gem version validation

**Files:**
- Create: `go.mod`
- Create: `.gitignore`
- Create: `internal/catalog/catalog.go`
- Create: `internal/catalog/version.go`
- Create: `internal/catalog/version_test.go`

**Interfaces:**
- Consumes: nothing.
- Produces: `catalog.Catalog`, `catalog.Version`, `catalog.Provider` exactly as defined in the spec; `catalog.ValidGemVersion(s string) bool`.

- [ ] **Step 1: Initialize the module and ignore generated files**

```bash
cd /path/to/vagrant-registry
git init  # only if not already a repository
go mod init github.com/rmoriz/vagrant-registry
printf 'public/\nbin/\n.DS_Store\n' > .gitignore
go get gopkg.in/yaml.v3@v3.0.1
```

- [ ] **Step 2: Write the failing version test**

Create `internal/catalog/version_test.go`:

```go
package catalog

import "testing"

func TestValidGemVersion(t *testing.T) {
	tests := []struct {
		in   string
		want bool
	}{
		{"1.2.0", true},
		{"1.0", true},
		{"0.1.0.pre.1", true},
		{"2.0.0.rc1", true},
		{"v1.2.0", false},
		{"1.2.0+build", false},
		{"1..2", false},
		{"", false},
		{"1.2.0-rc1", false},
	}
	for _, tt := range tests {
		if got := ValidGemVersion(tt.in); got != tt.want {
			t.Errorf("ValidGemVersion(%q) = %v, want %v", tt.in, got, tt.want)
		}
	}
}
```

- [ ] **Step 3: Run it and verify failure**

Run: `go test ./internal/catalog -run TestValidGemVersion -v`
Expected: FAIL with `undefined: ValidGemVersion`.

- [ ] **Step 4: Implement the types and validator**

Create `internal/catalog/catalog.go` with the three structs from spec §4, including the `DefaultArchitecture *bool` field.

Create `internal/catalog/version.go`:

```go
package catalog

import "regexp"

var gemVersionRE = regexp.MustCompile(`^[0-9]+(?:\.[0-9A-Za-z]+)*$`)

func ValidGemVersion(s string) bool {
	return gemVersionRE.MatchString(s)
}
```

- [ ] **Step 5: Run tests and commit**

Run: `go test ./internal/catalog -run TestValidGemVersion -v`
Expected: PASS.

```bash
git add go.mod go.sum .gitignore internal/catalog
git commit -m "feat: add catalog types and Gem version validation"
```

---

### Task 2: YAML loader and full validation

**Files:**
- Create: `internal/catalog/load.go`
- Create: `internal/catalog/validate.go`
- Create: `internal/catalog/load_test.go`
- Create: `internal/catalog/validate_test.go`
- Create: `internal/catalog/testdata/valid/acme/base.yaml`
- Create: `internal/catalog/testdata/invalid/...` (fixtures named per test)

**Interfaces:**
- Consumes: `Catalog`, `Version`, `Provider`, `ValidGemVersion`.
- Produces: `LoadDir(root string) ([]Catalog, error)`, `LoadFile(path string) (Catalog, error)`, `Validate(c Catalog) error`.

- [ ] **Step 1: Write the failing loader tests**

Create `internal/catalog/load_test.go` with:

```go
package catalog

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoadDirLoadsAndSortsCatalogs(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, "acme", "base.yaml"), `
name: acme/base
versions:
  - version: "1.0.0"
    providers:
      - name: virtualbox
        url: https://example.com/base.box
`)
	writeFile(t, filepath.Join(dir, "acme", "other.yml"), `
name: acme/other
versions:
  - version: "2.0.0"
    providers:
      - name: libvirt
        url: https://example.com/other.box
`)
	catalogs, err := LoadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(catalogs) != 2 || catalogs[0].Name != "acme/base" || catalogs[1].Name != "acme/other" {
		t.Fatalf("unexpected catalogs: %+v", catalogs)
	}
}

func TestLoadFilePreservesUnquotedVersionText(t *testing.T) {
	path := filepath.Join(t.TempDir(), "base.yaml")
	writeFile(t, path, `
name: acme/base
versions:
  - version: 1.0
    providers:
      - name: virtualbox
        url: https://example.com/base.box
`)
	c, err := LoadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if c.Versions[0].Version != "1.0" {
		t.Fatalf("version = %q, want %q", c.Versions[0].Version, "1.0")
	}
}

func TestLoadDirRejectsNamePathMismatch(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, "acme", "base.yaml"), `
name: acme/wrong
versions:
  - version: "1.0.0"
    providers:
      - name: virtualbox
        url: https://example.com/base.box
`)
	if _, err := LoadDir(dir); err == nil {
		t.Fatal("expected error for name/path mismatch")
	}
}
```

Add a small `writeFile` helper in the test file:

```go
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

- [ ] **Step 2: Write the failing validation tests**

Create `internal/catalog/validate_test.go`:

```go
package catalog

import (
	"strings"
	"testing"
)

func validCatalog() Catalog {
	def := true
	return Catalog{
		Name: "acme/base",
		Versions: []Version{{
			Version: "1.0.0",
			Providers: []Provider{{
				Name: "virtualbox", URL: "https://example.com/base.box",
				Architecture: "amd64", DefaultArchitecture: &def,
			}},
		}},
	}
}

func TestValidateAcceptsValidCatalog(t *testing.T) {
	if err := Validate(validCatalog()); err != nil {
		t.Fatal(err)
	}
}

func TestValidateRejects(t *testing.T) {
	cases := map[string]func(*Catalog){
		"bad name":            func(c *Catalog) { c.Name = "acme" },
		"double slash":        func(c *Catalog) { c.Name = "acme/base/extra" },
		"dot segment":         func(c *Catalog) { c.Name = "acme/.." },
		"no versions":         func(c *Catalog) { c.Versions = nil },
		"bad version":         func(c *Catalog) { c.Versions[0].Version = "v1.0.0" },
		"duplicate version":   func(c *Catalog) { c.Versions = append(c.Versions, c.Versions[0]) },
		"no providers":        func(c *Catalog) { c.Versions[0].Providers = nil },
		"bad provider name":   func(c *Catalog) { c.Versions[0].Providers[0].Name = "VirtualBox" },
		"bad url":             func(c *Catalog) { c.Versions[0].Providers[0].URL = "ftp://example.com/x" },
		"credentials in url":  func(c *Catalog) { c.Versions[0].Providers[0].URL = "https://u:p@example.com/x" },
		"checksum without type": func(c *Catalog) { c.Versions[0].Providers[0].Checksum = strings.Repeat("a", 64) },
		"bad checksum length": func(c *Catalog) {
			c.Versions[0].Providers[0].ChecksumType = "sha256"
			c.Versions[0].Providers[0].Checksum = "abc"
		},
		"default without arch": func(c *Catalog) { c.Versions[0].Providers[0].Architecture = "" },
		"duplicate provider arch": func(c *Catalog) {
			p := c.Versions[0].Providers[0]
			c.Versions[0].Providers = append(c.Versions[0].Providers, p)
		},
	}
	for name, mutate := range cases {
		t.Run(name, func(t *testing.T) {
			c := validCatalog()
			mutate(&c)
			if err := Validate(c); err == nil {
				t.Fatal("expected validation error")
			}
		})
	}
}

func TestValidateRejectsMultipleDefaultsPerProvider(t *testing.T) {
	c := validCatalog()
	second := c.Versions[0].Providers[0]
	second.Architecture = "arm64"
	second.DefaultArchitecture = c.Versions[0].Providers[0].DefaultArchitecture
	c.Versions[0].Providers = append(c.Versions[0].Providers, second)
	if err := Validate(c); err == nil {
		t.Fatal("expected multiple-default error")
	}
}
```

- [ ] **Step 3: Run tests and verify failure**

Run: `go test ./internal/catalog -run 'TestLoad|TestValidate' -v`
Expected: FAIL with `undefined: LoadDir` / `Validate`.

- [ ] **Step 4: Implement the loader**

Create `internal/catalog/load.go`:

- `LoadDir` walks the root with `filepath.WalkDir`, skips directories and non-`.yaml`/`.yml` files, calls `LoadFile`, sorts by `Name`.
- `LoadFile` reads the file, decodes with `yaml.Unmarshal`, and calls `Validate`. `LoadDir` additionally derives the expected name from the path relative to its `root` argument and compares it to `c.Name`; `LoadFile` validates the document itself only.
- Implement `Version.UnmarshalYAML`:

```go
func (v *Version) UnmarshalYAML(node *yaml.Node) error {
	var raw struct {
		Version   yaml.Node `yaml:"version"`
		Providers []Provider `yaml:"providers"`
	}
	if err := node.Decode(&raw); err != nil {
		return err
	}
	v.Version = raw.Version.Value
	v.Providers = raw.Providers
	return nil
}
```

- Wrap YAML errors with the file path.

- [ ] **Step 5: Implement validation**

Create `internal/catalog/validate.go`:

- `Validate` returns `errors.Join` of all field errors, each prefixed with the field path (`versions[0].providers[1].url`).
- Implement the table in spec §4.1 exactly.
- Checksum lengths: `md5:32, sha1:40, sha256:64, sha384:96, sha512:128`.
- `name` regex: `^[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*$`.
- Provider name regex: `^[a-z][a-z0-9_]*$`.
- Architecture regex: `^[A-Za-z0-9_]+$`.
- URL parse must require scheme `http`/`https`, non-empty host, and `u.User == nil`.
- Reject `default_architecture == true` when `architecture == ""`.
- Count defaults per provider name per version; more than one `true` is an error.
- Reject duplicate `(name, architecture)` keys within a version.
- Enforce at least one version and at least one provider per version.

- [ ] **Step 6: Run tests and commit**

Run: `go test ./internal/catalog -v`
Expected: PASS.

```bash
git add internal/catalog
git commit -m "feat: load and validate YAML catalogs"
```

---

### Task 3: Deterministic JSON and static tree generation

**Files:**
- Create: `internal/catalog/render.go`
- Create: `internal/catalog/render_test.go`

**Interfaces:**
- Consumes: `Catalog`, `Validate`.
- Produces:

```go
type Profile string

const (
	ProfileS3    Profile = "s3"
	ProfilePages Profile = "pages"
)

func Marshal(c Catalog) ([]byte, error)
func WriteTree(catalogs []Catalog, outDir string, p Profile) error
```

- [ ] **Step 1: Write the failing render tests**

Create `internal/catalog/render_test.go` with:

```go
package catalog

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestMarshalEmitsDefaultArchitectureFalseForNonDefault(t *testing.T) {
	def := true
	f := false
	c := Catalog{Name: "acme/base", Versions: []Version{{
		Version: "1.0.0",
		Providers: []Provider{
			{Name: "virtualbox", URL: "https://e/x-amd64.box", Architecture: "amd64", DefaultArchitecture: &def},
			{Name: "virtualbox", URL: "https://e/x-arm64.box", Architecture: "arm64", DefaultArchitecture: &f},
		},
	}}}
	b, err := Marshal(c)
	if err != nil {
		t.Fatal(err)
	}
	var raw map[string]any
	if err := json.Unmarshal(b, &raw); err != nil {
		t.Fatal(err)
	}
	providers := raw["versions"].([]any)[0].(map[string]any)["providers"].([]any)
	second := providers[1].(map[string]any)
	if second["default_architecture"] != false {
		t.Fatalf("default_architecture = %v, want false", second["default_architecture"])
	}
	if b[len(b)-1] != '\n' {
		t.Fatal("expected trailing newline")
	}
}

func TestMarshalOmitsDefaultArchitectureForLegacyProvider(t *testing.T) {
	c := Catalog{Name: "acme/base", Versions: []Version{{
		Version:   "1.0.0",
		Providers: []Provider{{Name: "virtualbox", URL: "https://e/x.box"}},
	}}}
	b, err := Marshal(c)
	if err != nil {
		t.Fatal(err)
	}
	var raw map[string]any
	_ = json.Unmarshal(b, &raw)
	p := raw["versions"].([]any)[0].(map[string]any)["providers"].([]any)[0].(map[string]any)
	if _, ok := p["default_architecture"]; ok {
		t.Fatal("legacy provider must omit default_architecture")
	}
}

func TestWriteTreeS3WritesBothRoutes(t *testing.T) {
	out := filepath.Join(t.TempDir(), "public")
	if err := WriteTree([]Catalog{validCatalog()}, out, ProfileS3); err != nil {
		t.Fatal(err)
	}
	api, err := os.ReadFile(filepath.Join(out, "api", "v2", "vagrant", "acme", "base"))
	if err != nil {
		t.Fatal(err)
	}
	legacy, err := os.ReadFile(filepath.Join(out, "acme", "base"))
	if err != nil {
		t.Fatal(err)
	}
	if string(api) != string(legacy) {
		t.Fatal("API and legacy routes differ")
	}
}

func TestWriteTreeRemovesStaleFiles(t *testing.T) {
	out := filepath.Join(t.TempDir(), "public")
	if err := WriteTree([]Catalog{validCatalog()}, out, ProfileS3); err != nil {
		t.Fatal(err)
	}
	removed := Catalog{Name: "acme/removed", Versions: validCatalog().Versions}
	if err := WriteTree([]Catalog{removed}, out, ProfileS3); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(out, "acme", "base")); !os.IsNotExist(err) {
		t.Fatal("stale catalog still present")
	}
}

func TestWriteTreePagesUsesJSONExtension(t *testing.T) {
	out := filepath.Join(t.TempDir(), "public")
	if err := WriteTree([]Catalog{validCatalog()}, out, ProfilePages); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(out, "catalogs", "acme", "base.json")); err != nil {
		t.Fatal(err)
	}
}

func TestWriteTreeValidationErrorLeavesOutputUntouched(t *testing.T) {
	out := filepath.Join(t.TempDir(), "public")
	if err := WriteTree([]Catalog{validCatalog()}, out, ProfileS3); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(filepath.Join(out, "acme", "base"))
	if err != nil {
		t.Fatal(err)
	}
	bad := validCatalog()
	bad.Versions[0].Version = "v1.0.0"
	if err := WriteTree([]Catalog{bad}, out, ProfileS3); err == nil {
		t.Fatal("expected validation error")
	}
	after, err := os.ReadFile(filepath.Join(out, "acme", "base"))
	if err != nil {
		t.Fatal(err)
	}
	if string(before) != string(after) {
		t.Fatal("output changed after a failed generation")
	}
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `go test ./internal/catalog -run 'TestMarshal|TestWriteTree' -v`
Expected: FAIL with `undefined: Marshal` / `WriteTree`.

- [ ] **Step 3: Implement `Marshal`**

Create `internal/catalog/render.go`:

- Call `Validate`; return the error wrapped if invalid.
- Normalize a copy: for every provider with `Architecture != ""` and `DefaultArchitecture == nil`, set a pointer to `false`.
- `json.MarshalIndent(c, "", "  ")`; append `"\n"`.
- Do not escape HTML beyond the standard library default; determinism is what matters.

- [ ] **Step 4: Implement `WriteTree`**

- Create a temp directory in `filepath.Dir(outDir)` with `os.MkdirTemp`.
- For `ProfileS3`, write each catalog to both:
  - `api/v2/vagrant/<org>/<box>`
  - `<org>/<box>`
- For `ProfilePages`, write `catalogs/<org>/<box>.json`.
- Use `filepath.FromSlash` for paths; create parent dirs `0o755`; write files `0o644`.
- After all writes succeed, `os.RemoveAll(outDir)` and `os.Rename(tmp, outDir)`.
- On any error, remove the temp directory and leave `outDir` untouched.
- Reject unknown profiles.
- Test that a validation error leaves any pre-existing `outDir` unchanged.

- [ ] **Step 5: Run tests and commit**

Run: `go test ./internal/catalog -v`
Expected: PASS.

```bash
git add internal/catalog
git commit -m "feat: render deterministic static catalog trees"
```

---

### Task 4: `registry-gen` CLI

**Files:**
- Create: `cmd/registry-gen/main.go`
- Create: `cmd/registry-gen/main_test.go`

**Interfaces:**
- Consumes: `catalog.LoadDir`, `catalog.WriteTree`, `catalog.ProfileS3`, `catalog.ProfilePages`.
- Produces: `func run(args []string, stdout, stderr io.Writer) int`; exit code 0 on success, 1 on validation/generation errors, 2 on flag errors.

- [ ] **Step 1: Write the failing CLI tests**

Create `cmd/registry-gen/main_test.go`:

```go
package main

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"
)

func TestRunCheckSucceedsOnValidSource(t *testing.T) {
	dir := t.TempDir()
	writeCatalog(t, dir)
	var out, errb bytes.Buffer
	if code := run([]string{"-check", "-src", dir}, &out, &errb); code != 0 {
		t.Fatalf("code = %d, stderr = %s", code, errb.String())
	}
}

func TestRunCheckFailsOnInvalidSource(t *testing.T) {
	dir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(dir, "acme"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "acme", "base.yaml"), []byte("name: acme\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	var out, errb bytes.Buffer
	if code := run([]string{"-check", "-src", dir}, &out, &errb); code != 1 {
		t.Fatalf("code = %d, want 1; stderr = %s", code, errb.String())
	}
}

func TestRunGeneratesTree(t *testing.T) {
	src := t.TempDir()
	writeCatalog(t, src)
	outDir := filepath.Join(t.TempDir(), "public")
	var out, errb bytes.Buffer
	if code := run([]string{"-src", src, "-out", outDir}, &out, &errb); code != 0 {
		t.Fatalf("code = %d, stderr = %s", code, errb.String())
	}
	if _, err := os.Stat(filepath.Join(outDir, "acme", "base")); err != nil {
		t.Fatal(err)
	}
}

func writeCatalog(t *testing.T, root string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Join(root, "acme"), 0o755); err != nil {
		t.Fatal(err)
	}
	content := `name: acme/base
versions:
  - version: "1.0.0"
    providers:
      - name: virtualbox
        url: https://example.com/base.box
`
	if err := os.WriteFile(filepath.Join(root, "acme", "base.yaml"), []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `go test ./cmd/registry-gen -v`
Expected: FAIL with `undefined: run`.

- [ ] **Step 3: Implement the CLI**

Create `cmd/registry-gen/main.go`:

- `main` calls `os.Exit(run(os.Args[1:], os.Stdout, os.Stderr))`.
- Flags: `-src` (default `./boxes`), `-out` (default `./public`), `-profile` (default `s3`), `-check`.
- `-check`: load and validate only; print a one-line summary per catalog to stdout; exit 0.
- Otherwise: load, validate, `WriteTree`; print `wrote N catalogs to <out> (<profile>)`.
- Any load/validation error: print every error to stderr, one per line; return 1.
- Flag parse error: return 2.
- Reject `-profile` values other than `s3`/`pages` with a clear error.

- [ ] **Step 4: Run tests and commit**

Run: `go test ./cmd/registry-gen -v`
Expected: PASS.

```bash
git add cmd/registry-gen
git commit -m "feat: add registry-gen CLI"
```

---

### Task 5: Seed catalogs, dev server, and local verification

**Files:**
- Create: `boxes/acme/base.yaml`
- Create: `boxes/acme/other.yaml`
- Create: `cmd/registry-dev/main.go`
- Create: `Makefile`
- Create: `docs/usage.md` (Option A usage; keep the plan index in `README.md` and add a link)

**Interfaces:**
- Consumes: the generator output tree.
- Produces: `registry-dev -dir ./public -addr 127.0.0.1:8080`, a local-only file server that sets `Content-Type: application/json` for every file and answers `HEAD`.

- [ ] **Step 1: Add seed catalogs**

Create `boxes/acme/base.yaml` from spec §3, including an amd64 and arm64 VirtualBox provider and a libvirt provider. Use placeholder `https://downloads.example.com/...` URLs and valid 64-character hex checksums (use repeated `a`/`b`/`c` patterns). Replace these URLs and checksums before publishing a real registry.

Create `boxes/acme/other.yaml` with one legacy provider entry (no architecture keys).

- [ ] **Step 2: Add the dev server**

Create `cmd/registry-dev/main.go`:

- Flags `-dir` (default `./public`) and `-addr` (default `127.0.0.1:8080`).
- Handler: reject methods other than `GET`/`HEAD` with 405; reject any request whose `EscapedPath()` contains `..`, `%2e%2e`, `%2f`, or `%5c` (case-insensitive) **before** cleaning; then `path.Clean` the remainder and map `/` to a small text index; otherwise open `filepath.Join(dir, filepath.FromSlash(strings.TrimPrefix(p, "/")))`.
- Set `Content-Type: application/json`, `Content-Length`, and `Cache-Control: no-store`.
- For `HEAD`, write headers and no body.
- 404 for missing files.
- Print `serving <dir> on http://<addr>` at startup.

This server is documented as development-only; it is not a production registry.

- [ ] **Step 3: Add Makefile targets**

```make
.PHONY: check generate test serve clean
check:
	go run ./cmd/registry-gen -check -src ./boxes
generate:
	go run ./cmd/registry-gen -src ./boxes -out ./public
test:
	go test ./...
serve: generate
	go run ./cmd/registry-dev -dir ./public
clean:
	rm -rf public bin
```

- [ ] **Step 4: Write `docs/usage.md` and link it from `README.md`

Document:

- `make check`, `make generate`, `make serve`.
- `curl -sSI http://127.0.0.1:8080/api/v2/vagrant/acme/base` must show `200` and `Content-Type: application/json`.
- `VAGRANT_SERVER_URL=http://127.0.0.1:8080 vagrant box add acme/base` (the provider URL is a placeholder and will fail the download; this verifies catalog resolution, not the artifact).
- The S3 and GitHub Pages deployment sections link to `deploy/aws.md` and `docs/github-pages.md`.
- State that `public/` is generated and ignored.

- [ ] **Step 5: Verify and commit**

Run:

```bash
gofmt -l .
go vet ./...
go test ./...
make check
make generate
test -f public/api/v2/vagrant/acme/base
test -f public/acme/base
```

Expected: no `gofmt` output, all tests pass, both files exist.

```bash
git add boxes cmd/registry-dev Makefile README.md docs/usage.md
git commit -m "docs: add seed catalogs and local verification server"
```

---

### Task 6: S3 + CloudFront publishing

**Files:**
- Create: `.github/workflows/publish.yml`
- Create: `deploy/aws.md`
- Create: `deploy/bucket-policy.json`

**Interfaces:**
- Consumes: `make generate`, the `public/` tree.
- Produces: a repeatable CI publish to S3 with explicit JSON content type and a CloudFront invalidation.

- [ ] **Step 1: Write the workflow**

Create `.github/workflows/publish.yml`:

```yaml
name: publish-catalog

on:
  push:
    branches: [main]
    paths:
      - "boxes/**"
      - "internal/**"
      - "cmd/**"
      - "go.mod"
      - "go.sum"
      - ".github/workflows/publish.yml"
  workflow_dispatch:

permissions:
  contents: read
  id-token: write

jobs:
  publish:
    runs-on: ubuntu-latest
    env:
      AWS_REGION: ${{ vars.AWS_REGION }}
      S3_BUCKET: ${{ vars.S3_BUCKET }}
      CLOUDFRONT_DISTRIBUTION_ID: ${{ vars.CLOUDFRONT_DISTRIBUTION_ID }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-go@v5
        with:
          go-version: "1.22"
      - run: go test ./...
      - run: go run ./cmd/registry-gen -check -src ./boxes
      - run: go run ./cmd/registry-gen -src ./boxes -out ./public
      - uses: aws-actions/configure-aws-credentials@v4
        with:
          role-to-assume: ${{ vars.AWS_ROLE_ARN }}
          aws-region: ${{ env.AWS_REGION }}
      - name: Sync catalogs
        run: |
          aws s3 sync ./public/ "s3://${S3_BUCKET}/" \
            --delete \
            --content-type application/json \
            --cache-control "public, max-age=60"
      - name: Invalidate CloudFront
        run: |
          aws cloudfront create-invalidation \
            --distribution-id "${CLOUDFRONT_DISTRIBUTION_ID}" \
            --paths "/*"
```

- [ ] **Step 2: Write the deployment runbook**

Create `deploy/aws.md` with exact steps:

1. Create a private bucket with Block Public Access on.
2. Create a CloudFront distribution with an Origin Access Control (OAC) in front of the bucket’s REST endpoint. Allow `GET` and `HEAD`; forward no query strings; use a short cache policy (or `CachingOptimized` plus invalidations).
3. Attach a bucket policy granting `s3:GetObject` to the CloudFront service principal. Include `deploy/bucket-policy.json` as a template with `BUCKET` and `DISTRIBUTION_ARN` placeholders.
4. Point a custom domain at CloudFront with an ACM certificate.
5. Set repository variables `AWS_REGION`, `AWS_ROLE_ARN`, `S3_BUCKET`, `CLOUDFRONT_DISTRIBUTION_ID`.
6. Verify:

```bash
curl -sSI "https://registry.example.com/api/v2/vagrant/acme/base"
curl -sSI "https://registry.example.com/acme/base"
```

Both must return `200` and `Content-Type: application/json`. The runbook must state that a missing content type means the `aws s3 sync` flags were omitted or an object already existed with the wrong metadata.

7. Publish manually for the first time:

```bash
go run ./cmd/registry-gen -src ./boxes -out ./public
aws s3 sync ./public/ "s3://${S3_BUCKET}/" --delete \
  --content-type application/json --cache-control "public, max-age=60"
```

Include a note: to repair metadata on existing objects, re-upload them or use `aws s3 cp --recursive --metadata-directive REPLACE --content-type application/json`.

- [ ] **Step 3: Verify workflow syntax and commit**

Run: `ruby -ryaml -e 'YAML.load_file(".github/workflows/publish.yml")'`
Expected: no output and exit 0.

Optionally run `aws s3 sync --dryrun` locally against a test bucket.

```bash
git add .github/workflows/publish.yml deploy
git commit -m "ci: publish static catalogs to S3 and CloudFront"
```

---

### Task 7: GitHub Pages profile and workflow

**Files:**
- Create: `.github/workflows/pages.yml`
- Create: `docs/github-pages.md`

**Interfaces:**
- Consumes: `registry-gen -profile pages`.
- Produces: `public/catalogs/{org}/{box}.json` served by GitHub Pages.

- [ ] **Step 1: Write the Pages workflow**

Create `.github/workflows/pages.yml`:

```yaml
name: publish-pages

on:
  push:
    branches: [main]
    paths:
      - "boxes/**"
      - "internal/**"
      - "cmd/**"
      - ".github/workflows/pages.yml"
  workflow_dispatch:

permissions:
  contents: read
  pages: write
  id-token: write

concurrency:
  group: pages
  cancel-in-progress: false

jobs:
  deploy:
    runs-on: ubuntu-latest
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-go@v5
        with:
          go-version: "1.22"
      - run: go test ./...
      - run: go run ./cmd/registry-gen -src ./boxes -out ./public -profile pages
      - uses: actions/configure-pages@v5
      - uses: actions/upload-pages-artifact@v3
        with:
          path: ./public
      - id: deployment
        uses: actions/deploy-pages@v4
```

- [ ] **Step 2: Document the limitation and usage**

Create `docs/github-pages.md`:

- Explain that GitHub Pages cannot set `Content-Type: application/json` on extensionless paths, so it cannot be used as `VAGRANT_SERVER_URL` shorthand.
- Show the supported usage:

```ruby
config.vm.box = "acme/base"
config.vm.box_url = "https://<org>.github.io/<repo>/catalogs/acme/base.json"
```

- Show the CLI form:

```bash
vagrant box add https://<org>.github.io/<repo>/catalogs/acme/base.json
```

- State that `vagrant box update` works because the `.json` URL is stored as `metadata_url`.
- Link back to the S3 option for full shorthand support.

- [ ] **Step 3: Verify and commit**

Run:

```bash
go run ./cmd/registry-gen -src ./boxes -out /tmp/registry-pages -profile pages
test -f /tmp/registry-pages/catalogs/acme/base.json
ruby -ryaml -e 'YAML.load_file(".github/workflows/pages.yml")'
```

Expected: file exists, YAML parses.

```bash
git add .github/workflows/pages.yml docs/github-pages.md
git commit -m "ci: publish GitHub Pages catalog profile"
```

---

## Self-review checklist

- [ ] Every spec §2 contract item is covered by a task or the deployment runbook.
- [ ] `Validate` rejects every row in spec §4.1, with a test for each.
- [ ] `default_architecture: false` rendering has a test.
- [ ] Stale-output removal has a test.
- [ ] The S3 runbook explicitly verifies `Content-Type` with `curl -I`.
- [ ] The Pages docs explicitly state the shorthand limitation.
- [ ] No task depends on a file created by a later task.
