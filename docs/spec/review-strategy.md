# Review Strategy

**Project**: whirlwind
**Language**: Rust (edition 2024)
**Last updated**: 2026-03-28
**Status**: Pre-implementation — no PR process established yet

---

## Current State

This project has no PR templates, no contribution guidelines, no CI, and no review history.
The `README.md` is empty. The codebase is a single `fn main()` stub.

This document establishes the review strategy to be applied from the first substantive PR forward,
based on the risks specific to a nascent Rust binary project.

---

## Review Dimensions (Priority Order)

The following dimensions are ranked by their current relevance to this project. Re-rank as the
codebase grows.

### 1. Code Quality (Highest Priority)

**Why first**: No linter config, no CI, and no formatter config exist yet. The earliest PRs will
establish the baseline style and conventions that all future code inherits. Drift here is expensive
to fix later.

Review for:
- Adherence to Rust naming conventions (`snake_case`, `UpperCamelCase`, `SCREAMING_SNAKE_CASE`)
- Absence of `unwrap()` / `expect()` on fallible operations in non-test code
- Use of `Result` and `?` operator for error propagation rather than `panic!`
- Presence of `#![forbid(unsafe_code)]` unless `unsafe` is explicitly justified
- No dead code (`#[allow(dead_code)]` should not be used to silence compiler warnings indefinitely)
- Module visibility (`pub`) is not broader than necessary

### 2. Correctness

Review for:
- Logic errors in conditional branches and edge cases
- Off-by-one errors in index arithmetic
- Integer overflow considerations (Rust panics on overflow in debug, wraps in release — use checked/saturating arithmetic in critical paths)
- Lifetime and ownership correctness (the compiler catches most issues, but reviewers should spot logical ownership anti-patterns)

### 3. Security

Review for:
- No secrets or credentials committed (see `docs/spec/security.md`)
- `unsafe` blocks are absent or explicitly justified with a safety comment
- New dependencies are necessary, well-maintained, and do not introduce known CVEs
- User input is validated at system boundaries before use

### 4. Testing

Review for:
- New logic has accompanying unit tests in `#[cfg(test)] mod tests`
- Tests assert behavior, not implementation details
- Test names describe the scenario being tested
- Edge cases and error paths are covered, not just the happy path

### 5. Operations

Review for:
- Structured logging is used rather than bare `println!` (once logging is introduced)
- New configuration values are documented
- Breaking changes to CLI interface or environment variable names are flagged

### 6. Performance (Lowest Priority — Premature Optimization Risk)

Performance review is explicitly deprioritized at this stage. The project has no benchmarks
and no established baselines. Premature optimization introduces complexity without measured benefit.

Apply performance review only when:
- A benchmark exists and a regression is measured
- The code path is demonstrably in the hot path
- An obvious algorithmic inefficiency is present (e.g., O(n²) where O(n) is straightforward)

---

## Rust-Specific Reviewer Checklist

Use this checklist on every PR that touches Rust source files:

- [ ] No `unsafe` blocks, or each `unsafe` block has a `// SAFETY:` comment explaining the invariant
- [ ] No `.unwrap()` on `Result` or `Option` in non-test code (use `?`, `expect("reason")`, or explicit handling)
- [ ] New dependencies in `Cargo.toml` are justified and minimal
- [ ] `pub` visibility is not broader than necessary — prefer `pub(crate)` or private
- [ ] No clippy warnings (`cargo clippy -- -D warnings` passes)
- [ ] `cargo fmt` has been run (`cargo fmt --check` passes)
- [ ] New public items have doc comments (`///`)
- [ ] Error types implement `std::error::Error` or use an established crate (`thiserror`, `anyhow`)

---

## Blocking vs. Non-Blocking Criteria

### Blocking (must be resolved before merge)

- Correctness bugs
- Security issues (committed secrets, unsafe without justification, unvalidated external input)
- Clippy errors (`-D warnings`)
- Formatting failures (`cargo fmt --check`)
- Test failures (`cargo test`)
- New `unwrap()` on fallible operations in production code paths

### Non-Blocking (should be addressed but do not block merge)

- Style suggestions beyond what rustfmt/clippy enforce
- Minor documentation improvements
- Performance suggestions without a benchmark to support them
- Refactoring suggestions unrelated to the PR's purpose

---

## Areas of Highest Review Risk

As code is added, the following areas warrant extra scrutiny:

| Area | Risk | Why |
|---|---|---|
| Dependency additions | Supply chain / binary size | Each dep expands attack surface and compile time |
| Error handling strategy | Technical debt | Established early, hard to change later |
| Public API surface | Backwards compatibility | Hard to remove once stabilized |
| `unsafe` blocks | Correctness / security | Bypasses Rust's memory safety guarantees |
| Configuration parsing | Security | Entry point for untrusted input |

---

## Existing PR Templates and Contribution Guidelines

None exist. No `.github/PULL_REQUEST_TEMPLATE.md`, no `CONTRIBUTING.md`, no `CODE_OF_CONDUCT.md`.

**Gap**: Add a minimal PR template when the first external contributor or regular review workflow is established. Suggested minimum fields:
- What does this PR do?
- How was it tested?
- Are there any breaking changes?

---

## Earliest PRs Should Establish

The first few PRs are the highest-leverage moment for quality standards. Reviewers should prioritize that these foundational items are introduced early:

1. `rust-toolchain.toml` — pin the toolchain
2. `rustfmt.toml` — explicit formatting config (even if it mostly uses defaults, making choices explicit)
3. CI pipeline — `fmt --check`, `clippy -D warnings`, `cargo test`
4. Error handling strategy — decide before the second non-trivial function is written
5. `#![forbid(unsafe_code)]` — opt-in to safe-by-default
