# Usage

This registry serves catalog JSON only. Box binaries stay wherever they
already live; the catalog points at them.

## Served paths

A box named `acme/base` is reachable through all three of these, with
identical bytes:

```text
/api/v2/vagrant/acme/base     # preferred Vagrant discovery path
/acme/base                    # legacy fallback
/catalogs/acme/base.json      # direct catalog URL
```

`GET` and `HEAD` are supported on every path. On a successful `HEAD`, the
status and `Content-Type` match the `GET` and the body is empty. Any other
method on a resolved path returns `405` with `Allow: GET, HEAD`. An unknown box
returns `404` with the body `not found`. Both extensionless catalog routes also
resolve percent-encoded names such as `/acme%2Fbase`.

## Add a box

1. Copy an existing document and rename it so the path without `.json` equals
   the `name`:

   ```bash
   mkdir -p data/example
   cp data/acme/base.json data/example/toolbox.json
   ```

   `data/example/toolbox.json` may only declare `"name": "example/toolbox"`.
   A mismatch fails the build, as does a duplicate name.

2. Edit the document, including `name`: a copied document still carries the old
   name until you change it. Replace the description, versions, providers,
   URLs, and checksums. Point every `url` at the real artifact.

3. Validate the whole tree:

   ```bash
   cargo test -p vagrant-registry-core
   ```

   The build reads every document under `data/`, reports failures with file
   and field, and re-renders the catalogs. Nothing is served that did not
   pass.

4. Commit, push, and open a pull request. A merge to `main` deploys
   production; a merge to `staging` deploys staging.

## Document shape

Documents are strict JSON, shaped like this, and every field is spelled out
below:

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
          "checksum": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
          "architecture": "amd64",
          "default_architecture": true
        },
        {
          "name": "virtualbox",
          "url": "https://downloads.example.com/acme-base-1.2.0-vbox-arm64.box",
          "checksum_type": "sha256",
          "checksum": "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
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

| Field | Required | Rule |
|---|---|---|
| `name` | yes | `org/box`, each segment starting alphanumeric, then `[A-Za-z0-9._-]`; no `.` or `..` segment. Must equal the path without `.json` |
| `description` | no | at most 4096 bytes |
| `versions` | yes | at least one; version strings unique |
| `version` | yes | a `Gem::Version`: `^[0-9]+(\.[0-9A-Za-z]+)*$` |
| `providers` | yes | one to 100 per version |
| provider `name` | yes | `^[a-z][a-z0-9_]*$`; matching the provider's box format (for example `virtualbox` or `libvirt`) is a review convention, not machine-checked |
| provider identity | yes | `(name, architecture)` unique within a version; an empty architecture is its own key |
| provider `url` | yes | absolute `http`/`https` URL with a host and no `user:pass@`; may be presigned |
| `checksum` / `checksum_type` | no | both or neither; `md5`, `sha1`, `sha256`, `sha384`, `sha512` with a hex digest of the matching length |
| `architecture` | no | `^[A-Za-z0-9_]+$`; `unknown` is allowed |
| `default_architecture` | no | `true` only with `architecture`, at most once per provider name and version |

Unknown fields are an error, so a typo cannot vanish at render time.

Architecture rules matter:

- **Legacy single-arch box:** omit `architecture` and
  `default_architecture` entirely. Such an entry matches when it is the only
  provider of its name.
- **Architecture-aware, non-default:** write
  `"default_architecture": false` explicitly. The renderer fills in `false`
  when `architecture` is set, but the explicit flag makes the source say what
  it means and keeps the non-legacy handling obvious in review.
- **Architecture-aware default:** `"default_architecture": true`.

Each rendered document may use at most 1 MiB; the whole tree may use at most
16 MiB. Both are build errors, not deploy surprises.

## Client setup

Global, works for the CLI and Vagrantfiles:

```bash
export VAGRANT_SERVER_URL="https://registry.example.com"
```

Vagrant then probes `https://registry.example.com/api/v2/vagrant/acme/base`
and `https://registry.example.com/acme/base`; if both answer, the API path
wins. Keep the value at the host root: the route forms are appended to it.

Per project:

```ruby
config.vm.box = "acme/base"
config.vm.box_server_url = "https://registry.example.com"
```

`config.vm.box_server_url` is only read by the `up`/`box` action path; the CLI
`vagrant box add acme/base` ignores it. Use `VAGRANT_SERVER_URL` for CLI
shorthand adds.

A direct catalog URL bypasses shorthand resolution entirely:

```ruby
config.vm.box_url = "https://registry.example.com/catalogs/acme/base.json"
```

```bash
vagrant box add https://registry.example.com/catalogs/acme/base.json
```

## Catalog URL persistence

Vagrant records the catalog URL in
`~/.vagrant.d/boxes/<org>/<box>/metadata_url` when a box is added, and
`vagrant box update` re-fetches exactly that URL. Keep hosts and paths stable:
moving to a new domain breaks updates for boxes added under the old one, and
those users have to re-add the box. Do not put expiring query strings on
catalog routes.

## Local preview

Neither preview path needs secrets or a deployment:

```bash
npx wrangler dev                                # Worker adapter
cargo run -p vagrant-registry-server            # server adapter, port 8080
```

`PORT` overrides the server port. `scripts/smoke.sh <base-url>` runs the shared
contract checks against either one, and `scripts/container_smoke.sh` builds and
smokes the container image.

## Caching

A catalog `GET` or `HEAD` is served with
`Cache-Control: public, max-age=60`. Index, health, and error responses are
`no-store`. Because of the 60 seconds, a merged change or a host migration can
take up to a minute to be visible through caches. Vagrant also caches
outdated checks client-side for an hour, so `vagrant box outdated` is not
instant either.
