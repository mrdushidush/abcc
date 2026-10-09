# Contributing

Thank you for looking. ABCC 2.0 is a one-person project, and the scarce resource
in it is review time. That is not a figure of speech: it is the thing the project
measures — human review minutes per merged change (W13).

**Issues are welcome.** A bug, a run that went wrong (the output of
`abcc replay <task>` helps), or a claim in the README, `SECURITY.md` or a source
comment that the code does not bear out — the last kind especially.

**Pull requests are by invitation, and the invitation is a label.** An issue
labelled [`pr-welcome`](https://github.com/mrdushidush/abcc/labels/pr-welcome) is
pre-approved: comment "taking this" and send the pull request without asking. A
claim lapses after 14 days without a draft pull request. For anything else, open
an issue first and say what you want to change; if it is wanted, the issue gets
the label.

How pull requests get reviewed:

- **On Fridays, up to three a week, oldest first.**
- **Over 200 changed lines, or outside the issue's scope:** closed with a note,
  not reviewed — not because it is unwelcome, but because reviewing it costs the
  minutes the project is trying to save. Split it, or open an issue.
- **You keep the merge.** If a pull request needs a fix, I ask you for it or push
  it to your branch; I never rewrite your pull request as my own.

CI must pass, and it runs the same three checks you can run locally, on Linux and
on Windows. None of them needs a model or a GPU:

```
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as `MIT OR Apache-2.0`, without any additional terms or conditions.

Security problems: see [`SECURITY.md`](SECURITY.md#reporting-something).
