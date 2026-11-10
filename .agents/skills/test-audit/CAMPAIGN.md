# Test-pruning campaign

A campaign prunes the whole test surface of one part in one pull request: one crate, or the crates that share one
boundary, as the server and the client around the shared crate of the collisions. The value bar, the retention bar,
the candidate evidence and the validation of [SKILL.md](SKILL.md) hold in each lane. This file gives the order of the
work. Each step ends on its criterion; do not start the next step early.

A test that alone holds a line of `cargo coverage` is no candidate until its keeper holds that line.

## 1. Baseline

At a pinned sha of `main`, record the lines of the tests and of their support in the part, and whether each test
file passes. Keep the failures of the base in a list of their own: a failure of the base is a bug of the product
until you prove it is not.

Done when each test file in scope has a recorded result.

## 2. Lanes and inventory

Split the surface into lanes along the owners of the product, not along the names of files. For the servers, the
lanes follow the shape of the game that `AGENTS.md` fixes: the writer of a zone, the gateway, the stores, and the
shared crate of the collisions. Include the tests of the part at shared boundaries, such as the tests of a message in
each crate that sends or reads it.

Done when each test file of the part belongs to exactly one lane.

## 3. A read-only ledger for each lane

Give each lane to one read-only agent. The agent reads each test of the lane whole, with its tables of cases, and the
code that owns its behavior: the entry points, the callers, the history, and the gate of CI that runs it. It writes
each test into a ledger with one mark:

- `R`: keep it, and name the contract and the bug it catches; a test that only moves to a better file stays `R`;
- `F`: keep the contract, and repair the assertion, such as a negative that passes when one of several items is
  missing;
- `C`: consolidate it, and name the owner that takes its assertion first: a case beside it, a suite at a stronger
  boundary, or a shared helper;
- `D`: delete it, and name the proof that remains, or why no contract exists.

Judge a test by its assertions, not by its name.

Done when each test of the lane has a mark and a line of evidence.

## 4. A plan of layers for each lane

The ledger is the input, not the list of edits. A second read-only pass starts from the ledger and looks for the
layer that repeats another, such as a test of the client that plays again a rule that the shared crate of the
collisions proves. Name the keeper suite of each contract. Prefer the real boundary with a double of the outside,
such as a store in memory, to a double of a collaborator. Correct the errors of the ledger that this pass finds.

Done when each plan names the files it retires, the keeper of each contract, the assertions to carry into the
keepers, and the seams of the product it lets go.

## 5. Cutover

Edit lane by lane. One owner changes a helper that several tests share, one change at a time. With each lane, remove
the seams of the product it lets go: the parameters that only tests inject, the getters, the exports, and the layers
of indirection. Keep `cargo coverage` at 100 percent.

Done when each plan is applied and the keepers of each lane pass.

## 6. Preservation review

Before you claim the work is complete, have independent reviewers compare the coverage you deleted with the keepers,
one reviewer for each group of boundaries. They look for a contract that lost its only proof, and for a new
assertion that cannot fail, such as a refusal that the product never reaches.

For each contract you restore, make one deliberate mutation of the owner in the product, and see the keeper fail.
Then restore the source byte for byte, and check the restore with `git diff`.

Done when each gap is restored or refused with evidence from the source, and each restored contract caught a
mutation.

## 7. Defects of the product

A failure of the base that survives into a keeper is a bug report. Fix it at its owner in a commit of its own, and
prove it through the real flow, with a control run that reverts the fix and shows the old behavior. Record a defect
that does not belong to the campaign as work that is left.

Done when each repaired defect has a failing control and a passing candidate on the same harness.

## 8. Reconcile and hand off

A campaign outlives many commits of `main`. Merge `main` into it rather than rebase a long campaign. When `main`
changed a file that the campaign deleted, keep the deletion, and carry the new contract into its keeper. Run the
whole suite of the part again on the merged head.

Hand off with the report of [SKILL.md](SKILL.md), and:

- the lines of the tests and of their support at the base and at the end, and the lines of the product apart;
- the lanes, the layers you retired, and the keepers;
- the gaps the review found, and their mutations;
- the defects of the product, with their controls and their candidates.
