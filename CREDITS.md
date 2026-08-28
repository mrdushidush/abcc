# Credits

## The people who contributed to ABCC v1

Four people outside the project sent work to
[`agent-battle-command-center`](https://github.com/mrdushidush/agent-battle-command-center)
and had it merged. For most of that repository's life the README thanked five
upstream projects and zero humans, and closed with *"Built with ❤️ by the ABCC
community"*. That was an omission, and naming them here is the correction.

| | What they contributed to v1 |
|---|---|
| **Gonçalo Alves** | The keyboard-shortcut layer, the voice-pack plumbing and pack selection, the top bar, the task cards and queue, the shortcuts-help panel, and the UI skeleton components |
| **Mohit Hingorani** | `forceRecoverAll()` on the stuck-task recovery service, and work on the active-missions view |
| **Ayush Joshi** | The task queue component |
| **Karel Švancar** | The top bar and task card |

**None of their code is in this repository, and that is not a slight.** ABCC 2.0
is a clean-room rewrite in Rust with a terminal console: v1's React front end is
not carried across at all, and the three files that *were* on the carry path were
reimplemented from their behaviour rather than translated — a decision taken on
2026-08-27 so that no copyright question arises, before anyone measured how small
the affected surface was.

It turned out to be very small. In the voice-pack file, every one of the 95 voice
lines is David's own; the contributed lines are the interface declaration, four
signatures, and the event keys repeated across three packs. In the audio manager,
the queueing behaviour — the part that is not obvious — is David's; the
contributed lines are two accessors and two wrappers. In the recovery service, the
contribution is one method.

**So this is an acknowledgement, not a licence claim.** Credit is given here
because it is deserved, not because it is owed.

## Assets

The 96 voice lines, the sprite art and the identity are David's own work,
generated or drawn for v1 and carried into 2.0 unchanged. The voice packs are
`tactical`, `mission-control` and `field-command`; no franchise naming from the
original pull request survives, and none is used.

## Upstream

ABCC 2.0 depends on the Rust ecosystem, and on llama.cpp and LM Studio for local
model serving. Its architecture was informed by reading three predecessor
codebases — ABCC v1, Claudette and battle-command-forge — as specification and as
a source of test cases. Where a design here is better than theirs, it is usually
because one of them shipped the defect first and left it where it could be
measured.
