# ABCC 2.0 — agent context

Rust workspace. RTS-framed agent command center: the operator runs a fleet of
local model workers against real repository tasks. Read `README.md` for what it
is; this file is how to work in it.

## Commands

The loop ladder, measured warm on the development box, 2026-08-28, with exit
status asserted. Use the cheapest rung that answers the question, and re-measure
these when the workspace grows — they are small today because it is one crate.

| when | command | ~time |
|---|---|---|
| after every edit | `cargo check --workspace --all-targets` | 0.15 s |
| before proposing a change | `cargo clippy --all-targets -- -D warnings && cargo test --lib` | 0.42 s |
| once, before commit | `cargo test --workspace` | 0.5 s |

Run a single test by name (`cargo test <name>`) over a whole module when you are
iterating on one failure.

Formatting and the toolchain are pinned in `rust-toolchain.toml`. If
`cargo fmt --all --check` disagrees with CI, that is a bug in the pin — report it,
do not reformat around it.

## Environment

- Nothing is required to build or test. The model-serving path needs LM Studio
  running locally, and its port and API key change on every model load.
- The model server is not started by the test suite. Tests that need one are
  `#[ignore]`d and named `*_live_*`; run them with `--ignored` after starting it.
- `target/` gets large. Do not clean it to free space without asking.

## Style rules that differ from defaults

- **The RTS domain vocabulary is load-bearing.** `Queued`, `Deployed`, `Engaged`,
  `AwaitingOrders`, `Holding`, `Commandeered`, `Accomplished`, `Failed`,
  `Aborted` are the type names, chosen against the voice lines the console
  speaks. Do not "correct" them toward functional naming — a theme is a label map
  over these fixed names, and `classic` is one of them.
- Errors are typed per crate and never `anyhow` across a public boundary.
- **A number in a comment is quoted from a named finding** (`F146`, `W3`,
  `ADR-0004`). Do not write a figure you cannot cite, and do not update one
  without re-deriving it.

## Architecture you cannot infer from one file

- **A model verdict is a report, never a gate.** Only deterministic phases may
  refuse — this is written as code in `AttemptPhase::may_refuse`, so wiring the
  Judge into a gate is an edit somebody can see rather than a quiet one.
- **Task state transitions are append-only events.** Never mutate a task's state
  in place, and never add a generic set-status path. Emit the command, let
  `TaskState::apply` decide, and let the store write the event and the projection
  in one transaction.
- **`Outcome` has no `bool` in it.** `Headline::is_pass` is the only function in
  the workspace that produces one. If you find yourself wanting a second, the
  thing you actually want is a new `Why` variant.
- **Attempts are immutable.** Retry, edit, re-route and replay are all one
  operation — fork from a checkpoint with a `Cause` — so lineage exists by
  construction. Nothing updates an attempt row except the event that ends it.
- The isolation boundary is the tool child process, not the agent. A tool that
  spawns a shell is in the same class as one that runs code, regardless of its
  argument surface.

## Repository etiquette

- Conventional Commits. One logical change per commit.
- Work happens in a worktree, one per task. Never edit the operator's checkout
  directly.
- Agent-authored commits carry `Co-authored-by:` so authorship stays countable
  after squash.
- ADRs live in the research repository and are **append-only**. When reality
  diverges from an ADR, write a new one that supersedes it. Never edit an ADR to
  match what shipped.

## What you cannot do, and why it is not a list here

Capabilities are meant to be removed, not requested: if an action is not
available to your role, the tool should be absent from your set or the permission
tier should refuse it. There is no honour-system list of forbidden commands in
this file, because a sentence binds only as far as the model complies — measured
at 39 of 50 for the best-written wrapper in the family.

⚠ **`max_tier` does not exist yet.** It arrives with the Posture milestone, so
until then this section describes the intended enforcement and not the shipped
one. Say so rather than relying on it.

If you believe you need a capability you do not have, say so and stop. Do not
route around it with a different tool.
