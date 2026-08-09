# MCDX deploy artifacts — GitHub CI contract (Aug 2026)

Paste-ready contract shared across MCDX app repos. **mcdx-stack** Ansible expects this when `mcdx_artifact_source=github` (testnet).

## Model

| Environment | Who deploys | Artifact source |
|-------------|-------------|-------------------|
| **testnet** | `ded` (laptop or on-box stack builder) | **GitHub** — CI on `main` publishes artifacts; `ded` waits for green `ci.yml`, then Ansible pulls the release/GHCR tag `ci-<sha7>` |
| **mainnet** | `mcdx snapshot promote` (on demand) | Same GitHub artifacts, but only when a snapshot pins specific `ci-*` tags — no auto-deploy on every commit |

`ded` stays. Local `local-build.sh` / `build-for-deploy.sh` becomes break-glass only.

## mcdx-ql (this repo)

**Exception:** no `ci-<sha7>` GitHub Release deploy artifact.

| What | How |
|------|-----|
| CI workflow | `.github/workflows/ci.yml` (`name: ci`) |
| On `main` | `test` → `publish` deploys `com.nilpferdschaefer:mcdx-ql:<version>-SNAPSHOT` to GitHub Packages (Maven) |
| On tag `v*.*.*` | `release.yml` deploys immutable `com.nilpferdschaefer:mcdx-ql:<version>` |
| Stack rollout | Bump `mcdx_ql_version` in **mcdx-stack** — ql commits do **not** auto-redeploy consumers (`ded` watch mode) |

Maven package includes: `mcdx-ql-*.jar`, `mcdx-ql-*-javadoc.jar`, `mcdx-ql-*-crate.crate` (Rust `cargo package`).

Rust sibling repos still use **git** dependencies (Packages is not a Cargo registry).

## Other deployable repos (reference)

| Repo | CI publishes | Ansible pulls |
|------|--------------|---------------|
| **mcdx-datastore** | Release `ci-<sha7>`: `agent` + `agent.sha256` | Release API |
| **mcdx-analytics** | Release `ci-<sha7>`: `analytics-indicators.jar` + `.sha256` | Release API + repo tarball |
| **mcdx-executor** | Release `ci-<sha7>`: `mcdx-executor` + `.sha256` | Release API + repo tarball |
| **mcdx-ram** | GHCR `ghcr.io/nilpferdschaefer/mcdx-ram:ci-<sha7>` | `docker compose pull` |

Shared rules for those repos: workflow file `ci.yml`; triggers `push`/`main`, `pull_request`, `workflow_dispatch`; release tag `ci-<7-char-sha>`; no SSH/Ansible from app CI; test job must fail the workflow on failure; release job only on `main` push after tests.

## Verify (this repo)

```bash
# After push to main — SNAPSHOT in GitHub Packages
gh api "/orgs/nilpferdschaefer/packages/maven/com.nilpferdschaefer/mcdx-ql/versions" \
  --jq '.[0].name'

# CI green
gh run list --workflow=ci.yml --branch=main --limit=1
```

## mcdx-stack follow-up (not in this repo)

1. `ansible/inventories/testnet/.../vars.yml` → `mcdx_artifact_source: github` (for deployable apps, not ql)
2. `config/ded.json` → `build.mode: github_actions`, `artifact: release`, `release_tag: ci-{sha_short}`
3. `ded_mcdx.py` → accept `artifact: release` for `github_actions` mode
