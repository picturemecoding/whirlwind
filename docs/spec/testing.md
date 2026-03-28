# Testing Specification

## Current State

This project is in its initial scaffold state. As of the current snapshot, the repository contains:

- `Cargo.toml` — a minimal package manifest with no dependencies and no `[dev-dependencies]`
- `src/main.rs` — a single-entry binary containing only the generated `fn main()` stub
- No test modules, no test files, no CI configuration, no coverage tooling

There are **no tests of any kind in this codebase today.**

This document records that ground truth and establishes the conventions that should be followed
as the project grows, based on idiomatic Rust practice for a binary crate.

---

## Test Runner

The project uses the standard Rust toolchain. The test runner is `cargo test`, which is built
into Cargo and requires no additional configuration for basic use.

```
cargo test
```

No alternative test harnesses (e.g., `cargo-nextest`) are configured. If `nextest` is adopted
later, a `.config/nextest.toml` should be added and this file updated.

---

## Test Pyramid

### Current Breakdown

| Level | Count | Notes |
|---|---|---|
| Unit | 0 | No `#[test]` functions exist |
| Integration | 0 | No `tests/` directory exists |
| End-to-end | 0 | No e2e harness configured |

### Expected Breakdown as the Project Grows

Because this is a binary crate, the expected test distribution once meaningful code exists:

- **Unit tests (majority)** — inline `#[cfg(test)] mod tests { ... }` blocks within each
  source module. These test individual functions and logic in isolation.
- **Integration tests (supporting)** — files under `tests/` at the crate root. These test
  the binary's public interface or assemble multiple modules to verify end-to-end behavior
  of a subsystem. Integration tests in Rust compile as a separate crate, so they can only
  access public API surface.
- **End-to-end / binary invocation tests (selective)** — invoking the compiled binary via
  `std::process::Command` in integration tests, or via a shell-based harness, to verify
  CLI behavior and output. These are expected to be sparse and targeted.

---

## Coverage

No coverage tooling is configured. There is no `tarpaulin.toml`, no `llvm-cov` setup, and no
`.cargo/config.toml` with instrumentation flags.

When coverage is added, the two standard options for Rust are:

- **`cargo-tarpaulin`** — simpler setup, Linux-only, appropriate for CI environments that
  run on Linux runners.
- **`cargo-llvm-cov`** — broader platform support, more accurate instrumentation, requires
  `llvm-tools-preview` via `rustup component add llvm-tools-preview`.

Neither is currently present. Coverage is a gap.

---

## Test Utilities, Fixtures, and Mocking

None exist. There is no `tests/` directory, no fixture data, and no helper modules. The
`[dev-dependencies]` section in `Cargo.toml` is absent entirely.

Common additions to expect as the codebase grows:

- `[dev-dependencies]` entries in `Cargo.toml` for test-only crates
- A `tests/` directory for integration tests
- A `tests/fixtures/` or `tests/data/` directory for static test data files
- Helper modules (e.g., `tests/common/mod.rs`) for shared test setup logic

Mocking: Rust does not have a universal mocking framework. Common choices are `mockall` (for
trait-based mocking) and `wiremock` (for HTTP service mocking). Neither is a dependency today.
The project should prefer designing around traits that are straightforward to substitute in
tests rather than reaching for mocking frameworks prematurely.

---

## CI Configuration

There is no CI configuration in this repository. No `.github/`, no `.circleci/`, no `Makefile`,
no `justfile`. The `docs/spec/` directory itself is the first non-source artifact added.

When CI is introduced, the minimum expected test step is:

```yaml
- run: cargo test --all-targets --all-features
```

A `cargo clippy` step and a `cargo fmt --check` step are also conventional for Rust projects
and should accompany the test step.

---

## Gaps

The following are honest gaps in the current state of testing for this project:

1. **No tests exist.** The only source file is a stub `main()` function with no testable logic.
2. **No `[dev-dependencies]`.** No test utilities or assertion helpers are available.
3. **No integration test directory.** The `tests/` directory at the crate root does not exist.
4. **No coverage tooling.** No instrumentation is configured or documented.
5. **No CI.** Tests are not automatically run on commit or pull request.
6. **No test conventions documented in code.** No examples of how tests are structured exist for contributors to follow.

---

## Conventions to Follow When Tests Are Added

These are the idiomatic Rust conventions this project should adhere to:

- Unit tests live in the same file as the code they test, inside a `#[cfg(test)] mod tests` block at the bottom of the file.
- Integration tests live in `tests/` at the crate root, one file per logical area of behavior.
- Test names use `snake_case` and should read as a sentence describing the behavior under test (e.g., `returns_error_on_empty_input`, not `test1` or `testErrorCase`).
- Tests should assert observable behavior, not implementation details.
- Each test should have a single logical assertion or a tightly scoped group of assertions for one scenario.
- Shared test setup should be extracted into helper functions within the `mod tests` block or, for integration tests, into `tests/common/mod.rs`.
- Do not use `unwrap()` in production code paths; in tests, `unwrap()` is acceptable but `expect("descriptive message")` is preferred for faster diagnosis on failure.

---

_Last updated: 2026-03-28. Reflects the initial scaffold state of the project._
