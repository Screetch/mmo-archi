---
name: testing
description: Choose the proportional checks for a change of mmo-archi, read a failure, and send a proof that needs a run of the game, a real load or another system to the place that owns it.
license: Adapted from the skill openclaw-testing of openclaw at 9a75b9b, under the terms in LICENSE.txt
---

# Testing mmo-archi

Prove the changed contract with the smallest check that means something, run the gates the change needs, and stop.
Run more, or run again, only for a new change, a failure, or a risk that stays open. Do not add a test that only
mirrors a change of the implementation that can be undone at low cost; use the `test-audit` skill when you write or
review a test.

`AGENTS.md` gives every command, and says what each one runs. Read it first. `.github/workflows/gates.yml` says what
CI runs, and on which systems.

## Select the proof

Each gate runs locally. Run each build through `sh script/build-slot.sh`, as `AGENTS.md` says under Commands.

| Change | Start with |
| --- | --- |
| A defect | Reproduce it narrowly; run that proof again after the repair, and the tests beside it |
| A crate of `crates/` | `cargo test -p <crate>` as a quick probe, then `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo coverage`, which must stay at 100 percent |
| The lints or the format, `Cargo.toml` or `rustfmt.toml` | `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` on the whole workspace |
| A message between a client and the servers | The tests of each crate that sends or reads it, since a change of a door is a breaking change, as `.github/pull_request_template.md` says |
| The shared crate of the collisions | Its tests, with a test that the same state and the same inputs give the same result, since the server and the client must agree |
| The workflow, a hook or a script of `script/` | `git diff --check` and `sh script/hooks/pre-commit run`, then read the run of the workflow on the pull request |
| The title of a pull request | `python3 script/title.py "<title>"` |
| The documents alone | Their links and their format, and `git diff --check`; no test of the code |

## Boundaries of source and state

Never run a test against a real Redis, a real PostgreSQL or a server that players use. A test that needs a store
gives it a store of its own, which the test makes and drops, and never the data of the owner or of a deployment.

A run of the game, a benchmark and a measure of the load are no gate. Run one only when the owner asks, or when the
change touches what it measures and no test can prove it, and give its result in the pull request, as the template
says.

The gates run clippy and the tests on Linux, on macOS and on Windows. A proof on one system does not prove the
others. When CI fails on a system you do not have, read its log before you change anything.

## CI failures

```bash
gh run list --branch <branch> --limit 10 --json databaseId,headSha,status,conclusion,url
gh run view <run-id> --json status,conclusion,headSha,url,jobs
gh run view <run-id> --job <failed-job-id> --log
```

Bind the diagnosis to the exact sha and job. A run that was cancelled may be one that a newer run of the same branch
replaced. Fetch the failed log once and use it again. Tell a failure of the product from a failure of the harness,
of the runner, or of a cache before you run a job again. A branch that is behind `main` fails for reasons that come
before its own change: bring it up to `main` before you verify it.

Fix the failures that belong to the change, and run the proof they touch again. Report a failure that does not
belong to the change with its evidence, rather than make the task wider.
