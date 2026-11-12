# vagrant-registry

Design and implementation plans for a **catalog-only Vagrant box registry**:
a service or static site that answers the version/provider index for
`VAGRANT_SERVER_URL`, while all `.box` binaries stay wherever they already
live.

> Status: design and plans only. No application code has been written yet.
> Date: 2026-11-13.

## Read in this order

1. [Design and contract](docs/superpowers/specs/2026-11-13-vagrant-registry-design.md)
   — what Vagrant actually calls, the JSON contract, the shared YAML source
   format, validation rules, and the four options with trade-offs.
2. One implementation plan:
   - [Option A — static catalogs](docs/superpowers/plans/2026-11-13-option-a-static-catalogs.md)
     (S3 + CloudFront for `VAGRANT_SERVER_URL`; GitHub Pages for direct
     `box_url` catalogs)
   - [Option B — file-backed service](docs/superpowers/plans/2026-11-13-option-b-file-backed-service.md)
     (small Go service, YAML files on disk, no database)
   - [Option C — database registry](docs/superpowers/plans/2026-11-13-option-c-database-registry.md)
     (Go service, SQLite, admin API, minimal admin UI)
   - [Option D — Cloudflare Workers + D1](docs/superpowers/plans/2026-11-13-option-d-cloudflare-rust.md)
     (Rust Worker, D1, same admin API/UI and YAML import, no server to run)

## Which option?

| If this is true | Choose |
|---|---|
| Catalogs change by pull request; you want zero application runtime; a few minutes of publish latency is fine | **A** |
| CI should publish immediately without an S3/Pages deploy step; you are fine running one small container | **B** |
| Non-engineers edit boxes in a browser, or you need queryable records and an admin API | **C** |
| You want C’s admin API/UI and YAML import, but do not want to operate a server or database; Cloudflare Workers + D1 is acceptable | **D** |

Recommendation for the stated goal (“registry only, no hosting”): **A**.
It has the smallest operational surface and is fully compatible with
`VAGRANT_SERVER_URL` when deployed to S3 + CloudFront. Choose B when
publishing must be decoupled from static deployment, C when a UI or many
editors justify a database, and D when you want C’s feature set as a
managed serverless deployment.

## Important

These are **alternative** plans, not a sequence. Pick one. If you want to
compare them side by side, run each in its own branch or worktree; do not
implement more than one in the same working tree.

Options A/B/C use Go 1.22+; Option D uses Rust 1.91 on Cloudflare Workers.
All four share the same catalog schema and validation rules from the design
doc. A and B use the same YAML files; C and D import them and expose the
same JSON contract through an API.

## Repository layout the plans produce

```text
.
├── README.md
├── docs/
│   ├── superpowers/
│   │   ├── specs/2026-11-13-vagrant-registry-design.md
│   │   └── plans/
│   │       ├── 2026-11-13-option-a-static-catalogs.md
│   │       ├── 2026-11-13-option-b-file-backed-service.md
│   │       ├── 2026-11-13-option-c-database-registry.md
│   │       └── 2026-11-13-option-d-cloudflare-rust.md
│   └── ... deployment runbooks created by the chosen plan
└── (application files created by the chosen plan)
```
