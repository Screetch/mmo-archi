---
name: test-audit
description: "Use whenever you write, change, review, or sweep tests of mmo-archi. The gate for a new test, and the audit of tests that restate the source, repeat a stronger proof, tie a behavior to its implementation, or keep a seam alive that only a test uses."
license: Adapted from the skill test-audit of openclaw at 9a75b9b, under the terms in LICENSE.txt
---

# Test audit

Three modes hold one bar of value. The authoring mode gates each new or changed test when you write it. The audit
mode sweeps one part for tests of low value. The campaign mode prunes the whole test surface of one part in one
pull request; read [CAMPAIGN.md](CAMPAIGN.md) before you start one. The aim is confidence, not a count of deleted
tests.

## The tests of mmo-archi

`AGENTS.md` gives the laws of the tests, in its section The tests. Read it first.

- The tests of a module of Rust stand beside it, in its `mod tests`, as in `crates/mmo-protocol/src/codec.rs`.
- `cargo coverage` runs every test of the workspace and fails under 100 percent, so a test that you delete can leave
  a line with no measure. Close that gap with a test at the boundary that owns the behavior, or remove the code that
  no caller needs, and never lower the bar.

Each contract has one primary test at its strongest boundary. A public item of a crate is proved by the tests of that
crate. A test of a crate that uses it proves what that crate adds. A message between a client and the servers is
proved once, at the crate that defines it, and a test of a crate that sends or reads it proves what that crate does
with it. The shared crate of the collisions proves that it is deterministic, and the server and the client do not
prove it again.

## Authoring gate

Before you add a test, answer these four questions. When an answer is missing, do not add the test.

1. What observable behavior, law, or contract does it protect?
2. What regression that can occur makes it fail?
3. Why does the coverage that exists not catch that regression? A second layer needs a risk of its own, such as a
   message that only the wire carries, or a process that only a binary starts. Extend a case or a helper of the
   `mod tests` beside the code before you write a near copy of a test.
4. Does it need a seam in the product, such as an export, a flag, a wrapper or a hook, that no product caller needs?
   If it does, move the test to the real boundary.

Then check the test against each [junk pattern](#junk-patterns). A match fails the gate unless the
[retention bar](#retention-bar) names the contract that the test alone guards. A test that breaks when a refactor
keeps the behavior asserts the implementation. Write it again at the boundary that owns the behavior.

A regression test must fail on the code before the fix, for the reason it names, and pass after the fix. A
regression test that never failed proves its double, not the fix. One regression at the boundary that owns the bug
covers it; do not play the same scene again at each layer it crosses.

## Junk patterns

The authoring gate refuses a new test that matches one of these, and an audit looks for the tests that match.

- a test with no assertion, which only runs code;
- a value compared with itself, or a copy of what the test gave;
- a copy of a fixture, a list of names, or a list of exports;
- a grep of the exact source, an import, or a string;
- a test of a private predicate or of the shape of a call, which a real boundary proves again;
- a second call of the same contract;
- a test whose only purpose is to keep an export, a global or a wrapper that only tests use;
- product code that only tests call;
- an expected value that the helper or the renderer under test made;
- a double that holds the behavior that the test asserts, or one double that stands in for two different APIs;
- a fixture that gives the order or the answer that the owner must make itself;
- a negative control that passes for another reason, such as a refusal of a different guard;
- a name or a docstring that promises more than the input tries.

These are particular to mmo-archi:

- a test of a crate that reads a private item, when a public item reaches the same behavior;
- an assertion that is weaker than the behavior it names, such as `is_ok()` or a check of the variant of an enum
  where the code gives an exact value;
- a test of a crate that proves again a behavior that the tests of the crate that owns it prove;
- a test that only runs code to keep `cargo coverage` at 100 percent, with no assertion of a behavior.

## Value bar

A test earns its cost when it protects a behavior, a regression that can occur, or a contract that means something
alone. In an audit, a test that must change when a refactor keeps the behavior is a suspect, not a sure deletion.
The authoring gate still refuses a new one.

Before you judge a candidate, read the whole test and the code that owns its behavior: the entry point, the
callers, the callees, the tests that overlap it, the gate of CI that runs it, and its history. When the test
claims a behavior of a dependency, such as parry3d, wgpu or a client of Redis or PostgreSQL, read that dependency.

## Discovery

Keep discovery read-only, and report the evidence before you edit. For a wide scope, split the work into lanes:

- one lane for each crate of `crates/`;
- one sweep across all of them for one pattern.

Outside a campaign, prefer a few candidates you are sure of to a long list of guesses.

## Retention bar

Keep a test when it alone enforces a contract: a public item of a crate, a message between a client and the servers,
the determinism of the shared crate of the collisions, the authority of the server over a state, a default, or a line
that `cargo coverage` measures. Also keep:

- a test of an order of calls, when the order is a behavior someone sees;
- a regression test whose failure can occur;
- a check of the source, when it is the cheapest guard that fails when the contract changes and that survives a
  rename.

A kept test that fails on the base is a possible bug of the product: reproduce it, and repair the owner.

A test that is slow or that reads the source is not deleted for that. A test that looks like the implementation may
still be the only guard of a contract; prove that it is not before you remove it.

## Candidate evidence

Record each field before you edit. A candidate with a missing field is not ready to delete.

- the name and the place of the test;
- the failure it can detect;
- the callers of the product seam it covers, other than tests;
- the stronger proof that remains at the boundary that owns the behavior, or why no proof is needed;
- the history, and why the test or the seam exists;
- the product code or the test support that its deletion lets go;
- the risk, and the command that proves the change.

## Edit shape

Take one batch that one owner holds. Delete the exports, the globals, the wrappers and the dead product paths that
only tests used, and keep no alias. Move a kept regression to the owner it belongs to. Put repeated assertions of one
contract into one test.

Prefer a change that removes lines of the product. Do not add a test that says the implementation again, and do not
turn a candidate you are not sure of into a cleanup to raise a count.

## Validation

The commands are in `AGENTS.md`, and the `testing` skill says which one proves which change.

1. Run the tests of the owner and of its siblings: `cargo nextest run -p <crate> <filter>`.
2. When you remove a check of the source, run the command that owns the real contract.
3. Format with `cargo fmt --all`, then run `git diff --check`.
4. Run the gates that the change needs, and `cargo coverage` for any change of a test.
5. Read `git diff --numstat`, and count the product apart from the tests and their support.

## Landing

Commit and push only when the owner asks. Land one coherent pull request at a time. After it lands, start again from
the current `main`, and run the read-only discovery again for the next batch.

## Handoff

Report:

- the categories of low value you removed, and why they stood;
- what the change made simpler in the product;
- the candidates you kept, and why they have value;
- the proof you ran, the focused one and the whole one;
- the lines of the product and the lines of the tests, apart;
- the state of the pull request;
- the work that is left, by name.
