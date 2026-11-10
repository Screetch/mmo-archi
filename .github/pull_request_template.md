<!--
This file holds the rules for a pull request of mmo-archi. It is the only place they live.
Read it whole and apply it strictly.

Scope: one change per PR. A PR does one thing. Keep it as small as that one thing allows. When small
and atomic disagree, stay atomic. Do not divide one change into artificial parts. When the work
needs more than one PR, each PR enables the next.

Source: write the title and the body from the work itself. Give the problem you were brought, what
you debugged, and what you decided. Do not summarize the commit messages. An agent writes from its
own session, never from `git log`.

Title: `type(area): What changed`, 70 characters or fewer, as
`fix(zone): Drop the entity of a player that leaves the zone`.
- The type comes first, since release-please reads it to make the next version and CHANGELOG.md:
  `feat` for a thing that a user can do now and could not do before, `fix` for a thing that was
  wrong, `perf` for a thing that is faster or smaller, and `chore` for a change that no user of a
  package sees. Below 1.0, a feat, a fix or a perf raises the patch version, and a break raises the minor.
  A chore raises nothing and stays out of the changelog. Add a `!` after the area for a break.
- The area is required. It is the part that owns the change, such as `zone`, `gateway`, `sim`,
  `client`, `ci`, or an extension by its name, as `conductor`. Give the primary area when
  the change touches more than one.
- What changed is one sentence in Simplified Technical English that starts with a capital letter.
- Do not start with a tool name such as [claude], [codex] or [ai].
- The squash merge keeps the title as the commit on main. The check `the title` fails a title that
  breaks this rule, and `python3 script/title.py "<title>"` runs it on your machine.

Body: keep Why and Outcome. Delete Breaking changes when nothing breaks. Write up to 300 words.
The checklist, Breaking changes, a table and a diagram label do not count. Cut by deleting a whole
paragraph, not by compressing a sentence. A large diff does not need a large body. A section that
grows with the diff repeats the diff. Write it as AGENTS.md says under "Prose".

Do not give the how. The diff and the commit messages hold the mechanism. When the diff is hard to
read, the PR is too large. Divide it.
-->

## Why

<!-- The problem and the motivations. What is wrong today, who has the problem, what it costs, and
why it is worth solving now. Do not start with code.
Name the PR below this one when this PR is one of a chain. -->

## Outcome

<!-- The outcome is what the diff cannot tell. A reader can read every line that moved. They cannot
read what changed for the people the code serves.
Give what is true now that was not true before. Give what it costs a caller: a program that uses
a crate of the workspace, a client that speaks to the servers, a player, or an operator
of the servers. Give any gap you leave in on purpose. Link the sibling PRs, such as
one in another repository, when this change has any.
Give the evidence under each claim you make here: what you ran, and what it showed. Nothing that
did not run goes in. CI reports itself, so give only what CI cannot: a run of the game, a
capture of a screen, a benchmark, a measure of the load. When you checked nothing by hand, say that CI
covers it. A reviewer can disagree.
An addition breaks nobody. Give it one line here when a caller would use it, and open no section
for it.
Add a mermaid diagram when the reader has to hold a shape, such as two paths that differ or a
state machine. Put a number in a table. -->

## Breaking changes

<!-- Only when the change forces a downstream caller to change code.
Give each break its own line. Do not merge two, and do not abbreviate one. Add a before snippet
and an after snippet where the call site moves:
- Ground the snippet in the real doors of mmo-archi: the public items of each crate of
  `crates/`, and the messages between a client and the servers.
- Keep a snippet to 3 to 6 lines. Show the call site and nothing else.
- Skip the snippet when the call site does not move.
A PR with this section carries the `breaking-change` label, and its title carries a `!`. -->

---

<!-- Tick each box, and only after you check it. Each one is a claim a reviewer can test.
A box you cannot tick names work to do. -->

- [ ] I am accountable for this diff, whoever or whatever wrote it.
- [ ] It is one change. No part belongs in another PR. No part could be its own PR.
- [ ] Its comments obey AGENTS.md: none, or each one prevents a debugging session.
- [ ] Its prose obeys AGENTS.md: Simplified Technical English, for a general non-technical audience.
- [ ] Every flaw I know of is fixed in this diff or named in `## Outcome`.
- [ ] I checked each statement above. I did not assume it.
