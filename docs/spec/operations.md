# Operations Specification

**Project**: whirlwind
**Language**: Rust (edition 2024)
**Last updated**: 2026-03-28
**Status**: Pre-implementation — no operational infrastructure exists

---

## Current State

This project is a brand-new Rust scaffold. No operational infrastructure of any kind exists:

- No `.github/` directory — no CI/CD workflows
- No Dockerfile or container configuration
- No deployment manifests (Kubernetes, Nomad, ECS, etc.)
- No infrastructure-as-code (Terraform, Pulumi, CDK)
- No monitoring, logging, or observability setup
- No runbooks or operational documentation
- No Makefile, Justfile, or task runner

The only artifacts are `Cargo.toml`, `src/main.rs`, `.gitignore`, and `README.md` (empty).

---

## Build

### Local Build

```bash
cargo build           # debug build
cargo build --release # release build, output: target/release/whirlwind
```

No custom build scripts (`build.rs`) exist. No feature flags are defined.

### Gaps

- No `rust-toolchain.toml` — toolchain version is not pinned; builds may differ across developer machines
- No `.cargo/config.toml` — no linker overrides, target configuration, or build flags
- No release profile tuning in `Cargo.toml` (e.g., `opt-level`, `lto`, `strip`)

---

## CI/CD

No CI/CD pipeline exists. No `.github/workflows/`, no CircleCI, no GitLab CI, no Buildkite.

**Gap**: When CI is introduced, the minimum viable pipeline for a Rust binary crate should include:

```yaml
jobs:
  check:
    steps:
      - run: cargo fmt --check
      - run: cargo clippy -- -D warnings
      - run: cargo test --all-targets --all-features
      - run: cargo build --release
```

Recommended additions once the project matures:
- `cargo audit` for CVE scanning
- `cargo deny` for license and advisory policy
- Release artifact upload on version tags

---

## Deployment

No deployment target, process, or environment has been defined.

**Gap**: Document here when established:
- Deployment target (bare metal, VM, container, serverless, WASM)
- Container image build process if applicable
- Deployment mechanism (push-based vs. pull-based, platform-specific)
- Environment promotion path (dev → staging → production)

---

## Environments

No environments are defined. No environment-specific configuration exists.

**Gap**: Define environment management when the project acquires configuration or external dependencies:
- How environment-specific config is injected (environment variables, config files, secrets manager)
- Which environments exist and their purposes
- How to run the service locally vs. in CI vs. in production

---

## Configuration

The binary currently accepts no configuration. When configuration is introduced, document:
- Configuration sources and their precedence (CLI args > env vars > config file > defaults)
- Required vs. optional configuration values
- How configuration is validated at startup
- Sensitive values and how they are handled (see `docs/spec/security.md`)

---

## Observability

### Logging

No logging is present. The binary only calls `println!`. When logging is introduced, the idiomatic Rust choice is the `log` facade crate with a backend such as `env_logger` (simple) or `tracing` + `tracing-subscriber` (structured, async-friendly).

### Metrics

No metrics infrastructure exists.

### Tracing / Distributed Tracing

No tracing infrastructure exists. If the project grows into an async service, `tracing` with OpenTelemetry export is the standard Rust approach.

### Alerting

Not applicable at this stage.

---

## Release Process

No release process is defined. No version tagging convention, no changelog, no release artifacts.

**Gap**: Establish before the first release:
- Versioning scheme (SemVer recommended — already implied by Cargo conventions)
- Changelog format (e.g., `CHANGELOG.md` following Keep a Changelog)
- Release artifact distribution (GitHub Releases, package registry, container registry)
- Tag naming convention (e.g., `v0.1.0`)

---

## Rollback

No rollback procedure exists. No deployment means no rollback to define.

**Gap**: When deployment is established, document the rollback procedure:
- How to redeploy the previous version
- Maximum acceptable rollback window
- Data migration rollback considerations if a database is introduced

---

## On-Call and Runbooks

No on-call rotation or runbooks exist.

**Gap**: When the service becomes operational, create runbooks for:
- Service restart procedure
- Common failure modes and their remediation
- Escalation path

---

## Summary of Gaps

| Area | Status |
|---|---|
| CI/CD pipeline | Not present |
| Deployment target | Not defined |
| Container / packaging | Not present |
| Environment management | Not defined |
| Logging | Not present |
| Metrics | Not present |
| Distributed tracing | Not present |
| Release process | Not defined |
| Rollback procedure | Not defined |
| Runbooks | Not present |
| `rust-toolchain.toml` | Not present |

This document should be updated as each operational concern is introduced.
