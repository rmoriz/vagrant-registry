# Deployment

Two independent targets serve the same handler: Cloudflare Workers (deployed
by GitHub Actions) and a Docker image (run yourself). Pick one.

## Cloudflare Workers

### Prerequisites

- A Cloudflare account with Workers enabled.
- This repository's `main` and `staging` branches.
- Two GitHub environments, `production` and `staging`, each holding its own
  copy of two secrets. Environments keep the two token sets apart so a
  staging deploy cannot touch production.
- An API token with the **Workers Scripts:Edit** permission and the account
  ID it belongs to.

Create the environments and set the secrets per environment (each command
prompts for the value):

```bash
gh api "repos/{owner}/{repo}/environments/production" -X PUT
gh api "repos/{owner}/{repo}/environments/staging" -X PUT

gh secret set CLOUDFLARE_API_TOKEN --env production
gh secret set CLOUDFLARE_ACCOUNT_ID --env production
gh secret set CLOUDFLARE_API_TOKEN --env staging
gh secret set CLOUDFLARE_ACCOUNT_ID --env staging
```

`CLOUDFLARE_ACCOUNT_ID` is the account the token was minted in; it is not a
credential, but it has to match the token.

### Branches and URLs

| Branch | Environment | Worker | URL |
|---|---|---|---|
| `main` | `production` | `vagrant-registry` | `https://vagrant-registry.<account>.workers.dev` |
| `staging` | `staging` | `vagrant-registry-staging` | `https://vagrant-registry-staging.<account>.workers.dev` |

A push to either branch runs the test gate and then deploys that branch's
Worker. Staging can hold experimental catalogs; production only changes on
`main`. Pull requests run CI and deploy nothing.

Both Workers keep workers.dev enabled. No zone or DNS record is required.

### Rollback

`wrangler rollback` redeploys the previous version of the Worker in one step:

```bash
npx wrangler rollback
npx wrangler rollback --env staging
```

Run it with `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` exported (and
the account selected), exactly as the deploy job uses them. After a rollback,
fix forward on the branch: the next merge deploys again.

### Custom domain (optional)

Add a `routes` entry for the hostname to `wrangler.toml` and create the
matching DNS record in Cloudflare. There are no `routes` today, so this is a
config-only change.

**Keep workers.dev enabled.** `wrangler rollback` and the staging hostname
depend on the workers.dev URL staying reachable, and boxes added under it
persist that URL in `~/.vagrant.d`.

## Docker

The image is published from `v*` tags only, to
`ghcr.io/rmoriz/vagrant-registry`. Pull a tag, or preferably a digest so a
rollback is reproducible:

```bash
docker pull ghcr.io/rmoriz/vagrant-registry:<tag>
docker run -d --read-only --name vagrant-registry -p 8080:8080 ghcr.io/rmoriz/vagrant-registry:<tag>
```

The image runs as the nonroot user `65532`, reads `PORT` for the listen port
(default `8080`), and ships a built-in `--healthcheck` that the image's
`HEALTHCHECK` calls. `--read-only` is safe: the server writes nothing to disk,
and both `data/` and the copy of the server binary are baked into the image.

The image is immutable per digest. Roll back by pulling and running the
previous tag or digest; nothing else changes.

### Front proxy

Put a TLS-terminating proxy in front of the container:

- Pass the original `Host` header through.
- Terminate TLS at the proxy; the container speaks plain HTTP.
- Forward `GET` and `HEAD` unchanged, plus `/healthz` for probes.
- Do not rewrite or normalize the path. Vagrant sends percent-encoded
  segments (`/acme%2Fbase`) and the server must see them exactly as sent.

An nginx location that satisfies this:

```nginx
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_set_header Host $host;
}
```

Point `VAGRANT_SERVER_URL` at the proxy's `https://` hostname. See
[usage](usage.md#client-setup) for client setup and the URL-persistence
caveat.
