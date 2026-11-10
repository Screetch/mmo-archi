"""Check that the title of a pull request is a conventional commit of a type that release-please reads here.

The rule lives in `.github/pull_request_template.md`, and the types in `changelog-sections` of
`release-please-config.json`, which is their one home. A title is a type, its area in parentheses, a ! for a breaking
change if any, then a colon, a space and a sentence that starts with a capital letter, in 70 characters or fewer, as
`fix(tui): Reap the shell of a command that drops before its exit`.

Usage: python3 script/title.py "<title>"
"""

import json
import re
import sys
from pathlib import Path

CONFIG = Path(__file__).resolve().parent.parent / "release-please-config.json"


def types() -> list[str]:
  return [str(x["type"]) for x in json.loads(CONFIG.read_text())["changelog-sections"]]


LONGEST = 70


def fits(title: str, kinds: list[str]) -> bool:
  shape = rf"(?:{'|'.join(map(re.escape, kinds))})\([^()\s]+\)!?: [A-Z][^\n]*"
  return len(title) <= LONGEST and re.fullmatch(shape, title) is not None


def main() -> int:
  title, kinds = " ".join(sys.argv[1:]), types()
  if fits(title, kinds):
    sys.stdout.write(f"The title is right: {title}\n")
    return 0
  sys.stderr.write(
    f"The title is wrong: {title!r}\n"
    f"Start it with a type of {', '.join(kinds)}, its area in parentheses, a ! for a breaking change if any, then a"
    f" colon, a space and a sentence that starts with a capital letter, in {LONGEST} characters or fewer, as"
    " 'fix(tui): Reap the shell of a command that drops before its exit'. .github/pull_request_template.md says"
    " the rule.\n"
  )
  return 1


if __name__ == "__main__":
  sys.exit(main())
