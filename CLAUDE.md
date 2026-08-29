# ABCC 2.0 — agent context

Rust workspace. RTS-framed agent command center: the operator runs a fleet of
local model workers against real repository tasks. Read `README.md` for what it
is; this file is how to work in it.

## Commands

The loop ladder, re-measured warm on the development box, 2026-08-29, at seven
crates and 273 tests, with exit status asserted. Use the cheapest rung that
answers the question, and re-measure when the workspace grows again.

| when | command | ~time |
|---|---|---|
| after every edit | `cargo check --workspace --all-targets` | 0.42 s |
| before proposing a change | `cargo clippy --all-targets -- -D warnings` | 0.46 s |
| once, before commit | `cargo test --workspace` | 11.0 s |

🚨 **The middle rung used to end `&& cargo test --lib`, and that ran zero tests.**
Every test in this workspace is an integration test under `tests/`, which
`--lib` does not build — so the rung looked like it asserted something and
asserted nothing. It is dropped rather than repaired, because clippy over
`--all-targets` already compiles every test target.

When you want tests inside the middle rung, name them:
`cargo test --workspace --test heads --test policy --test control --test turn_loop --test workspace --test patch --test http --test journal --test view --test theme --test screen --test keys --test cli --test confirm --test home --test desk --test feed`
is **1.9 s** for 169 tests and covers everything that does not start a process.
The full suite costs 11.0 s because `tests/child.rs` and `tests/exec.rs` spawn real
children, and `abcc-vcs`, `abcc-drive` and `abcc`'s `tests/operator.rs` drive real git —
that time is processes, not compilation, and it is the price of testing claims about an
OS against the OS.
⚠ `tests/http.rs` is 0.9 s of that subset and nearly all of it is deliberate
sleeping: it drives a socket that writes when it is told to, because the claims
it makes are about *when* bytes arrive.

Run a single test by name (`cargo test <name>`) when you are iterating on one
failure.

Formatting and the toolchain are pinned in `rust-toolchain.toml`. If
`cargo fmt --all --check` disagrees with CI, that is a bug in the pin — report it,
do not reformat around it.

## Environment

- Nothing is required to build or test. The model-serving path needs LM Studio
  running locally, and its port and API key change on every model load.
- 🚨 **The `bash` tool resolves its own interpreter and does not spawn the name.**
  On this box the first `bash` on PATH is the WSL relay in the system directory,
  and with no distribution installed it answers `execvpe(/bin/bash) failed` at
  **exit 1** — which a tool layer that spawned the name would record as *the
  command failed* about a shell that never ran (F492). `ABCC_SHELL` overrides the
  search; the fallback is Git for Windows.
- The model server is not started by the test suite. Tests that need one are
  `#[ignore]`d and named `*_live_*`; run them with `--ignored` after starting it.
  `ABCC_MODEL_BASE_URL` and `ABCC_MODEL_API_KEY` point the provider somewhere
  other than `http://127.0.0.1:1234`, and `ABCC_MODEL` names the model.
- 🚨 **The hang detector is ours, not the HTTP client's** (F493). ADR-0006 rests
  on F198 — that `RequestBuilder::timeout` on `reqwest::blocking` is a *per-read*
  budget — and the test ADR-0006 called not optional failed the first time it
  ran: measured against a socket, six lines 200 ms apart under a 500 ms budget
  fail at **0.502 s on reqwest 0.12.28 and 0.509 s on 0.13.4**. It is a total
  duration on both. So `openai.rs` builds its client with **no timeout at all**
  (the blocking default is 30 s, which is why that is explicit), reads the
  response on its own thread and applies the gap with `recv_timeout` on the
  consuming side. **Do not put a `timeout()` back on that request** — it would
  cap the whole turn, and `tests/http.rs` fails if you do.
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
- 🚨 **The driver never says `Accomplished`, and that is not an omission.** There
  is no gate until the Gate milestone, so `abcc-drive` ends a working attempt
  `Uncertain { NoCheckerForArtifact }` and hands the task to the operator with a
  question that names the snapshot. Do not "finish" this by mapping a model's
  answer onto success: that is a claim standing where a measurement belongs,
  which is the one defect ADR-0009 exists to prevent.
- **A failure before `AttemptStarted` leaves the task `Deployed`.** That state's
  contract *is* "a slot is held and no attempt has started" — it is reaped on the
  spin-up bound and requeued by boot — so the driver does not invent a third
  recovery path. v1's second recovery path was the one that had never run when it
  was needed.
- **Task state transitions are append-only events.** Never mutate a task's state
  in place, and never add a generic set-status path. Emit the command, let
  `TaskState::apply` decide, and let the store write the event and the projection
  in one transaction.
- 🚨 **The reader reads the event log and never the projection.** `abcc-tui` folds
  `Logged` events into its own board even though the `task` table exists and is
  cheaper to query, because Skeleton's exit criterion is *"the reader shows that
  run without reading anything but the log"* — and a reader that queried the
  projection would keep drawing a healthy board for a milestone that had stopped
  writing events. Do not "optimise" it onto `Store::tasks`.
- **Every event variant has a line, and the compiler says so.**
  `abcc_tui::line::describe` is one exhaustive match over `Event` with **no
  wildcard arm**, which is the whole reason the reader ships in Skeleton rather
  than at Console: *"every milestone emits events in the shape Console reads"* is
  otherwise a written rule nothing checks. A new variant does not build until
  somebody has decided what an operator sees when it happens. The same holds for
  `Theme` over `TaskState` — a theme is a label map, exhaustive by construction.
- **`Outcome` has no `bool` in it.** `Headline::is_pass` is the only function in
  the workspace that produces one. If you find yourself wanting a second, the
  thing you actually want is a new `Why` variant.
- **Attempts are immutable.** Retry, edit, re-route and replay are all one
  operation — fork from a checkpoint with a `Cause` — so lineage exists by
  construction. Nothing updates an attempt row except the event that ends it.
- **A tool below the exec tier may not start a child process — including to do
  its own job.** `apply_patch` applies diffs in-process and `list_files` reads
  ignore rules in-process, rather than either of them calling git, because the
  exec class is *derived* from `ToolSpec::reach`: a `Reach::Edits` tool that
  spawned a child would be an exec-class tool admitted at the write tier, which
  is the donor defect the derivation exists to prevent. If a file tool seems to
  need a subprocess, that is the design telling you it is an exec tool.
- The isolation boundary is the tool child process, not the agent. A tool that
  spawns a shell is in the same class as one that runs code, regardless of its
  argument surface. That class is **derived** from `ToolSpec::reach` rather than
  declared in a second column, so the two cannot drift apart.
- **A prompt head cannot vary.** `Head::prefix()` takes no arguments and returns
  `&'static str`. Do not add a parameter to it — a task id or a timestamp in the
  system prefix costs a full cold prefill, and the freeze is checked end to end
  by `the_head_never_moves_and_the_body_only_grows`. Context is appended, which
  is why `Body` has no operation but `append` and `Role` has no `System`.
- **Cancelling a turn is dropping the stream.** `TurnStream` has deliberately no
  `cancel()`; the worker samples `ControlPoint::interrupted` between deltas and
  lets the stream fall out of scope. A second way to stop is a second thing that
  can be forgotten. With `openai.rs` the drop reaches the socket one hop away:
  the reader thread's next send fails and it drops the response.
- **A tool result carries the assistant turn that asked for it.** `Message` has a
  `tool_calls` field and the loop fills it, because a transcript with an answer
  and no question is a message the `OpenAI` dialect rejects and a lenient chat
  template renders as a result arriving from nowhere. That field exists because
  writing the real provider found the seam short — which is what `scripted.rs`
  is for.
- 🚨 **The log and the worktrees never live inside the repository they are about.**
  `abcc::home` computes a per-repository directory under the platform data
  directory and **refuses** an explicit one that is inside the checkout. Two
  independent reasons: a checkpoint stages the whole tree, so a log that changes
  on every event would land in every snapshot and no two snapshots of unchanged
  work would be equal; and git will not nest a worktree inside the tree it came
  from. Do not add a `.abcc/` directory to a working tree.
- 🚨 **Nothing runs against an unconfirmed model, and there is no `--force`.**
  `abcc::confirm` asks the server what it is holding and matches it against what
  was asked for, or against an operator-configured substring
  (`ABCC_MODEL_FINGERPRINT`). It needs the fingerprint because LM Studio answers a
  request naming a model it does not have **using whichever model is loaded**, and
  the bare `llama-server` names models by GGUF path — so a match can be neither
  assumed nor spelled, and only the operator can assert one. The verdict goes on
  the log as a `Note`, which is the only place the run says which brain answered:
  `ModelCallStarted` records the id that was *requested*.
- **The console edge opens a second connection, and may write exactly one event
  type.** ADR-0006 requires `ControlRequested` on the log *before* the channel is
  poked, and the driver borrows the `Store` mutably for the whole of an attempt —
  so `abcc::desk` holds its own connection and appends `ControlRequested` and
  nothing else. `Store::apply` is still the one path that moves a task. Do not
  widen what the desk writes.
- 🚨 **Only `abcc run` calls `Store::boot`.** Boot's orphan sweep tombstones the
  attempt behind any slot-holding state and requeues its task — right for a
  process that has just started, catastrophic for a second process standing beside
  a live attempt, where `abcc board` would kill the run it was opened to look at.
  Listing and operator commands open the log without reconciling it. The
  consequence is that after a crash the board shows a task still `ENGAGING TARGET`
  until the next run; that is the log telling the truth about itself, and it beats
  a listing command that changes what it lists.
- 🚨 **`abcc accept` is how a working task reaches a terminal state, and it is not
  `Accomplished`.** The driver leaves a working attempt in `AwaitingOrders`, which
  is not terminal, because nothing measured the work. `accept` commandeers the
  task and finishes it by hand — `Aborted { CompletedByOperator }` — and `reject`
  is `Aborted { Operator }`. `Fail` is deliberately not reachable from
  `AwaitingOrders`: it names an attempt, and by then the attempt is over.
- **A stopped tool child is `Cancelled`, never `exit: 1`.** `TerminateProcess`
  hands back 1, so `Killer::kill` takes a `by` and `finish()` reads it — otherwise
  the record says *the tests failed* about work nobody ran.

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
