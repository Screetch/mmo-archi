---
name: expert
description: The skills that each task of mmo-archi runs, and the rules of mmo-archi that win over them. Run it with the skill ponytail before you design or write, and with the skill ponytail-review before you review a design note or a diff.
---

# Work as an expert

Write only code that an expert would write, and that an expert would pass in review. The rules below say what that
means in mmo-archi.

## The skills of each task

- To write: `ponytail` and `codebase-design`, and `rust` for each Rust file.
- To review a design note or a diff: `ponytail-review` and `code-review`.
- For architecture and convergence: `improve-codebase-architecture` and the rules of Ousterhout,
  `a-philosophy-of-software-design`.
- For a refactor: the rules of Fowler, `refactoring`.
- To write a test: `tdd`, below the section The tests of `AGENTS.md` and the skills `testing` and `test-audit`.
- To split a change into a stack of pull requests: `gh-stack`.

The rules below win over each of these skills, and `AGENTS.md` wins where it says more. A review reads a design note
as the diff that it will make, and gives one report in the form of `ponytail-review`: the axes and the smells of
`code-review` give findings in it as each other check does.

## 1. Read the mechanism of mmo-archi first

A new capability extends a mechanism that mmo-archi has. It is never a new mechanism. Read the whole artifact that a
design touches before you derive the design.

This is step 2 of the ladder of `ponytail` in mmo-archi. Before you write code, find the mechanism of mmo-archi that
does the nearest thing: a crate of `crates/`, the shape of the game that `AGENTS.md` fixes (the authority of the
server, the one writer of each layer, the stateless gateway, the stores of Redis and PostgreSQL, and the shared
deterministic crate of the collisions), a script of `script/`, or a skill. Read it, and name it in your design note or
your pull request. When no mechanism fits, say why.

## 2. No shortcut, no deferral

No hack and no workaround. Never defer the right solution because it is harder: build it now. An unfinished path
stands only for a real blocker that you report, and its error says what is missing and why.

So the rule `shortcut:` of `ponytail` does not apply. Write no `shortcut:` comment. In a review or an audit, a
`shortcut:` or `ponytail:` comment is a finding of **Must fix**, not a decision, and the ledger of `ponytail-debt`
stays empty. A finding on a change is fixed in that change, whatever its grade: "Fix in next pass" and "Fix if time
permits" do not hold. The refactor that a change needs is part of that change, also in the loop of `tdd`.

## 3. Converge

Do one thing in one way. Two concepts that serve one purpose become one: merge them, and keep no second thing beside
the first. Repair, move or delete the implementation that stands; never build a parallel one to avoid fixing it.

A change that adds a second way to do a thing deletes the first way in the same change, or the review refuses it. Get
the explicit consent of the owner of the repository before a change removes a public capability, as a public item of
a crate or a message between a client and the servers, adds a cost that crosses each part, or adds spend.

## 4. The size of a diff

In a healthy code base the parts fit together and compose in one consistent way, so little code expresses a lot. A
pattern that repeats becomes one function that each place reuses. Meet a limit of size by a better representation,
never by deleting a capability, obscuring a name or inlining a concept out of sight.

A review compares the size of a diff with the size of its idea. A diff that is larger than its idea is a finding of
**Must fix**, and its case is the smaller diff that does the same thing. A copy of code that mmo-archi holds already is
a finding of **Must fix**: the fix uses that code and names its path. A smell of `code-review` with a concrete case is
a finding, not a judgement call.

## 5. The texts of others

`ponytail` and its five skills come from [DietrichGebert/ponytail](https://github.com/DietrichGebert/ponytail);
`codebase-design`, `improve-codebase-architecture`, `code-review` and `tdd` from
[mattpocock/skills](https://github.com/mattpocock/skills); `a-philosophy-of-software-design` and `refactoring` from
[ciembor/agent-rules-books](https://github.com/ciembor/agent-rules-books); and `gh-stack` from
[github/gh-stack](https://github.com/github/gh-stack). Each folder holds the texts that its author published at the tag
that its `metadata` names, as `gh skill install` puts them, and the license of its source. It holds only the files
that its task reads. Do not edit these texts: to take a later tag, install them again with `--pin` and keep the same
files. mmo-archi holds the texts alone, so these parts of them do not apply:

- Their scripts, plugins, hooks, commands and configs: run none, but the command `gh stack` that `gh-stack` drives.
  The level of `ponytail` is full.
- The skills and the files that mmo-archi does not hold, as `grilling`, `domain-modeling`,
  `setup-matt-pocock-skills`, `HTML-REPORT.md`, a `GLOSSARY.md` and the folder `docs/adr/`.
- A `GLOSSARY.md`, an ADR or an HTML report: write none. The design note and the pull request hold what they would
  hold.
- A question to the user: decide from the brief and the rules, and name each decision in the pull request. Ask only
  when a product decision that no one made, or a permission that you lack, blocks the correct action.

## 6. Private words

Words of a person in a session are private. Never quote them, translate them or attribute them in code, docs, skills,
commits, PRs or issues: state the rule they carry, in plain English.

## What stays

A simplification keeps each capability and changes only its representation. These rules cut nothing from the list
"Never cut" of `ponytail`: validation at trust boundaries, the handling of errors that prevents data loss, security and
accessibility stay.
