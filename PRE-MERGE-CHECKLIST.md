# Pre-merge checklist

This repository is public-facing: people evaluate it by reading it, not just by
running it. This checklist is what a PR author — **human or AI agent** — runs
through before requesting review. It complements
[CONTRIBUTING.md](CONTRIBUTING.md) (how to contribute) by defining *done*
(what must be true before a merge).

Not every section applies to every PR; skip what genuinely doesn't apply, but
say so in the PR description rather than silently omitting it.

## 1. Documentation matches the current state ⭐

Stale documentation is the most common defect in this repo's reviews — and the
easiest to prevent. **Verify, don't assume**: open each affected doc and read
the relevant section as a newcomer would, against what the code *now* does.
Where a doc shows a command, run the command.

Use this table to find what your change touches:

| If the PR changes… | …then check |
|--------------------|-------------|
| CLI commands, flags, exit codes, output | README [Usage](README.md#usage), the skill recipes in [`skills/sensorwatch/SKILL.md`](skills/sensorwatch/SKILL.md), the CLI's own help text |
| `config.toml` schema or defaults | README [Configuration](README.md#configuration) table, comments in the sample [`config.toml`](config.toml), the skill's config section |
| Log/event output format | README [Output format](README.md#output-format), the skill's analyze recipe, [`examples/`](examples/) that parse the format |
| The C ABI or public header | [`docs/C_ABI.md`](docs/C_ABI.md), all four bindings (cffi declarations, `sensorwatch.hpp`, regenerated `bindings.rs`, vendored header), README binding sections |
| Behavior of a binding | That binding's README section **and** its code example (paste it into a file; it must compile/run) |
| Build system, CI, or release flow | README [Building the native core](README.md#building-the-native-core-c), [CONTRIBUTING → Releasing](CONTRIBUTING.md#releasing) |
| Error codes / error behavior | The error tables in the skill, `docs/C_ABI.md`, README troubleshooting prose |
| Anything user-visible at all | README [Features](README.md#features) and [`ROADMAP.md`](ROADMAP.md) — move items from "planned" to "shipped" when a PR delivers them |

Additional rules:

- [ ] **Skill lockstep:** a PR that ships or changes a CLI/API surface updates
      the corresponding `SKILL.md` recipe *in the same PR* — never "in a
      follow-up."
- [ ] Code snippets in docs and skills are real: they were executed (or
      compiled) against this branch, not adapted from memory.
- [ ] Relative links in changed docs resolve (`grep` for `](` in changed
      files and spot-check targets exist).
- [ ] **For AI agents:** list in the PR description which documentation
      surfaces you checked and how you verified them (ran the command, compiled
      the example, diffed the table against the code). "No docs affected" is a
      claim that needs the same one-line justification.

## 2. Tests and CI

- [ ] The relevant suites pass locally: `uv run pytest` (Python),
      `ctest --test-dir build` (C, if native code changed),
      `cargo test --workspace` from `rust/` (Rust).
- [ ] New logic has tests. Parser or input-handling changes come with
      **synthetic-buffer** cases (this is how the untrusted-input guarantees
      stay tested without live hardware — see
      [Testing / CI scope](README.md#testing--ci-scope)).
- [ ] Windows-only paths degrade correctly elsewhere: asserted
      `SW_ERR_UNSUPPORTED_PLATFORM` on Linux, self-skip on Windows without
      HWiNFO.
- [ ] All CI jobs are green — including the drift gates (`bindgen-drift`,
      `vendor-sync`), `rust-msrv`, and `crates-package-check`; they fail for
      reasons `cargo test` alone won't catch.

## 3. Packaging and release invariants

Only when the PR touches versions, dependencies, or packaged files:

- [ ] Version bumps land in **all** their homes at once: `pyproject.toml` +
      `sensorwatch/__init__.py` (Python); `rust/Cargo.toml` workspace version +
      the wrapper's exact `=X.Y.Z` pin on `sensorwatch-sys` (Rust); lockfiles
      refreshed (`uv lock` / `cargo update -p …`).
- [ ] C source or public-header changes are mirrored into
      `rust/sensorwatch-sys/vendor/` and `bindings.rs` is regenerated
      ([CONTRIBUTING → Releasing](CONTRIBUTING.md#releasing) has the exact
      commands).
- [ ] New dependencies are justified in the PR description and respect the
      dependency-light principle and the Rust MSRV (1.82).

## 4. Public-repo hygiene

- [ ] No secrets, tokens, or credentials — including in test fixtures, CI
      logs quoted into the PR, and example configs.
- [ ] No private-infrastructure details: machine hostnames, network topology,
      or personal-environment specifics. Example configs and docs use generic
      placeholder values (a real sensor *product* name as a filter example is
      fine; a real alert threshold tuned to one person's machine is not).
- [ ] Committed sample data is intentional: small, explained by a nearby
      README, and something you're comfortable being public forever.
- [ ] PR title follows the existing `type: summary` convention (`feat:`,
      `fix:`, `docs:`, …) — it becomes the squash-commit headline on `master`.

## 5. Security posture

- [ ] Anything that parses external input (shared memory, config, replayed
      logs) validates bounds and lengths, and the new paths have hostile-input
      tests. See [`SECURITY.md`](SECURITY.md).
- [ ] The **read-only guarantee** holds: no code path controls hardware, and
      agent-facing guidance stays read-only.
- [ ] No new network listeners. If a change alters the attack surface
      (new input source, new output channel, new privilege), `SECURITY.md`'s
      threat model is updated in the same PR.

## 6. PR shape

- [ ] One logical change, with a description that says *what* and *why* —
      a reviewer should not need the diff to understand the intent.
- [ ] The description states how the change was verified (tests run, commands
      exercised, platforms covered) and calls out anything deliberately
      deferred.
