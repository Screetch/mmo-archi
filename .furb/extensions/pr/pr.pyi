from furb.engine import Exit

async def prcheck(title: str, why: str, outcome: list[str], breaking: list[str] | None = None, where: str = "") -> int:
  """prcheck(title, why, outcome) runs script/title.py of the worktree at where on the title, and refuses a title that it refuses, with its words.
  It counts the words of why and outcome, which hold 300 at most, and gives that count.
  It refuses a sentence of more than 25 words, with its length and its start.
  It refuses a line of why or outcome that ends with no full stop, as a command does.
  It refuses a breaking change when the title carries no ! after its area.
  """

async def pr(
  title: str,
  why: str,
  outcome: list[str],
  checks: list[str],
  *,
  breaking: list[str] | None = None,
  fixes: int = 0,
  where: str = "",
  labels: str | list[str] = "",
  number: int = 0,
  base: str = "main",
  part_of: int = 0,
) -> Exit:
  """pr(title, why, outcome, checks) checks them with prcheck, and opens the pull request of the branch of the worktree at where, or edits the one that stands, with the template of that worktree filled as its body.
  The body starts with a line of 🤖, Fixes #fixes and Part of #part_of, fills Why and Outcome, fills Breaking changes or deletes it, and holds no comment of the template.
  It ticks each box of the checklist from checks in order, and a check that starts with not: leaves its box open under a line Not yet: with what is left.
  It refuses checks that do not give one line to each box of the checklist.
  It gives each label one flag after the body, and a breaking change the label breaking-change.
  """

async def checked(number: int, where: str = "", bound: float = 3600.0, every: float = 60.0) -> tuple[str, list[str]]:
  """checked(number) awaits one command that waits for the checks of the PR, and gives a tuple: pass, fail, or pending when the bound ended, and lines whose first names the head.
  It waits until a check is listed, no check pends and no run of the head is open.
  Its command writes one short line each every seconds while it waits, so its chain says a fact.
  At the bound it gives pending with what pends: the checks that pend, the run that is open, or that no check started.
  For each failed check it gives [machine] for a fault of the runner or the network, [defect], [cancelled] or [unread], with its run, its job and its first failed lines.
  """
