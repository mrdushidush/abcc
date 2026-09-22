# ABCC 2.0 — agent context

Rust workspace. RTS-framed agent command center: the operator runs a fleet of
local model workers against real repository tasks. Read `README.md` for what it
is; this file is how to work in it.

## Commands

The loop ladder, re-measured warm on the development box, 2026-08-30, at eight
crates and 329 tests, with exit status asserted. Use the cheapest rung that
answers the question, and re-measure when the workspace grows again.

| when | command | ~time |
|---|---|---|
| after every edit | `cargo check --workspace --all-targets` | 0.38 s |
| before proposing a change | `cargo clippy --all-targets -- -D warnings` | 0.42 s |
| once, before commit | `cargo test --workspace` | 16.8 s |

🚨 **Measure it with the output going to `/dev/null`, not into a shell
variable.** The same `cargo test --workspace`, same build, back to back:
**15.3 s redirected and 63.0 s captured** with `out=$(...)`, for 610 lines of
output. The 47 s is this platform's pipe handling against 32 test binaries and
the children they spawn, and it is not in the tests — the harness's own reported
times sum to ~13 s either way. **An instrument that reads the output changes the
number by 4×**, which is the same shape as F202 one level up: the pipe is the
cost, not the work.

🚨 **The middle rung used to end `&& cargo test --lib`, and that ran zero tests.**
Every test in this workspace is an integration test under `tests/`, which
`--lib` does not build — so the rung looked like it asserted something and
asserted nothing. It is dropped rather than repaired, because clippy over
`--all-targets` already compiles every test target.

When you want tests inside the middle rung, name them:
`cargo test --workspace --test assets --test battlefield --test breaker --test cli --test confirm --test control --test corpus_review --test desk --test durability --test endings --test feed --test films --test frame_cost --test fun --test heads --test home --test http --test journal --test judge --test keys --test lifecycle --test lines --test patch --test policy --test properties --test pulse --test reading --test redact --test roster --test screen --test sixel --test theme --test turn_loop --test view --test weights --test workspace`
covers everything that does not start a process: **36 names of the 49**, 412
tests, re-derived 2026-09-11 when Posture added `redact` and `weights`.

🚨 **That list is derived, not remembered — check it against `crates/*/tests/`
whenever a test file is added.** It has now drifted twice. The first time it
omitted five process-free targets and 57 tests; **re-derived 2026-09-07 it was
24 names against 47 targets and omitted ten of them and 92 tests** — `assets`,
`battlefield`, `breaker`, `corpus_review`, `films`, `frame_cost`, `fun`,
`pulse`, `roster` and `sixel`. **A list of names does not fail when the
workspace grows; it just stops covering things**, and the number beside it
(251, then 291 when the same list was re-run) goes on looking like a
measurement.

The process bucket is exactly `attempt`, `child`, `corpus`, `cycle_cost`,
`durability_rate`, `exec`, `isolation`, `ladder`, `live`, `operator`, `paint`,
`replay` and `sortie` — thirteen of the forty-nine named targets, so the
subset is the other thirty-six. ⚠ **`replay` is in the bucket for a reason
that is not about processes**: two crates have a target of that name
(`abcc-core` and `abcc`, and the same is true of `fun`), and `--test replay`
runs both, so the pair is as cheap as its more expensive half.

The full suite is **575 tests** (19 ignored), and almost all of the time is in
seven targets:
`ladder` 3.5 s, `attempt` 2.7 s, `sortie` 2.5 s, `exec` 2.5 s, `child` 2.3 s,
`operator` 2.0 s and `isolation` 1.3 s — real children, real git, and in
`ladder`'s case both. That time is processes, not compilation, and it is the
price of testing claims about an OS against the OS.
⚠ `tests/http.rs` is 0.9 s of the process-free subset and nearly all of it is
deliberate sleeping: it drives a socket that writes when it is told to, because
the claims it makes are about *when* bytes arrive.

⚠ **`tests/live.rs` is not in the suite at any price.** It is `#[ignore]`d and
points the gate at real checkpoint pairs, which means a cold `cargo` build per
changed tree — **373 s and one 2.3 GB `target/` at a time** for the 25 attempts
on this project's log. Run it deliberately, never in a loop.

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
- 🚨 **Only a measurement says `Accomplished`, and the measurement is
  `Headline::Green`.** The Gate milestone made that word reachable and did not
  make it cheap: `Green` requires **every declared rung to have produced a
  measurement** and none of them to be red. ⚠ It does **not** require the model
  to have said it was finished — an ending it never chose is measured (F655) and,
  since the operator's ruling of 2026-09-10, promoted. There is no driver-side
  check that the tree *changed*, and adding one would be a defect: `Rung::Structural`
  refuses an unchanged tree, runs first, and a refusal breaks the walk, so
  **green already implies updated**. What still cannot reach it is
  anything a model wrote — its verdict is a `Claim`, and there is no function in
  the workspace that turns one into an `Outcome`. Do not add one.
- 🚨 **The conjunction is `Report::headline`, not code in `abcc-gate`.** ADR-0008's
  `Accept ⇔ structural ∧ acceptance ∧ ¬Veto` is not implemented anywhere; the
  gate produces the right `Outcome`s in the right order and the type does the
  `∧`. That is why the Judge cannot vote now that it is here: a `Claim` attaches
  through `Report::note` and there is nothing to wire it to.
- 🚨 **The Judge is built, it reports, and the rule runs in BOTH directions.** A4
  is one model call, no tools (`Head::Commandos` is capped at `Tier::NoTools`, and
  `NoTools` is the tool layer it gets), constrained to `judge::REVIEW`, over a
  fresh body holding the task, the diff and the rungs. It cannot refuse — its
  answer is a `Claim` and `AttemptPhase::may_refuse` is `!uses_model()`. **And
  its own failure is not the attempt's**: a review that times out, says nothing
  or comes back malformed leaves the ending exactly where the ladder put it,
  which is `abcc-drive`'s rule 6 and is asserted by
  `a_review_that_never_arrives_changes_nothing_about_the_ending`.
- 🚨 **What the Judge reads is the whole of what it is worth, and the list is a
  struct.** `judge::Dossier` has four fields and no fifth: same model, fresh
  call, **11/12** reading the diff, **5/12** reading the author's completion
  report, **0/3** when the report is added *alongside* the diff — the author's
  prose is not merely unhelpful, it is subtractive (F280–F282). So neither
  Builders' claim nor Recon's brief is in it, `Head::Commandos`' charter was
  corrected to say so, and putting either back is a change to the type rather
  than a change to a format string.
- **The Judge is not asked about an empty diff, and the log says why.** The
  structural rung refuses an unchanged tree on 16 of this project's 25 logged
  attempts (F518), so *not asked* is the common case and a model call not made.
  The other refusal is a diff over `judge::MAX_PATCH_CHARS`, which is **not**
  truncated to fit: a review of part of a change is a review of a different
  change and arrives indistinguishable from a review of the whole one.
- 🚨 **The ladder stops at the first refusal and never at an absence.** A red is
  a decision, and there is nothing after it worth a cold build; an absence is
  not, so the rungs after it still run and the report says everything it saw.
  This cannot produce a wrong `Green`, because `Green` means nothing refused,
  which is the case where every rung ran.
- 🚨 **A rung nobody declared is absent, not missing.** The standard rung
  (`cargo clippy -- -D warnings`) is declared only when the repository carries
  the witness for it — a `clippy.toml`. A workspace with no witness has three
  rungs and a `Green` that means what it says, rather than a fourth `Unmeasured`
  for a check nobody asked for. **The gate enforces what a repository asks for
  and nothing it does not.**
- 🚨 **The gate may not share a build cache** (F356): two trees with one package
  name and one `CARGO_TARGET_DIR` make cargo print `Fresh`, run the *other*
  tree's binary and report `ok. 0 passed` at exit 0. Nothing has to be done to
  get this right — `CARGO_TARGET_DIR` is not on `ENV_ALLOWLIST`, so a rung child
  never inherits one. Do not add it. The price is a cold build per attempt:
  measured at **55 s and 2.3 GB** on this workspace.
- 🚨 **A task may not go terminal while something is owed to a person.** A
  refused attempt goes to `AwaitingOrders` and recommends a retry; those do not
  disagree, because an operator prompt is exactly *here is what I would do, say
  the word*. Landing a refusal on `Failed` would make the recommendation
  unreachable and throw away work that is often one line from landing.
- 🚨 **A phase asks again before it gives up on a missing answer** (ADR-0016,
  F503). The champion reasons to the end and emits five to nine tokens that trim
  to an empty string; across ten runs of one task the closing answer was missing
  from **8 of 10 phases**, and **one nudge recovered 5 of 5**. `Limits::nudges`
  is 2, the sentence lives in `NO_ANSWER` at the *end* of the body — never the
  head, F81 — and exhausting it still ends the phase `Why::SaidNothing`. Every
  nudge is an `Event::PhaseNudged`. ⚠ Do not treat a first empty answer as a
  verdict: the same head and brief produced 2,008- and 3,431-character claims on
  the runs that worked, so it is a sample.
- 🚨 **A `Finish::Length` turn ends the phase and NONE of its tool calls run**
  (ADR-0016, F506). A payload cut mid-token is a fragment and a tool call cut
  mid-argument is not a request: twice the last call arrived with a
  **zero-character** argument string and was recorded as the model failing its
  own schema, and appending that turn is what produced both contentless HTTP
  500s. `content_empty` and the F498 comparison select *which* `Why` — never
  whether there is one. ⚠ `a422` was 8,209 + 8,192 = 16,401 against a 32,768
  window, so do not reach for the context window to explain a 500.
- 🚨🚨 **A card's test must be shown RED on the unfixed tree, by running it
  there. The gate cannot do this for you** (F832). It runs `cargo test` and
  sees green whether or not the new test exercises the change, and **two of
  three landed cards have carried a test that passes unfixed**: `SHELL-10`'s
  child reads 0 bytes of stdin whether stdin is `null` or **inherited**,
  because under `cargo test` the parent's stdin is already at EOF; and
  `RUNTIME-10b` built its *deeply nested* JSON as `"[]"` repeated, which is
  flat and errors on trailing content long before any depth check. Both
  production fixes were correct and both tests proved nothing. ▶ The check is
  cheap and there is no substitute: extract the test, put it on the pre-fix
  tree, watch it fail. 🎉 The **judge** caught one of these and **decided
  nothing**, which is ADR-0009 working — the first finding in this project's
  history that the deterministic rungs could not produce.
- 🚨 **An inspecting tool's result the body already carries, byte for byte, is
  replaced by a back-reference** (F826, F828, F830). Thirteen byte-identical
  whole-file reads filled a 40,960 window inside one attempt and it died having
  changed nothing; whole-file reads are 54.1% of tool calls and **92.5% of the
  bytes**. ⚠ **The prompt is not the lever and that was measured** — a paragraph
  written against this exact behaviour, quoting its own numbers, moved the
  re-read rate 89.9% → 88.0% (F827). The state is the **`Body` and never the
  `Workspace`**: a `Body` is fresh per phase, a `Workspace` is one instance
  shared across Localize and Change, so dedup state on the workspace would
  substitute in a phase whose body never saw the original. ⚠ **`Reach::Inspects`
  only** — two identical `applied 1 hunk to 1 file` lines are two applied
  patches, and 12 of the 150 repeats in the whole log are exactly that class.
  ⚠ The escape valve (every 4th repeat served whole) is a **design guess with no
  rate behind it**, there because `body` is not guaranteed to equal what the
  server retained; say so, and measure it.
- 🚨 **A turn that reasons past `Limits::reasoning_ceiling` is ended by abcc,
  mid-stream** (F829). **50,000 characters, and characters is not a
  convenience**: `usage.reasoning_tokens` arrives only in the closing usage
  block, so the published 12,288-*token* rule was unwireable; the loop's one
  live counter is `reasoning_chars`. Re-derived over **3,048 turns** — every
  ceiling from ~41,000 to ~60,000 catches 5 and costs **0** false positives, so
  it looked like a plateau. 🚨 **F831: it is not one, and the field data said
  so.** `a2065` holds a **barren** turn at 26,275 reasoning chars and a
  **productive** one at 34,760 **in the same attempt**, so the two populations
  are not separable by a threshold and no ceiling catches the first without
  discarding the second. **5 of 6 on turns, not 5 of 5.** ▶ So the stop also
  requires the turn to have produced nothing — no text, no assembled call, no
  argument bytes, no `ToolCallOpened`. That can only *prevent* firings, and it
  does **not** make the rule complete: F624 buffers a tool call's arguments to
  the end, so the threshold is still doing the real work. ⚠ It catches one
  shape — one turn that reasons itself to the cap, never an attempt that dies
  through many small rounds. ⚠ One of the five ends `stop`, not `length`, so do
  not reach for the finish reason. 🚨 **`--reasoning-ceiling 0` turns it off,
  and that flag is not a convenience**: the ceiling ended `a2468`, which made
  *would that turn have produced anything* unobservable, because the
  observation is the thing the stop prevents. **A stop that cannot be taken out
  of the path cannot be measured.**
- 🚨 **Text out of a tool call reaches the log and the model's context only as
  a `Scrubbed`, and there is one constructor** (ADR-0014 §5). `redact::Secrets`
  is applied at exactly one seam — `TurnLoop::tool_round` — because a tool's
  output has three sinks (the durable log, the console that projects it, the
  model's own context) and scrubbing at each would be three denylists that agree
  until one is edited. **Do not add a `From<String> for Scrubbed`**: the missing
  conversion is what makes a new path to either sink a compile error. ⚠ It is a
  **backstop and not the control** — the control is that a role without
  `Tier::Exec` cannot run `cat` at all — and a denylist over text is the shape
  W7 measured at 39 of 50 and rejected. Say that, do not oversell it.
- **A refused tool call keeps its arguments on the log; a successful one does
  not, and a denied one does not either** (F505). Five `apply_patch` refusals
  were once undiagnosable after the fact. The denial case stays empty because
  ADR-0014's control is the class — see `Why::Denied`.
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
- 🚨 **That sentence was aspirational until 2026-09-11, and `Driver::fork_point`
  is what makes it true.** `open_workspace` snapshotted the operator's checkout
  unconditionally and never read `Cause`, so every retry and every redirect threw
  its parent's work away — while seven places in three crates, this bullet
  included, said otherwise (F701, F702). All seven agreed with each other and
  none agreed with the code: **a comment and a prompt that agree are one witness,
  not two.** A retry, an edit and a rescope now open on the task's latest
  checkpoint (`abcc take`'s rule, so the operator's hand-back is not stepped
  over); a replay opens where the attempt it replays *started*; `Cause::Fresh`
  still gets the checkout, which is what `abcc run` relies on.
- 🚨 **The gate's diff is what *this attempt* changed, and that is a control
  rather than a detail.** A forked attempt reuses its parent's checkpoint as its
  own opening one, so `checkpoint_from` names a real ancestor. Take that away and
  a retry could inherit a green tree, do nothing at all, pass `Rung::Structural`
  — whose whole job is refusing an unchanged tree — and be promoted to
  `Accomplished` having done no work.
- 🚨 **A brief may only say what `Opened::continues` supports.** What refused the
  previous attempt is in the next one's brief (F700), and it is read from the
  field that says whose work is under this tree — never from the task's last
  attempt. The two differ for a replay and for a task the operator has taken
  over, and in both cases the easy answer describes a tree the model is not
  looking at. That is the failure F702 *was*; do not re-introduce a second
  source for it.
- 🚨 **What the model was shown is on the log, and it is the brief and not the
  whole context** (F708). `Driver::phase` is the only caller of `TurnLoop::run`
  in the workspace; it takes the brief as text, builds the `Body` itself, scrubs
  once, and writes `Event::BriefRecorded` beside the `AttemptPhaseEntered`. So
  *was the model told X* is `json_extract(body, '$.text')` over
  `kind = 'brief_recorded'` rather than a code path somebody re-reads. Before it,
  thirty event kinds carried no prompt body at all and every prompt-surface arm
  this project has flown asserted its own prompt. ⚠ **Do not build a `Body`
  anywhere else**: the record is honest because the text logged and the text sent
  are one expression, and a second construction site is a prompt with no witness.
  ⚠ The head is a compile-time constant and is on the log as `head_digest`, a
  digest rather than the text. ⚠ **Tool output still is not** — a tool's text
  reaches the model's context and `ToolCallEnded` keeps only its exit and its
  arguments-when-refused, so `apply_patch`'s F649 sentence is a prompt surface
  the log still cannot read back.
- ⚠ **`AwaitingOrders` has no edge back to a slot, and that is the table
  telling the truth** (F703, answered by F732). It used to declare
  `Command::OrdersGiven`; nothing in the workspace sent it, its only caller was
  a lifecycle test, and **no transition on the archive ever took it** — so it
  was removed rather than left as a route a reader could plan around. A refused
  task returns to the board through `abcc take` + `abcc release`, or not at all.
  **The transition table is not evidence that a transition happens**: of its 12
  arms, 333 real transitions have taken 7, and `Hold` and `Resume` have senders
  (F646) that have never once fired on a log. ⏸ `(AwaitingOrders, Hold)` is
  the other arm with no sender and is deliberately left standing (F733).
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
- 🚨 **The weights are the one ungated input, and the run says which bytes
  answered it** (ADR-0014 §6, F688). Measured 2026-09-11: **neither `/v1/models`
  nor `/api/v0/models` carries a digest, a length or a path**, so the digest is
  of the file, and a full SHA-256 of the champion's 12.67 GiB is **54 s**. That
  is why `WeightsOutcome` has two passing arms and they are not interchangeable
  — `Verified` read the bytes, `Unchanged` read the directory entry — which is
  F495's *loaded versus listed* one asset over. ⚠ **A mismatch does not re-pin.**
  Overwriting the pin there would make the alarm fire exactly once and then
  describe the substitute as the reference; `abcc weights --repin` is the
  operator's deliberate act. It reports and does not refuse: ADR-0014 asks for a
  run-visible event, and `confirm`'s veto has evidence behind it that this does
  not.
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
- 🚨 **`abcc take` is narrower than `Command::Commandeer`, on purpose.** The
  transition is legal from every non-terminal state; the verb refuses while the
  task holds a slot, because *the transition moves a state and the verb moves a
  directory* — and that directory belongs to a driver still writing in it. The
  test is `StateContract::holds_slot` rather than a list of state names. Its
  order is also the inverse of the driver's opening (log first, worktree second),
  because only `abcc run` calls `Store::boot`: nothing sweeps up after an
  operator command, so the recoverable failure is the one that leaves the task
  moved and the directory missing.
- **A task may not go terminal still holding a workspace.** All three terminal
  states say `holds_workspace: false`, and until `abcc take` nothing could break
  that claim — the driver closes its own worktree before it sends the landing
  command. `accept`, `reject` and `release` all go through
  `takeover::hand_back`, which snapshots the tree before it takes it down.
- **There is one recipe for a checkpoint and it is `abcc_drive::snapshot`.** It
  is a free function rather than a `Driver` method because the operator's verbs
  are a second caller with their own `Store` and no `Driver`. The ref name is
  what stops `git gc --prune=now` collecting the snapshot (F330), so two places
  that name refs would be work quietly lost rather than a message somebody reads.
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

✅ **`max_tier` is shipped** (`abcc-engine/src/head.rs`), and so is the rest of
what this section used to describe as intended: the effective ceiling is the
narrower of the role's own and the slot's, and the exec class is *derived* from
`ToolSpec::reach` so it cannot drift from a second list. `SECURITY.md` is the
whole posture, row by row, including the four rows that are **admissions rather
than controls** — the OS beneath it, the model's context, a displaced task, and
the half of egress that `bash` leaves open. ⚠ Read those before describing this
project as sandboxed to anyone, including in a commit message.

If you believe you need a capability you do not have, say so and stop. Do not
route around it with a different tool.
