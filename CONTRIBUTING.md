# Contributing

Thank you for looking. ABCC 2.0 is a one-person project, and the scarce resource
in it is review time. That is not a figure of speech: it is the thing the project
measures — human review minutes per merged change (W13).

**Issues are welcome.** A bug, a run that went wrong (the output of
`abcc replay <task>` helps), or a claim in the README, `SECURITY.md` or a source
comment that the code does not bear out — the last kind especially.

**Pull requests are by invitation.** Open an issue first and say what you want to
change; if it is wanted, the issue will say so. An unsolicited pull request may be
closed without review — not because it is unwelcome, but because reviewing it
costs the minutes the project is trying to save.

If you are invited to send one, CI must pass, and it runs the same three checks
you can run locally, on Linux and on Windows:

```
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as `MIT OR Apache-2.0`, without any additional terms or conditions.

Security problems: see [`SECURITY.md`](SECURITY.md#reporting-something).
