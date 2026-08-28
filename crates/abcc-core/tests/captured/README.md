# Captured test-runner endings

Real output and the real exit status from pytest, `python -m unittest` and
`cargo test`, executed in a scratch tree by W6 item 8's spike. Nothing here is
typed by hand and the file names carry the ground truth.

These are **test corpus, not donor code**. ADR-0001 makes the donors a
specification and a test corpus; this is the second half of that, and it is the
reason `outcome.rs`'s classifiers can be checked against what the runners
actually print rather than against what their documentation says they print.

Provenance: `research/spikes/w6-honesty/captured/` in the ABCC 2.0 research
repository, written by `v1_parser.py` and `cargo_endings.py`.
