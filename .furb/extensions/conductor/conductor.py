import json
import re
from collections.abc import Generator
from typing import Any

from furb.engine import (
  HIDDEN,
  ROOT,
  Act,
  Exit,
  Refused,
  Text,
  act,
  acting,
  bash,
  cd,
  chain,
  clock,
  close,
  controlled,
  cwd,
  get,
  operated,
  over,
  paused,
  peek,
  prompt,
  read,
  rung,
  scope,
  transcript,
  wait,
  write,
)

CONDUCTING = """The round conducts the workers of the repository in one loop. It is a way for the work, not the work: a rule that a worker, a tool or GitHub can hold does not live here.
The round is autonomous: the word of the owner comes when he can give it, so the round queues no item that waits for him, asks him nothing and waits for nothing of his. His labels on a PR, his comments and his rulings change what the round does from then on.
Put each item in queue as a dict, in the order of the work, the hard work first: its key, brief and thread, and when it needs them its after (the keys whose PRs merge first), areas (its files), model, and kind (edit, audit or fix, edit by default). Then await conduct(). Each round runs tend(), sweep(), review(), keep_disk(), staff(), asked() and align(), then waits every seconds or until a worker ends. working maps the prompt of each worker to its item, which keeps its slot and, once the worker ends, the PR of its report in pr, or 0.
1. staff() hires the first item of queue whose after all merged, whose model has room, and whose files that must change ALONE no live worker and no open PR of the round holds, while fewer than cap() - 1 workers live, so one slot stays for a fixer: a worker lives while its chain said a fact in the last QUIET seconds, and one that said none is one notice with its open acts; cap() is at most MOST, and limits["room"][family] for each model family.
2. The files of an item are its areas, which you write, and a test file holds no item. An item with no areas holds no file. A conflict on a file that need not change alone gets a fixer, by E3 of #1084. Each item that waits on a PR or a file is one line of the next notice, with its cause, once.
3. A worker runs on a chain thread/key, at model_of(item), and an edit or a fix in a workspace {repo}-wN of its own, never one whose furb-holder names the chain of a live worker, and never one that holds work off a branch: a dirty tree, or a commit that no remote holds while its branch is the head of no merged PR. The round skips such a workspace and tells you once. Each round keep_disk() keeps KEEP GB of the disk free: it deletes the target of the idle workspace whose last worker was hired first, never of one that a live worker holds, and tells you once when it cannot. Its brief starts with the asks of the owner that its item carries. Its brief sends each build of the worker through script/build-slot.sh, which holds the slots of builds that all the workers of the machine share and caps the jobs of cargo: the tool holds that rule, and the round only briefs it.
4. A worker that ends gives its report whole in the next notice. A worker that ends with no PR is a decision, and its notice wakes you.
5. A PR of the round is an open PR whose label w:key names an item that the round holds. A red or conflicting one counts as handled only while a worker that knows of its head acts: its live maker, which the round tells at once, once for each head and problem, to fix it before any other step, else a fixer that the round sent for that head, at most 2 for a head, then a notice. A fixer runs at the model of the fixer on a fork of the chain of the worker that made the PR, when the round holds that worker, so it starts with what that worker read and did. A head that such a worker has not moved in UNMOVED seconds is one notice, with the worker and its last act. handler() holds that rule for the sweep.
6. While the newest run of the gates on main failed, the round sends no order and no fixer for a red PR, and tells you once. A red PR whose failed checks all fail on main too gets no fixer either, and the round says so in one line.
7. The round merges a green PR itself, and GitHub merges none: the round turns off the auto-merge of each PR that it tends, since GitHub would merge a head that stands on an old main. The ruleset of main requires the checks GATE and the title and no up-to-date branch, so the round holds that rule in its own merge. It merges a PR only when its head holds the current tip of its base and the newest run of its gates passed on that head: in one command it reads how far the head is behind its base, and merges with gh pr merge --squash --match-head-commit only at 0, else with gh stack merge for a layer of a stack. A green PR that is behind its base is not merged: the round updates its branch with gh pr update-branch, one PR for each sweep, only while no other PR of the round stands on the current tip with its gates still running, so the gates run again on the head and the next sweep reads them. Each merge changes main, so the PRs that wait are behind it, and each one meets the same rule in its turn. With no ruleset of main that requires the check GATE, the round merges no PR and tells you once. A draft, an open box of its checklist, or the label hold, hold:owner-review or owner-merge keeps a PR from the merge, and the label paused takes it out of the round.
8. At ALIGN_EVERY merged PRs since the last audit of each target of AUDITS, the round queues the next free audit of that target, alignN, of kind audit, on limits["models"]["astra"] when N is odd, else on the model of audit. alignN checks main against the words of the owner. Each row of its register becomes an item, queued in the order of the register after the items of queue, unless the coordinator asks for the rows before the items that no worker holds yet.
9. Each message of the owner that no ask of owner_asks names is one line, and so is each ask that is not done or a decision while no item in queue, at work or on an open PR of the round carries it. asked() gives the new lines in one notice, and no new notice while its last one stands open: no word of the owner drops between his message and main. A worker that ends with a line Left: <ids> queues one follow-up for each ask that no other item carries, and a follow-up that leaves it again is a decision.
10. Each new comment of the owner on a PR of the round goes to the worker on it, else to a fixer on a fork of the chain of its maker, and the label hold comes on the PR until its worker pushes the fix and takes hold off.
11. A fix of the round does not become its own item or PR: it waits in scratch://round/frozen.md, while the live module carries it, and the waiting fixes land together in one change.
A step that fails is one notice, and the round goes on at the next round. The whole state lives in the module of your chain, so a word that a checkpoint cuts loses nothing: await conduct() again in a turn that shows.
The round is an act on a chain of its own, so the end of each of its acts wakes nobody: you hear only its notices, and its done when it holds nothing. lead["round"] names it. close(None, lead["round"]) stops it, and each worker goes on, where a pause of it pauses each worker and a cancel ends them. await conduct() starts it again from any turn: the new round ends the beat of the round before it, which ends at its next step, and starts once that round is over, or after every seconds.
A change of the round loads as the whole file, run again as a rung: each name of the state binds with setdefault and keeps its value."""

MOST = 10
UNMOVED = 1800.0
ALIGN_EVERY = 4
OWNER = "Screetch"
GATE = "the gates"
FAILS = ("FAILURE", "ERROR", "TIMED_OUT", "STARTUP_FAILURE", "ACTION_REQUIRED", "CANCELLED")
GREEN = ("SUCCESS", "SKIPPED", "NEUTRAL")
HOLDS = ("hold", "hold:owner-review", "owner-merge", "paused")
ASKS = "scratch://owner/asks.json"
WORDS = "scratch://owner/words.md"
ASK_LEFT = re.compile(r"Left: ([^\n]*)")
ALONE = re.compile(r"^(Cargo\.toml|Cargo\.lock|AGENTS\.md|\.github/workflows(/.*)?|\.furb/extensions/conductor/conductor\.py)$")
QUIET = 1800.0
KEEP = 60
TESTS = re.compile(r"(^|/)(tests?|__tests__)/|\.test\.[a-z]+$|_test\.[a-z]+$|(^|/)test_[^/]+\.py$|(^|/)conftest\.py$")

# The module of a chain may hold these names already, with values: a name binds its default only when it is not there.
limits: dict[str, Any] = globals().setdefault(
  "limits",
  {
    "room": {"opus/high": 3, "opus/medium": 3, "sonnet/high": 3, "sonnet/medium": 3},
    "fixer": "claude-cli:sonnet/medium",
    "models": {"editor": "claude-cli:sonnet/high", "audit": "claude-cli:opus/high", "lead": "claude-cli:opus/medium"},
  },
)
queue: list[dict] = globals().setdefault("queue", [])
working: dict[str, dict] = globals().setdefault("working", {})
prs: dict[int, dict] = globals().setdefault("prs", {})
merges: dict[int, str] = globals().setdefault("merges", {})
assigned: dict[int, dict] = globals().setdefault("assigned", {})
lead: dict[str, str] = globals().setdefault("lead", {})
noticed: set[str] = globals().setdefault("noticed", set())
heard: list[str] = globals().setdefault("heard", [])
landed: list[int] = globals().setdefault("landed", [])
align_state: dict[str, dict] = globals().setdefault("align_state", {})
comments: set[str] = globals().setdefault("comments", set())
progress: dict[str, tuple[int, float]] = globals().setdefault("progress", {})
updated: set[str] = globals().setdefault("updated", set())


def asks_kept() -> dict[str, dict]:
  try:
    got = json.loads(read(ASKS, HIDDEN).content)
  except (Refused, ValueError):
    return {}
  return {str(x["id"]): x for x in got if isinstance(x, dict) and "id" in x} if isinstance(got, list) else {}


owner_asks: dict[str, dict] = globals().setdefault("owner_asks", asks_kept())

RULES = """Write no word of the owner in a PR, a commit, a comment, an issue or a file, as a quote, a translation or an attribution: say in your own words what the change does. Run no real life of a model. Never open a window on this machine, not even for a test: the owner works on it, and each window takes his mouse and his keyboard. Run a check of the app only on the runners of CI. Start each comment, review, issue and PR body that you write on GitHub with 🤖, since you write from the owner's account and his own words carry no mark; a title keeps its own rule. Never take off the label owner-merge: only the owner takes it off, and that is his approval to merge. The owner sleeps or is away: ask him nothing and wait for nothing of his. Take each decision yourself from the brief and the rulings, say it in your PR body, and go on. A cause that a brief names is a hypothesis: check it in the log before you build on it. Ask the one who gave you this work on its chain only when it alone can decide: await prompt(str, question, on="{home}"). Set each timeout and each wait to about twice the time that the step should take, never a round bound in case, and make a long step print its progress, so that a hang shows at once. Never wait on CI in a bash for more than 10 minutes: not gh pr checks --watch, not gh run watch, not a loop of sleep around gh. To wait for the checks of your PR, await checked(number) and never write a loop of your own. Rerun a check only when its line says [machine] or [cancelled], once for each head, with gh run rerun <run> --failed, and then await checked(number) again; a [defect] is yours to fix. Once your push is done, read the checks once and close with the PR and its checks as they stand, since the round tends them. A build with no cap took this machine down out of memory, so each build goes through sh script/build-slot.sh <command>, from the root of your workspace: cargo, git commit, whose hooks run cargo, and sh script/hooks/pre-commit. It holds one of the slots of builds that all the workers of the machine share, waits for a free one, and caps the jobs of cargo at 4. Give its bash a timeout of 2400 seconds: when it exits 75, no slot freed in 1200 seconds, so run it again. Run one build at a time, give cargo no -j and no CARGO_BUILD_JOBS above 4, and start no build in the background."""

WORKER = """You are a worker on the repository {repo}. Do one item, and open its PR yourself.
1. Your branch is w/{key}. {workspace} Build on origin/main, and stack your branch on an open PR only when your change needs its code, and say why in the body. Make a stack with gh stack, and run skill("gh-stack") in a word of your own before you make or touch one.
2. Change it as the AGENTS.md files of the repository say, and run only the tests that your change touches.
3. Commit with the title of the PR as its message, and push the branch. Then open the PR, or update it when it stands, with pr(), which fills the template of the owner from what you give it and checks each of its rules: await pr(title, why, outcome, checks, breaking=[], fixes=0, where=<your workspace>, labels="{labels}"). Write the title and the body as .github/pull_request_template.md of your workspace says, and read it whole: why is the problem and why it matters now, from your own work, outcome lists what is true now with the evidence of what you ran and each gap that you leave, breaking lists each change that forces a caller to change code, and checks gives one line for each box of its checklist in its order, the evidence of how you verified that box, or not: and the work that it still needs. Await prcheck(title, why, outcome, breaking, where=<your workspace>) before the commit. Never write the body of a PR by hand, and open it ready, never as a draft.
4. Open your first PR within about an hour of work, as soon as one coherent part serves the item: a large item lands as a gh stack of PRs, each layer one change, and you close with the number of the last one.
5. When the item asks for no change, answer it in a comment that starts with 🤖 on its issue or PR. Close with a tuple: the number of your PR, or 0 when the item asked for no change, and your report in 200 words at most: what you measured, what you decided, and what is left.
""" + RULES + """

The item, {key}:
{what}"""

WORKSPACE = """Work in the workspace {path}, which the round gives to you alone, and to a later item once you end, so that each build starts warm. If it does not stand, make it: sh {repo}/script/build-slot.sh {repo}/script/worktree.sh {path} <your branch>, then give cd({path!r}) in a word of your own and write your chain in it: echo {chain} > "$(git -C {path} rev-parse --absolute-git-dir)/furb-holder". If it stands, the round wrote your chain in it: git fetch origin, then git switch -C <your branch> onto origin/<your branch> when that branch is on origin, else onto origin/main. Before your first command of each prompt there, read the file furb-holder in its git dir: when it names another chain than {chain}, the round gave the workspace to another worker, so run nothing in it. Leave the workspace clean when you close, each change committed and pushed."""

AUDIT = """You are an auditor on the repository {repo}. Do one item: read the code, the records and the logs that it names, find each fact that it asks for, and write each in a register, scratch://audit/{key}.md ($TMPDIR/audit/{key}.md), with its evidence: the file and the line, or the command and the lines that it printed. Check each claim of the item in the code, since it is a claim. Change no file of the repository, make no worktree but the workspace that your item names, push nothing and open no PR. When your work yields items for workers, write them in that register as a table with one row for each item, in order of rank, the hard work first: a row that reshapes a module, a contract or a protocol ranks before a row of polish or a simple row: | Dn | key | model | area | after | brief |, where Dn is its rank, key a short name in letters and dashes, model one of {models}, area the folder or the file that the item changes, after the Dn of each row that it waits for, or nothing, and brief what the worker does, as a task. The round queues each row as an item when you end. Close with a tuple: 0, and your report in 200 words at most: the path of the register, what you found, and what is left.
""" + RULES + """

The item, {key}:
{what}"""

FIX = """You fix the open PR #{n} of the repository {repo}: it {problem} at {sha}. Read it with gh pr view {n} and await checked({n}). If the head of the PR is not {sha} any more, or the PR no longer {problem}, change nothing and close with its number and a report that says so. Else work at the head of its branch {head}. {workspace} Resolve a conflict with the meaning of both sides, run the checks that the change touches, and push with --force-with-lease. Close with a tuple: the number of the PR, and your report in 200 words at most.
""" + RULES

FIXOWN = """Your open PR #{n} {problem} at {sha}. Fix it before any other step: fetch origin, fix it on your branch, resolve each conflict with the meaning of both sides, run the checks that the change touches, push with --force-with-lease, and read its checks once. When its head has not moved in {minutes} minutes, the round tells the coordinator."""

ANSWER = """The owner commented on the open PR #{n} of the repository {repo}, at the head of its branch {head}, and no worker is on it. Do what the comment asks, answer it on GitHub with a reply that starts with 🤖, push the fix with --force-with-lease, and take the label hold off the PR. {workspace} The comment:
{line}
Close with a tuple: the number of the PR, and your report in 200 words at most.
""" + RULES

TRUTH = "The source of truth for this item is the words of the owner, scratch://owner/words.md, and the options that he picked, scratch://owner/picks.md: where a line of this brief bends them, his words and his picks rule.\n\n"

ALIGN = """The audit of alignment {key}: {count} PRs merged since the last one. Its window of commits is git log --oneline {window}. Read that window and the diff of each PR in it, and check main against the words of the owner, which are the one source of truth: {words}, scratch://owner/picks.md, and the ledger of his asks, {asks} ($TMPDIR/owner/asks.json). Check each ask of the ledger against origin/main as a claim: set its state to done, open, partly or missing, with its evidence and its gap, and write the ledger back in the same shape. The round holds the items below, so a row of your register queues none of them again: {held}."""

AUDITS = {"align": ALIGN}

SETTLE = """here=$1 mine=$2 heads=" $3 "
shift 3
cd "$here" 2>/dev/null || exit 0
holder=$(cat "$(git rev-parse --absolute-git-dir)/furb-holder" 2>/dev/null)
for live in "$@"; do [ "$holder" != "$live" ] || exit 3; done
if [ "$holder" != "$mine" ]; then
  [ -z "$(git status --porcelain)" ] || exit 5
  b=$(git branch --show-current)
  case "$heads" in *" ${b:-?} "*) ;; *) [ -z "$(git rev-list HEAD --not --remotes | head -1)" ] || exit 5 ;; esac
fi
echo "$mine" > "$(git rev-parse --absolute-git-dir)/furb-holder"
"""

DISK = """need=$1 live=" $2 "
shift 2
left() { df -Pk "$HOME" | awk 'NR == 2 {print int($4 / 1048576)}'; }
for p in "$@"; do
  [ "$(left)" -lt "$need" ] || exit 0
  h=$(cat "$(git -C "$p" rev-parse --absolute-git-dir 2>/dev/null)/furb-holder" 2>/dev/null)
  case "$live" in *" $h "*) [ -z "$h" ] || continue ;; esac
  [ ! -d "$p/target" ] || { rm -rf "$p/target"; echo "$p"; }
done
[ "$(left)" -ge "$need" ] || { left; exit 4; }
"""

LABELS = "gh label create hold -c FBCA04 -d 'It waits for the fix of a review before a merge' 2>/dev/null; gh label create hold:owner-review -c FBCA04 -d 'The owner iterates with its worker: no merge until he takes it off' 2>/dev/null; gh label create owner-merge -c 5319E7 -d 'The owner merges it or not' 2>/dev/null; gh label create paused -c BFBFBF -d 'The owner paused it: no fix and no merge' 2>/dev/null; true"

PRS = """gh pr list --state open --limit 200 --json number,isDraft,labels,mergeable,baseRefName,headRefName,headRefOid,statusCheckRollup,body,files,autoMergeRequest --jq '[.[] | ([.statusCheckRollup[]?] | group_by(.name // .context // "") | map(max_by(if (.status // "COMPLETED") == "COMPLETED" then (.startedAt // .completedAt // "") else "~" end))) as $c | {n: .number, draft: .isDraft, labels: [.labels[].name], mergeable, base: .baseRefName, head: .headRefName, sha: .headRefOid, checks: [$c[] | (.conclusion // .state // "")], names: [$c[] | (.name // .context // "")], open: [(.body // "") | splits("\\n") | select(startswith("- [ ] "))], files: [.files[]?.path], auto: (.autoMergeRequest != null)}]'
c=$?; echo; echo "--- $c"
gh pr list --state merged --limit 200 --json number,headRefName --jq '[.[] | [.number, .headRefName]]'
c=$?; echo; echo "--- $c"
gh run list --branch main --workflow gates.yml --limit 20 --json headSha,conclusion --jq '.[0].headSha as $s | {sha: ($s // ""), runs: [.[] | select(.headSha == $s) | .conclusion]}'
c=$?; echo; echo "--- $c"
gh api 'repos/{owner}/{repo}/rules/branches/main' --jq '[.[] | select(.type == "required_status_checks") | .parameters.required_status_checks[].context]'
c=$?; echo; echo "--- $c"
gh api 'repos/{owner}/{repo}/commits/main/check-runs?per_page=100' --jq '[.check_runs[] | select(.conclusion == "failure" or .conclusion == "timed_out") | .name]'
c=$?; echo; echo "--- $c"
"""

REVIEWS = r"""for n in {prs}; do
gh api "repos/{owner}/{repo}/pulls/$n/comments" --jq '.[] | "\(.pull_request_url | split("/") | last)\tc\(.id)\t\(.user.login)\t\(.path):\(.line // .original_line)\t\(.body | gsub("[\r\n\t]"; " "))"' || exit 1
gh api "repos/{owner}/{repo}/pulls/$n/reviews" --jq '.[] | select(.body != "") | "\(.pull_request_url | split("/") | last)\tr\(.id)\t\(.user.login)\t-\t\(.body | gsub("[\r\n\t]"; " "))"' || exit 1
gh api "repos/{owner}/{repo}/issues/$n/comments" --jq '.[] | "\(.issue_url | split("/") | last)\ti\(.id)\t\(.user.login)\t-\t\(.body | gsub("[\r\n\t]"; " "))"' || exit 1
done"""


def shell(command: str, **words) -> Generator[set[str], object, Exit]:
  ran = bash(command, show=HIDDEN, ok=True, **words)
  while not over(ran):
    yield {ran}
  if isinstance(got := peek(ran), Exit):
    return got
  raise Refused(f"{ran} came to {got!r}")


def merged_since() -> str:
  lines = [*heard, *(["Merged: " + " ".join(f"#{n}" for n in landed) + "."] if landed else [])]
  heard.clear()
  landed.clear()
  return ("\n\nSince the last notice:\n" + "\n".join(f"- {x}" for x in lines)) if lines else ""


def notice(key: str, message: str) -> None:
  if key not in noticed:
    noticed.add(key)
    prompt(None, message + merged_since(), on=home())


def once(key: str, line: str) -> None:
  if key not in noticed:
    noticed.add(key)
    heard.append(line)


def cap() -> int:
  return min(MOST, sum(limits["room"].values()))


def model_of(item: dict) -> str:
  kind = item.get("kind", "edit")
  return item.get("model") or (limits["fixer"] if kind == "fix" else limits["models"].get("audit" if kind == "audit" else "editor", limits["fixer"]))


def runs() -> list[str]:
  return list(dict.fromkeys([*limits["models"].values(), limits["fixer"]]))


def family(model: str) -> str:
  name = model.split(":")[-1]
  return name if name in limits["room"] else name.split("/")[0]


def living() -> list[str]:
  return [n for n in working if not over(n)]


def unheard(name: str) -> bool:
  return clock() - progress.get(name, (0, clock()))[1] >= QUIET


def running() -> list[str]:
  return [n for n in living() if not paused(n) and not unheard(n)]


def room(model: str) -> bool:
  mine = family(model)
  return len(running()) < cap() and len([n for n in running() if family(working[n]["model"]) == mine]) < limits["room"].get(mine, 0)


def held_keys() -> set[str]:
  return {q["key"] for q in [*queue, *working.values()]}


def ended(key: str) -> list[dict]:
  return [x for x in working.values() if x["key"] == key and "pr" in x]


def taken(key: str) -> str:
  return next((w for w in living() if working[w]["key"] == key), "") or ("queued" if any(q["key"] == key for q in queue) else "")


def maker_key(pr: dict) -> str:
  return next((m[2:] for m in pr["labels"] if m.startswith("w:")), "")


def ours(pr: dict) -> bool:
  return maker_key(pr) in held_keys()


def maker_of(pr: dict) -> str:
  return next((w for w in living() if working[w]["key"] == maker_key(pr)), "")


def owner_of(pr: dict) -> str:
  held = assigned.get(pr["n"]) or {}
  fixer = held.get("who", "") if held.get("fixer") and not over(held.get("who", "")) else ""
  return fixer or maker_of(pr)


def working_at(name: str) -> str:
  return str(get(name)[3])


def maker_chain(pr: dict) -> str:
  made = [w for w in working if working[w]["key"] == maker_key(pr)][-1:]
  return working_at(made[0]) if made else ""


def home() -> str:
  return scope(get(acting())[2])


def slug(text: str) -> str:
  return "-".join(re.findall(r"[a-z0-9]+", text.lower()))[:34].strip("-")


def files(item: dict) -> set[str]:
  return {x for a in item.get("areas", []) if (x := a.strip("/ `")) and not TESTS.search(x)}


def overlap(mine: set[str], theirs: set[str]) -> list[str]:
  pairs = [(x, y) for x in mine for y in theirs if x == y or x.startswith(y + "/") or y.startswith(x + "/")]
  return sorted({p for x, y in pairs if ALONE.search(p := y if y.startswith(x + "/") else x)})


def seated() -> set[str]:
  return {working[n].get("slot", "") for n in living()} - {""}


def placed(where: str) -> Generator[set[str], object, tuple[str, str]]:
  held = seated()
  live = " ".join(shq(working_at(n)) for n in living() if working_at(n) != where)
  heads = shq(" ".join(merges.values()))
  for slot in (f"w{i}" for i in range(1, 2 * MOST + 3)):
    path = f"{cwd()}-{slot}"
    code = 3 if slot in held else (yield from shell(f"sh -c {shq(SETTLE)} sh {shq(path)} {shq(where)} {heads} {live}", timeout=120)).code
    if code == 5:
      once(f"kept {path}", f"The workspace {path} holds work that no branch holds, so the round gives it to no worker until it is pushed or dropped.")
    elif code != 3:
      break
  else:
    raise Refused("each workspace holds a live worker")
  rung(f"try:\n  cd({path!r})\nexcept Refused:\n  pass", on=where)
  return WORKSPACE.replace("{path!r}", repr(path)).replace("{path}", path), slot


def carried(key: str) -> str:
  held = [(aid, a) for aid, a in owner_asks.items() if key in [str(i) for i in a.get("items") or []]]
  if not held:
    return ""
  lines = "\n".join(f'- {aid}: "{a.get("quote", "")}" ({a.get("prompt", "")})' for aid, a in held)
  ids = ", ".join(aid for aid, _ in held)
  return f'This item carries the asks {ids} of the owner, in his words:\n{lines}\nServe each of them whole. End your report with the line "Served: <ids>" and, for each part that stays undone, "Left: <id> (why)".\n\n'


def hire(item: dict) -> Generator[set[str], object, str]:
  key, kind, thread = item["key"], item.get("kind", "edit"), item.get("thread", "")
  model = item["model"] = model_of(item)
  marks = f" --label w:{key}" + (f" --label thread:{thread}" if thread else "")
  where = str(chain(f"{thread or 'w'}/{key}", source=item.pop("source", "")))
  rung(f"actor = {model!r}", on=where)
  text = {"audit": AUDIT, "edit": WORKER, "fix": "{what}"}[kind].replace("{what}", item["brief"])
  if "{workspace}" in text:
    space, item["slot"] = yield from placed(where)
    text = text.replace("{workspace}", space)
  for old, new in (("{labels}", marks), ("{repo}", cwd()), ("{key}", key), ("{home}", home()), ("{models}", ", ".join(runs())), ("{chain}", where)):
    text = text.replace(old, new)
  name = str(prompt(tuple[int, str], carried(key) + text, to=model, on=where))
  del item["brief"]
  working[name] = item
  return name


def waiting(item: dict) -> str:
  key, model = item["key"], model_of(item)
  if model not in runs():
    return f"it asks {model}, and a worker runs only {', '.join(runs())}"
  for dep in item.get("after", []):
    if not (done := ended(dep)) or dep in {q["key"] for q in queue} | {working[w]["key"] for w in living()}:
      return f"{dep} has not ended" + ("" if dep in held_keys() else ", and no item holds it")
    if shut := [x["pr"] for x in done if x["pr"] and x["pr"] not in merges]:
      return f"#{shut[0]} of {dep} has not merged"
  if not room(model):
    return f"{family(model)} has no room"
  mine = files(item)
  for w in living():
    if working[w]["key"] != key and (hit := overlap(mine, files(working[w]))):
      return f"the worker of {working[w]['key']} holds {hit[0]}"
  for n, pr in prs.items():
    if ours(pr) and f"w:{key}" not in pr["labels"] and (hit := overlap(mine, set(pr["files"]))):
      return f"#{n} changes {hit[0]}"
  return ""


def staff() -> Generator[set[str], object, None]:
  for item in list(queue):
    if len(running()) >= cap() - 1:
      return
    if cause := waiting(item):
      if not cause.endswith("has not ended"):
        once(f"waits {item['key']} {cause}", f"{item['key']} waits: {cause}.")
      continue
    queue.remove(item)
    yield from hire(item)


def register(key: str) -> None:
  try:
    rows = read(f"scratch://audit/{key}.md", HIDDEN).lines
  except Refused:
    return
  made, ranks, waits = [], {}, {}
  wide = int(any(re.fullmatch(r"\|\s*Dn\s*\|.*\|\s*after\s*\|.*", x.strip()) for x in rows))
  for line in rows:
    cells = [x.strip() for x in line.strip().strip("|").split("|")]
    rank, name = cells[0], f"{key}-{slug(cells[1])}" if len(cells) > 4 + wide else ""
    if not name or not re.fullmatch(r"D\d+", rank) or name == key + "-" or name in [x["key"] for x in made]:
      continue
    ranks[rank], waits[name] = name, re.findall(r"D\d+", cells[4]) if wide else []
    if name in held_keys():
      heard.append(f"The row {rank} of the register of {key} is {name}, which the round holds already, so it queued nothing for it.")
      continue
    model, area, brief = cells[2], cells[3], "|".join(cells[4 + wide :]).strip()
    item = {"key": name, "brief": f"{TRUTH}{brief}\n\nThis item is row {rank} of the register scratch://audit/{key}.md.", "thread": name}
    if (full := model if ":" in model else f"claude-cli:{model}") in runs():
      item["model"] = full
    if "/" in area:
      item["areas"] = [area.strip("`/ ")]
    queue.append(item)
    made.append(item)
  for item in made:
    if deps := [ranks[d] for d in waits[item["key"]] if d in ranks]:
      item["after"] = deps
  if made:
    heard.append(f"The register of {key} queued {', '.join(x['key'] for x in made)}.")


def kept_asks() -> None:
  write(Text(ASKS, json.dumps([{"id": i, **a} for i, a in owner_asks.items()], indent=1) + "\n"))


def left(item: dict, report: str) -> None:
  key = item["key"]
  ids = sorted({m for hit in ASK_LEFT.finditer(report) for m in re.findall(r"A\d+", hit[1])} & set(owner_asks))
  others = ({q["key"] for q in queue} | {working[w]["key"] for w in living()}) - {key}
  for aid in ids:
    a = owner_asks[aid]
    if a.get("state") == "decision" or others & {str(i) for i in a.get("items") or []}:
      continue
    if key.startswith("ask-"):
      a.update(state="decision", gap=report[:600])
      notice(f"left twice {aid} {key}", f"The follow-up {key} of the ask {aid} of the owner left it again, so it needs your decision and no new item. Its report:\n\n{report}")
      continue
    n = 1
    while f"ask-{aid.lower()}-{n}" in held_keys():
      n += 1
    name = f"ask-{aid.lower()}-{n}"
    a.update(state="partly", gap=f"{key} left it: {report[:400]}", items=[*(a.get("items") or []), name])
    brief = f"{TRUTH}The ask {aid} of the owner, in his words (from {a.get('prompt')}):\n\n> {a.get('quote')}\n\nThe worker of {key} left it, in its words:\n\n{report}\n\nDo what it left, and only that."
    queue.append({"key": name, "brief": brief, "thread": name, "model": model_of(item), "areas": list(item.get("areas", []))})
    heard.append(f"The worker of {key} left the ask {aid} of the owner, so the round queued {name}.")
  if ids:
    kept_asks()


def tend() -> None:
  for name, item in working.items():
    if "pr" in item or not over(name):
      continue
    got, key = peek(name), item["key"]
    n = item["pr"] = got[0] if isinstance(got, tuple) and got and isinstance(got[0], int) else 0
    report = got[1] if isinstance(got, tuple) and len(got) == 2 and isinstance(got[1], str) else repr(got)[:2000]
    if item.get("kind") == "audit":
      heard.append(f"The auditor {key} ({name}) ended: {report}")
      register(key)
    elif n > 0:
      heard.append(f"The worker {key} ({name}) ended with #{n}: {report}")
    else:
      notice(f"end {name}", f"The worker {key} ({name}) ended with no PR, which needs your decision:\n\n{report}")
    left(item, report)
  for name in living():
    facts = transcript(working_at(name))
    if progress.get(name, (-1, 0.0))[0] != len(facts):
      progress[name] = (len(facts), clock())
    elif unheard(name):
      acts = [str(x[1]) for x in facts if re.fullmatch(re.escape(str(x[0])) + r"\d+", str(x[1])) and not over(x[1])]
      notice(f"quiet {name} {len(facts)}", f"The worker {working[name]['key']} ({name}) said no fact on its chain for {int(QUIET // 60)} minutes, so the round counts it as no live worker. Its open acts: {', '.join(acts[-10:]) or 'none'}.")
  for gone in [n for n in progress if n not in living()]:
    progress.pop(gone)


def keep_disk() -> Generator[set[str], object, None]:
  last = {x["slot"]: i for i, x in enumerate(working.values()) if x.get("slot")}
  idle = sorted(set(last) - seated(), key=lambda s: last[s])
  live = " ".join(working_at(n) for n in living())
  got = yield from shell(f"sh -c {shq(DISK)} sh {KEEP} {shq(live)} " + " ".join(shq(f"{cwd()}-{s}") for s in idle), timeout=600)
  if got.code == 4:
    notice("disk short", f"The disk holds {got.stdout.lines[-1]} GB free, under {KEEP} GB, after the round deleted the target of each idle workspace: {', '.join(got.stdout.lines[:-1]) or 'none'}.")
  elif got.code == 0:
    noticed.discard("disk short")
  else:
    raise Refused(f"the keep of the disk exited {got.code}: {got.stderr.content.strip()[-200:]}")


def state(pr: dict) -> str:
  checks = pr["checks"]
  if pr["mergeable"] == "CONFLICTING":
    return "conflicting"
  if pr["open"] or (any(c in FAILS for c in checks) and all(c in (*FAILS, *GREEN) for c in checks)):
    return "red"
  return "green" if GATE in pr["names"] and all(c in GREEN for c in checks) else "running"


def trouble(pr: dict) -> str:
  now = state(pr)
  if now == "conflicting":
    return "conflicts with its base"
  if now != "red":
    return ""
  return "has an open box of its checklist: " + "; ".join(x[6:] for x in pr["open"]) if pr["open"] else "has a failed check"


def handler(pr: dict) -> str:
  held = assigned.get(pr["n"]) or {}
  who = str(held.get("who", ""))
  return who if who and not over(who) and held.get("sha") == pr["sha"] and (held.get("fixer") or held.get("problem") == trouble(pr)) else ""


def last_act(who: str) -> str:
  facts = transcript(working_at(who))
  return f"{facts[-1][0]} {facts[-1][1]}" if facts else "none"


def mend(pr: dict, problem: str) -> Generator[set[str], object, None]:
  n, sha = pr["n"], pr["sha"]
  held = assigned.get(n) or {}
  if who := handler(pr):
    if clock() - held["at"] >= UNMOVED:
      how = "sent to fix it" if held["fixer"] else "told of it"
      notice(f"stall {n} {sha} {who}", f"#{n} {problem} at {sha[:7]}, and {who}, whom the round {how}, has not moved its head in {int(UNMOVED // 60)} minutes. Its last act: {last_act(who)}.")
    return
  tries = held.get("tries", 0) if held.get("sha") == sha else 0
  if maker := maker_of(pr):
    order = FIXOWN.replace("{n}", str(n)).replace("{problem}", problem).replace("{sha}", sha[:7]).replace("{minutes}", str(int(UNMOVED // 60)))
    prompt(None, order, on=working_at(maker))
    assigned[n] = {"sha": sha, "problem": problem, "who": maker, "at": clock(), "fixer": False, "tries": tries}
    return
  if taken(f"fix{n}"):
    return
  if tries >= 2:
    notice(f"spent {n} {sha}", f"#{n} {problem} at {sha[:7]}, and 2 fixers ended at this head and did not move it, so the round sends no third one.")
    return
  if not room(limits["fixer"]):
    once(f"fixer {n} {sha}", f"#{n} {problem} at {sha[:7]}, and waits for a fixer on {limits['fixer']}, which has no room.")
    return
  brief = FIX.replace("{n}", str(n)).replace("{problem}", problem).replace("{sha}", sha).replace("{head}", pr["head"])
  thread = next((m[7:] for m in pr["labels"] if m.startswith("thread:")), "")
  who = yield from hire({"key": f"fix{n}", "brief": brief, "thread": thread, "kind": "fix", "source": maker_chain(pr)})
  assigned[n] = {"sha": sha, "problem": problem, "who": who, "at": clock(), "fixer": True, "tries": tries + 1}


def behind_by(pr: dict) -> str:
  return "gh api " + shq("repos/{owner}/{repo}/compare/" + pr["base"] + "..." + pr["sha"]) + " --jq .behind_by"


def land(ready: list[dict], flips: list[str]) -> Generator[set[str], object, None]:
  behind: dict[int, int] = {}
  if ready:
    got = yield from shell("".join(f"echo {pr['n']} $({behind_by(pr)}); " for pr in ready), timeout=300)
    for line in got.stdout.lines:
      bits = line.split()
      if len(bits) == 2 and bits[0].isdigit() and bits[1].isdigit():
        behind[int(bits[0])] = int(bits[1])
  steps = list(flips)
  fresh = [pr for pr in ready if behind.get(pr["n"]) == 0]
  for pr in fresh:
    if pr["state"] == "green":
      n, sha = pr["n"], pr["sha"]
      steps.append(f'[ "$({behind_by(pr)})" = 0 ] && {{ gh pr merge {n} --squash --match-head-commit {sha} || gh stack merge {n} --yes --squash; }}')
  if not any(pr["state"] == "running" for pr in fresh):
    stale = [pr for pr in ready if behind.get(pr["n"], 0) > 0 and f"{pr['n']} {pr['sha']}" not in updated]
    if stale:
      updated.add(f"{stale[0]['n']} {stale[0]['sha']}")
      steps.append(f"gh pr update-branch {stale[0]['n']}")
  if steps:
    yield from shell("".join(f"{{ {x}; }} >/dev/null 2>&1; " for x in steps), timeout=300)


def sweep() -> Generator[set[str], object, None]:
  got = yield from shell(PRS, timeout=300)
  bits = re.split(r"\n--- (\d+)\n", "\n" + got.stdout.content)
  if len(bits) != 11 or any(code != "0" for code in bits[1:8:2]):
    raise Refused(f"gh listed no PRs: {got.stderr.content.strip()[-300:] or 'it said nothing'}")
  mine = {n for n, pr in prs.items() if ours(pr)}
  prs.clear()
  prs.update({pr["n"]: pr for pr in json.loads(bits[0])})
  fresh = {n: head for n, head in json.loads(bits[2]) if n not in merges}
  merges.update(fresh)
  landed.extend(sorted(fresh.keys() & mine))
  main = json.loads(bits[4])
  red = "failure" in main["runs"]
  lead["main"] = str(main["sha"])
  gated = GATE in json.loads(bits[6])
  try:
    broke = set(json.loads(bits[8]))
  except ValueError:
    broke = set()
  if red:
    notice(f"red {main['sha']}", f"main is red at {str(main['sha'])[:7]}: find its cause first. The round sends no order and no fixer for a red PR until the gates of main pass.")
  if not gated:
    notice("ruleset", f"main has no ruleset that requires the check {GATE}, so the round merges no PR.")
  for gone in [k for k in assigned if k not in prs]:
    assigned.pop(gone)
  flips: list[str] = []
  ready: list[dict] = []
  for n in sorted(prs):
    pr = prs[n]
    if not ours(pr) or "paused" in pr["labels"]:
      continue
    now = pr["state"] = state(pr)
    failing = {name for name, end in zip(pr["names"], pr["checks"]) if end in FAILS}
    labelled = bool(set(HOLDS) & set(pr["labels"]))
    want = gated and not (labelled or pr["draft"] or pr["open"]) and now != "conflicting"
    if pr["auto"]:
      flips.append(f"gh pr merge {n} --disable-auto")
    if want and now in ("green", "running"):
      ready.append(pr)
    if now == "red" and red:
      once(f"main waits {n} {pr['sha']}", f"#{n} is red at {pr['sha'][:7]} while main is red, so it waits for main.")
    elif now == "red" and failing and not pr["open"] and not failing - broke:
      once(f"main fails {n} {pr['sha']}", f"#{n} is red at {pr['sha'][:7]} only on {', '.join(sorted(failing))}, which main fails too, so the round hires no fixer for it.")
    elif (problem := trouble(pr)) and not labelled:
      yield from mend(pr, problem)
  yield from land(ready, flips)


def review() -> Generator[set[str], object, None]:
  mine = sorted(n for n, pr in prs.items() if ours(pr))
  if not mine:
    return
  got = yield from shell(REVIEWS.replace("{prs}", " ".join(map(str, mine))), timeout=300)
  if got.code != 0:
    raise Refused(f"gh read no comments: {got.stderr.content.strip()[-300:]}")
  rows = [p for p in (x.split("\t", 4) for x in got.stdout.lines) if len(p) == 5 and p[1] not in comments]
  seed = not comments
  comments.update(p[1] for p in rows)
  held: set[int] = set()
  for p in [] if seed else [p for p in rows if p[2] == OWNER and not p[4].startswith("🤖") and p[0].isdigit() and int(p[0]) in prs]:
    pr = prs[int(p[0])]
    line = f"#{p[0]} {p[1]} {p[3]}: {p[4][:1500]}"
    own = owner_of(pr)
    if own:
      prompt(None, f"A new comment of the owner on your PR #{p[0]}. Do what it asks, answer it on GitHub with a reply that starts with 🤖, push the fix, and take the label hold off.\n{line}", on=working_at(own))
    elif not taken(f"fix{p[0]}"):
      answer = ANSWER.replace("{n}", p[0]).replace("{head}", pr["head"]).replace("{line}", line)
      queue.insert(0, {"key": f"fix{p[0]}", "brief": answer, "thread": next((m[7:] for m in pr["labels"] if m.startswith("thread:")), ""), "kind": "fix", "source": maker_chain(pr)})
    if "hold:owner-review" not in pr["labels"]:
      held.add(pr["n"])
  if held:
    yield from shell("".join(f"gh pr edit {n} --add-label hold >/dev/null 2>&1; " for n in sorted(held)), timeout=300)


def asked() -> None:
  if not owner_asks:
    return
  named = {p for a in owner_asks.values() for p in re.findall(r"prompt\d+", str(a.get("prompt", "")))}
  lines: dict[str, str] = {}
  for x in transcript(home(), "prompt"):
    if operated(str(x[2])) and x[4] == "str" and str(x[1]) not in named:
      lines[f"unasked {x[1]}"] = f"The message {x[1]} of the owner holds no ask in owner_asks."
  carrying = {q["key"] for q in queue} | {working[w]["key"] for w in living()} | {maker_key(pr) for pr in prs.values()}
  for aid, a in owner_asks.items():
    items = [str(i) for i in a.get("items") or []]
    if a.get("state") in ("done", "decision") or carrying & set(items):
      continue
    why = f"no item carries it now: its items {', '.join(items)} ended" if items else "no item names it"
    lines[f"uncarried {aid} {' '.join(items)}"] = f"The ask {aid} of the owner ({a.get('prompt')}) is {a.get('state') or 'open'}, and {why}."
  fresh = [k for k in lines if k not in noticed]
  last = lead.get("asked", "")
  if not fresh or last and not over(last):
    return
  noticed.update(fresh)
  head = "The words of the owner need the round before anything else:\n"
  tail = "\nSplit each message that holds no ask into its asks, each with his quote and the items that carry it. Queue an item for each ask that no item carries, or mark it done with the evidence of main."
  lead["asked"] = str(prompt(None, head + "\n".join(f"- {lines[k]}" for k in fresh) + tail + merged_since(), on=home()))


def owner_words() -> None:
  rows = []
  for x in transcript(ROOT, "prompt"):
    if operated(str(x[2])) and str(x[5]).strip():
      rows.append(f"## {x[1]}, his message\n\n{x[5]}\n\n")
    elif x[6] and operated(str(x[6])) and x[4] != "None" and isinstance(said := peek(x[1]), str) and said.strip():
      rows.append(f"## {x[1]}, his answer to my question\n\n{said}\n\n")
  write(Text(WORDS, "# The words of the owner in this session, verbatim, in order\n\n" + "".join(rows)))


def align() -> None:
  for name, brief in AUDITS.items():
    state = align_state.setdefault(name, {})
    if "base" not in state:
      state["base"] = str(len(merges))
      continue
    count = len(merges) - int(state["base"])
    if count < ALIGN_EVERY or any(re.fullmatch(rf"{name}\d+", k) for k in [*(q["key"] for q in queue), *(working[w]["key"] for w in living())]):
      continue
    n, held = 1, held_keys()
    while f"{name}{n}" in held:
      n += 1
    key, since, head = f"{name}{n}", state.get("sha", ""), lead.get("main", "")
    owner_words()
    window = f"{since}..{head}" if since and head else "-40 origin/main"
    for old, new in (("{key}", key), ("{count}", str(count)), ("{window}", window), ("{words}", WORDS), ("{asks}", ASKS), ("{held}", ", ".join(sorted(held_keys())) or "none")):
      brief = brief.replace(old, new)
    item = {"key": key, "brief": brief, "thread": name, "kind": "audit"}
    if n % 2 and (astra := limits["models"].get("astra")):
      item["model"] = astra
    queue.append(item)
    state.update(base=str(len(merges)), n=str(n), sha=head or since)
    heard.append(f"{count} PRs merged since the last audit of {name}, so the round queued {key}.")


def conduct(every: float = 300) -> Act:
  where = chain("round")
  cd(cwd(), on=where)

  def ear(id):
    yield "started", id
    old, lead["round"] = lead.get("round", ""), id
    if get(old) and not over(old):
      for x in transcript(old, "wait"):
        if old in x[5:] and not over(x[1]):
          close(None, x[1])
      gone = wait(every, old)
      while not over(gone):
        yield {gone}
    bash(LABELS, show=HIDDEN, ok=True, timeout=120)
    while True:
      alive: list[str] = []
      for at in ("tend", "sweep", "review", "disk", "staff", "asked", "align"):
        if lead.get("round") != id:
          yield "done", id, None
          return
        try:
          if at in ("sweep", "review", "disk", "staff"):
            yield from {"sweep": sweep, "review": review, "disk": keep_disk, "staff": staff}[at]()
          else:
            {"tend": tend, "asked": asked, "align": align}[at]()
          for done in [k for k in noticed if k.startswith(f"fault {at} ")]:
            noticed.discard(done)
        except Exception as fault:
          notice(f"fault {at} {type(fault).__name__}", f"The round {id} met a fault in its step {at}: {type(fault).__name__}: {str(fault)[:1000]}. It goes on at the next round, and tells this fault again only after a round of this step with no fault.")
        if at == "tend":
          alive = living()
      if not (queue or living() or any(ours(pr) and "paused" not in pr["labels"] for pr in prs.values())):
        yield "done", id, "The round holds no item and no open PR that it tends." + merged_since()
        return
      beat = wait(every, id, *dict.fromkeys([*alive, *living()]))
      while not over(beat):
        yield {beat}

  return act("conduct", where, controlled(ear))
