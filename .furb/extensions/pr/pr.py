import json
import re

from furb.engine import (
  HIDDEN,
  Exit,
  Refused,
  bash,
  read,
  span,
)

PULLING = """The rules of a pull request live in one home, the template .github/pull_request_template.md of its worktree, and pr.pyi says what pr, prcheck and checked do.
Await prcheck(title, why, outcome, breaking, where=...) before you commit, await pr(title, why, outcome, checks, where=...) to open or update the PR, and await checked(number) for its checks, never a loop of your own.
Rerun a check only when checked gives it [machine] or [cancelled], once for each head, with gh run rerun <run> --failed, and then await checked(number) again."""

PR_SENTENCE = 25


def entered(where: str) -> str:
  return f"cd {shq(where)} || exit 1; " if where else ""


async def prcheck(title: str, why: str, outcome: list[str], breaking: list[str] | None = None, where: str = "") -> int:
  said = await bash(f"{entered(where)}python3 script/title.py {shq(title)}", show=HIDDEN, show_err=HIDDEN, ok=True, timeout=60)
  if said.code != 0:
    raise Refused(" ".join(said.stderr.content.split()) or f"script/title.py exited {said.code}")
  if not why.strip() or not outcome:
    raise Refused("a PR needs a why and at least one outcome")
  if breaking and ")!:" not in title:
    raise Refused("a PR with a breaking change carries a ! after the area of its title")
  parts = [len(why.split()), len(" ".join(outcome).split())]
  if sum(parts) > 300:
    raise Refused(f"the body holds {sum(parts)} words, over 300 (why {parts[0]}, outcome {parts[1]}): delete a whole paragraph, not a word")
  long = [
    f"{len(s.split())} words: {' '.join(s.split()[:8])} ..."
    for part in [why, *outcome, *(breaking or [])]
    for s in re.split(r"(?<=[.!?])\s+", part.strip())
    if len(s.split()) > PR_SENTENCE
  ]
  if long:
    raise Refused(f"{len(long)} sentences over {PR_SENTENCE} words, split each one:\n" + "\n".join(long))
  bare = [" ".join(x.split())[:60] for x in [*why.split("\n\n"), *outcome] if x.strip() and not re.search(r"[.!?][)`']*\s*$", x)]
  if bare:
    raise Refused(f"{len(bare)} lines hold no sentence, as a command does: end each with a full stop: " + "; ".join(bare))
  return sum(parts)


def pr_filled(template: str, why: str, outcome: list[str], breaking: list[str], checks: list[str], head: str) -> str:
  text = re.sub(r"\n*<!--.*?-->", "", template, flags=re.S).strip()
  boxes = re.findall(r"(?m)^- \[ \] (.*)$", text)
  if len(checks) != len(boxes) or not all(c.strip() and c.strip().lower() != "not:" for c in checks):
    raise Refused(f"checks needs {len(boxes)} lines, one for each box of the checklist of the template in its order: the evidence of how you verified that box, or not: and the work that it still needs")
  for box, check in zip(boxes, checks):
    said = check.strip()
    ticked = f"- [ ] {box}\n  Not yet: {said[4:].strip()}" if said.lower().startswith("not:") else f"- [x] {box}"
    text = text.replace(f"- [ ] {box}", ticked, 1)
  fills = {"Why": why.strip(), "Outcome": "\n".join(f"- {x.strip()}" for x in outcome), "Breaking changes": "\n".join(f"- {x.strip()}" for x in breaking)}
  for name, body in fills.items():
    text = text.replace(f"## {name}\n", f"## {name}\n\n{body}\n", 1) if body else text.replace(f"## {name}\n\n", "", 1)
  return f"{head}\n\n{text}\n"


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
  await prcheck(title, why, outcome, breaking, where)
  head = "🤖" + (f" Fixes #{fixes}." if fixes else "") + (f" Part of #{part_of}." if part_of else "")
  body = pr_filled(read(f"{where}/.github/pull_request_template.md" if where else ".github/pull_request_template.md", HIDDEN).content, why, outcome, breaking or [], checks, head)
  names = (list(labels) if isinstance(labels, list) else re.findall(r"--label '?([^'\s]+)'?", labels)) + ["breaking-change"] * bool(breaking)
  flags = "".join(f" --label {shq(x)}" for x in names)
  made = "".join(f"gh label create {shq(x)} --force >/dev/null 2>&1; " for x in names)
  name = shq(title)
  if number:
    command = f"gh pr edit {number} --title {name} --body-file -{flags.replace('--label', '--add-label')} >/dev/null && gh pr view {number} --json url --jq .url"
  else:
    opening = PR_OPEN.replace("@NAME@", name).replace("@BASE@", shq(base))
    command = opening.replace("@ADDED@", flags.replace("--label", "--add-label")).replace("@FLAGS@", flags)
  got = await bash(entered(where) + made + command, stdin=body, show=span(-2, -1), timeout=180, ok=True)
  if got.code != 0:
    said = " ".join((got.stderr.content + " " + got.stdout.content).split())[-500:]
    raise Refused(f"gh did not open or edit the PR, exit {got.code}: {said}")
  return got

PR_OPEN = (
  "{ B=$(git branch --show-current); N=$(gh pr list --state open --head \"$B\" --json number --jq '.[0].number // empty');"
  " out=$(if [ -n \"$N\" ]; then gh pr edit \"$N\" --title @NAME@ --body-file -@ADDED@ >/dev/null"
  " && gh pr view \"$N\" --json url --jq .url; else gh pr create --base @BASE@ --head \"$B\" --title @NAME@"
  " --body-file -@FLAGS@; fi); c=$?;"
  " if [ $c -ne 0 ]; then u=$(gh pr list --state open --head \"$B\" --json url --jq '.[0].url // empty' 2>/dev/null);"
  " [ -n \"$u\" ] && { echo \"$u\"; exit 0; }; fi; echo \"$out\"; exit $c; }"
)


CHECK_WAIT = r"""end=$(( $(date +%s) + bound ))
until [ "$(gh pr checks "$n" --json bucket --jq 'length > 0 and all(.[]; .bucket != "pending")' 2>/dev/null)" = true ] &&
  h=$(gh pr view "$n" --json headRefOid --jq .headRefOid 2>/dev/null) &&
  [ "$(gh run list --commit "$h" --limit 100 --json status --jq 'all(.[]; .status == "completed")' 2>/dev/null)" = true ] ||
  [ "$(date +%s)" -ge "$end" ]
do echo "waits $(date +%s)" >&2; sleep "$every"; done
"""

CHECK_POLL = r"""for try in 1 2 3; do
  rows=$(gh pr checks "$n" --json name,bucket,link 2>/dev/null)
  case "$rows" in "["*) break ;; esac
  sleep 2
done
head=$(gh pr view "$n" --json headRefOid --jq .headRefOid 2>/dev/null)
echo "$head"
echo "$rows"
[ -n "$head" ] && gh run list --commit "$head" --json status,workflowName --limit 100 2>/dev/null
echo
"""

CHECK_LOG = r"""ok=
for try in 1 2 3; do
  if log=$(gh api "repos/{owner}/{repo}/actions/jobs/$id/logs" 2>&1); then ok=1; break; fi
  sleep 2
done
if [ -z "$ok" ]; then echo unread; printf '%s\n' "$log" | head -3 | cut -c1-300; exit 0; fi
if printf '%s\n' "$log" | grep -qE 'runner has received a shutdown signal|lost communication with the server|No space left on device|ECONNRESET|ETIMEDOUT|Could not resolve host|TLS handshake timeout|Connection reset by peer|runner is not responding|The operation was canceled'; then echo machine; else echo defect; fi
found=$(printf '%s\n' "$log" | grep -E 'FAILED|panicked|Error|error\[|AssertionError|\(fail\)|timed out|##\[error\]' | grep -v 'Process completed with exit code' | head -12 | cut -c1-300)
[ -n "$found" ] || found=$(printf '%s\n' "$log" | tail -45 | cut -c1-300)
printf '%s\n' "$found"
"""


async def checked(number: int, where: str = "", bound: float = 3600.0, every: float = 60.0) -> tuple[str, list[str]]:
  wait = f"n={int(number)} bound={int(bound)} every={every:g}\n{CHECK_WAIT}{CHECK_POLL}"
  got = await bash(entered(where) + wait, show=HIDDEN, show_err=HIDDEN, timeout=bound + 180)
  head, _, text = got.stdout.content.partition("\n")
  found = []
  for line in [*text.splitlines(), "", ""][:2]:
    try:
      found.append(json.loads(line))
    except ValueError:
      found.append(None)
  rows, runs = (x if isinstance(x, list) else None for x in found)
  pending = [str(one["name"]) for one in rows or [] if one["bucket"] == "pending"]
  unfinished = [str(one["workflowName"]) for one in runs or [] if one["status"] != "completed"]
  if not rows or runs is None or pending or unfinished:
    seen = ", ".join(pending) or ("the run of " + ", ".join(unfinished) if unfinished else "the runs are unread" if rows else "no check has started")
    return "pending", [f"head {head.strip()[:7]}: still pending at the bound of {int(bound)} s: {seen}"]
  failed = [one for one in rows if one["bucket"] in ("fail", "cancel")]
  lines = [f"head {head.strip()[:7]}: {len(rows)} checks, {len(failed)} failed"]
  for one in failed:
    name, hit = str(one["name"]), re.search(r"/runs/(\d+)/job/(\d+)", str(one["link"]))
    if one["bucket"] == "cancel" or not hit:
      lines.append(f"{name} [{'cancelled' if one['bucket'] == 'cancel' else 'unread'}] {one['link']}")
      continue
    log = await bash(f"{entered(where)}id={hit[2]}\n{CHECK_LOG}", show=HIDDEN, timeout=180)
    kind, *said = log.stdout.content.splitlines() or ["unread"]
    lines.append(f"{name} [{kind}] run {hit[1]} job {hit[2]}")
    lines += [f"  {x}" for x in said]
  return ("fail" if failed else "pass"), lines

def shq(s: str) -> str:
  home = "~" if s == "~" else "~/" if s.startswith("~/") else ""
  return home + ("'" + s[len(home) :].replace("'", "'\\''") + "'" if s != "~" else "")
