# Release Process

tiderace uses **trunk-based development** with **semantic versioning**. Releases are fully automated via CI — developers never manually tag or bump versions.

## Version Rules

| Change type | Who can trigger | Version bump |
|---|---|---|
| Bug fix, docs, perf | Any PR | patch `0.0.x` |
| New feature (backwards compatible) | Any PR | minor `0.x.0` |
| Breaking change | **CI only** (via `BREAKING_CHANGE=true` env) | major `x.0.0` |

**Major version bumps never happen from a developer's local machine.** The `BREAKING_CHANGE` gate is enforced exclusively in the release workflow.

## Workflow Overview

```
developer push → main
       ↓
  CI: test + lint
       ↓
  CI: compute next semver from commits
       ↓
  CI: build binaries (linux-x86_64, linux-arm64)
       ↓
  CI: create GitHub Release + upload binaries
       ↓
  CI: publish docs to GitHub Pages
```

## CI Workflows

See `.github/workflows/` for the full definitions:

| File | Trigger | Purpose |
|---|---|---|
| `ci.yml` | Push to any branch, PRs | Test, lint, format check |
| `release.yml` | Push to `main` | Semver compute, build, release |
| `docs.yml` | Push to `main` | Build and deploy MkDocs site |
| `security.yml` | Schedule (weekly) | `cargo audit` dependency scan |

## Caching in CI

The release workflow caches `.tiderace.db` between runs keyed on branch name:

```yaml
- uses: actions/cache@v4
  with:
    path: .tiderace.db
    key: tiderace-db-${{ github.ref_name }}-${{ hashFiles('tests/**') }}
    restore-keys: |
      tiderace-db-${{ github.ref_name }}-
      tiderace-db-main-
```

This means CI gets the same impact-analysis benefits as local development — only changed-file tests re-run between commits on the same branch.
