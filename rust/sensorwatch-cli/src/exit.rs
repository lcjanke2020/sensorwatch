//! The CLI-wide process exit-code contract (fixed by LEO-336).
//!
//! Agents dispatch on these codes, so they are a stable interface, not an
//! implementation detail. Only `watch.rs` constructs its exits from these
//! constants; the frozen `snapshot.rs`/`logger.rs` paths keep their own
//! literal `0`/`1`/`2` to avoid churning byte-compat-tested code.
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0    | Clean: snapshot printed; `log` clean shutdown; `watch` one-shot timeout with no event (heartbeat); `watch` replay exhausted. |
//! | 1    | Fatal: platform/source startup failure; signal-handler install failure; state/log/spool directory or seq-store *preparation* failure; `watch.seq` persistence failure. |
//! | 2    | Usage: clap errors (automatic); invalid `[[rules]]`; zero rules configured; zero rules after filters; unknown `--rule` name. |
//! | 10   | `watch` one-shot: a rule fired (the JSON event is on stdout). |
//! | 130  | Interrupted by signal — `watch` only, both modes, including Windows Ctrl-C (`log` keeps its documented Ctrl-C = 0). |
//!
//! Exit 1 covers *preparation* plus the `watch.seq` integrity anchor only.
//! Per-record `events_`/spool **writes** are best-effort: a failure is warned
//! and swallowed so a follow watcher survives disk pressure, and because `seq`
//! is monotonic (not dense) a skipped event is contractually acceptable, not
//! fatal — the visible seq gap is what ack cursors reconcile against.
//!
//! Source loss is deliberately NOT an exit code: it surfaces as the
//! `source-unavailable` rule *kind* in an event, so agents dispatch on event
//! content rather than a process code. Exit 0 (clean) is `ExitCode::SUCCESS`
//! and needs no constant here.

/// Fatal: startup or runtime failure the watcher cannot proceed through.
pub(crate) const FATAL: u8 = 1;

/// Usage: invalid invocation, rules, or filters.
pub(crate) const USAGE: u8 = 2;

/// `watch` one-shot: the first rule fired; the event JSON is on stdout.
pub(crate) const EVENT_FIRED: u8 = 10;

/// Interrupted by a signal (`watch` only, both modes).
pub(crate) const INTERRUPTED: u8 = 130;
