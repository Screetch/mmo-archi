# mmo-archi

mmo-archi is the architecture of a scalable MMORPG in Rust, at its start. The workspace of cargo, `Cargo.toml`, keeps
each crate in `crates/`, and the crates of the game come one by one, each in a pull request, in the order that
`docs/architecture.md` gives. Claude Code reads this file through `CLAUDE.md`, which holds only `@AGENTS.md`.

The shape of the game is fixed, and each crate serves it:

- The server is the one authority of the state of the world. A client sends the intents of its player, and the server
  decides. A client can predict a result and correct it when the server answers, but it never decides.
- A shard is one independent world, and more shards hold more players. The world of a shard is cut into zones, and a
  zone runs one or more layers, each a full copy of the zone with its own cap of players. Each layer has exactly one
  writer, a process that alone writes the state of the layer.
- The gateway keeps no state: it ends the connection of a client, checks its session, finds the writer of the layer of
  the player in the directory, and sends each message to that writer. So any gateway serves any client of any shard,
  and a gateway that stops loses nothing.
- Redis holds the short shared state: the sessions, the presence, the directory of the layers and the chat.
  PostgreSQL holds the durable data, as the accounts, the characters and the items.
- The client opens its window with winit and draws with wgpu.
- The collisions run on parry3d, in a deterministic crate that the server and the client share, so a prediction of the
  client and the decision of the server come from the same code.
- The game does not use Bevy.

The files beside the crates:

- `rust-toolchain.toml` pins the one Rust of every build, here and in CI, with clippy, rustfmt and
  `llvm-tools-preview`, which the coverage needs.
- `Cargo.toml` holds the one version of the workspace, its edition, and the bar of the Rust in
  `[workspace.lints]`. Each crate takes them with `workspace = true`, and has `publish = false`. `rustfmt.toml` holds
  the format of the Rust: a width of 100 and two spaces.
- `.cargo/config.toml` is the one home of the bar of the coverage of the Rust: its alias `cargo coverage` runs the
  tests of the workspace and measures them in the same run.
- `script/` holds the tools of the gates and of the workers: scripts of sh, and `script/title.py`, which the
  `python3` of the system runs with no package.
- `mmo-play`, the binary of `mmo-client`, is the game client: `cargo run -p mmo-client --bin mmo-play -- --demo` opens its
  window with no server, and it logs in with the variables `MMO_PLAY_AUTHORITY`, `_LOGIN`, `_GATEWAY`, `_NAME`,
  `_PASSWORD` and `_CHARACTER`. Never run it on a machine of the owner: it opens a window. The gates run it under a
  virtual display.
- `script/character.py` writes `crates/mmo-render/assets/character.glb`, the character of the game, with the
  `python3` of the system. Run it again after a change of the script, and commit the file that it writes.
- `script/hooks/` holds the hooks of git: `pre-commit` runs the guards of the gates before a commit.
- `.github/` holds the workflows, their actions, the rules of a pull request in `.github/pull_request_template.md`,
  and the forms of an issue in `.github/ISSUE_TEMPLATE/`: a defect and a request.
- `CHANGELOG.md` holds the releases, which release-please writes from `release-please-config.json` and
  `.release-please-manifest.json`.
- `.furb/config.json` names the extensions of the agents that work here, in `.furb/extensions/`: `pr` opens or updates
  the pull request of a branch with the template and waits for its checks, and `conductor` gives each item of the
  work to a worker on a chain of its own, in a worktree of its own, and tends the pull request of each worker.

## The meanings

The owner is the source of truth of what a word means, and a name serves its meaning strictly. When a name of the code
does not serve its meaning, the name is wrong, and the name changes.

- The authority of a state is the one process that decides it. The authority of the state of a layer is its writer. A
  client is never an authority.
- An intent is what a client asks for its player, as a move or the use of an item. The server checks it and decides
  what it changes. An intent is never a fact of the world.
- A zone is a part of the map of a world. It holds no state of its own: its layers hold it.
- A layer is one full copy of a zone in a shard, with its own state, its own players and its own cap of players. Each
  layer has exactly one writer, and two processes never write the state of one layer.
- A shard is one independent world, with its own zones and layers, its own directory, its own queue and its own
  players. A player of one shard never meets a player of another, and more shards hold more players.
- The gateway is stateless: all that it reads, it reads from Redis or from the message. A state that a gateway keeps
  is a defect.
- A session proves that a client signed in. The presence says which player is online, and where. The directory says
  which layers each zone runs in each shard, and which process writes each layer. Redis holds all three, and the chat.
- Durable data outlives each process and each restart, and PostgreSQL holds it. Short state lives in Redis, and the
  game can lose it and make it again.
- Deterministic means that the same state and the same inputs give the same result on the server and on each client.
  The shared crate of the collisions is deterministic, and code that breaks that is a defect.
- The doors of mmo-archi are the public items of each crate and the messages between a client and the servers. A
  change that forces a caller of a door to change code is a breaking change, as the template of a pull request says.

## The bar

You are not here to please the owner, and not here to reach a green gate as fast as possible. You are here to reach
perfection, whatever the effort and the time. When two shapes both pass every gate, the one that serves the meaning of
every word is the one, and a shape that is green and incoherent is not done.

Read the whole of each crate that you change and list each incoherence: a truth with two homes, a word with two
meanings, a part held by the wrong owner. Probe each, build its reshape, and measure its size and the tests. Keep what
adds coherence, then read it again.

The bar of the Rust lives in the lints of `Cargo.toml` and in `rustfmt.toml`, and the gates hold it with
`-D warnings`. Read them before you write or change a Rust file. A module that must break a lint holds an `expect` with
its reason at its top, and never an `allow`.

Words of a person in a session are private. Never quote them, translate them or attribute them in code, docs, skills,
commits, PRs or issues: state the rule they carry, in plain English.

## Skills

The skills of the project stand in `.agents/skills/`, one folder for each, with its `SKILL.md`. Claude Code reads
them through `.claude/skills`, a link to that folder. Read a skill before the work that it serves:

- Before you write or change a Rust file, read `rust`.
- Before you design or write a change, read `ponytail` and `expert`.
- Before you review a design note or a diff, read `ponytail-review` and `expert`.
- Before you write, change or review a test, read `test-audit`, and `testing` to choose the checks of a change.
- Before you make or change a stack of pull requests, read `gh-stack`.

`expert` names the other skills of each task, and the rules of mmo-archi that win over them.

## Questions of the owner

A question of the owner about a state or a delay, as "Why is it not built yet?", "Is it fine?" or "Why is this chain
paused?", most often points at a defect of the agent that he asks, and he wants that defect fixed durably. Read it as
that request. Fix the state at once, find the cause in your own work, make its durable fix at once (a change of the
instructions, of a tool or of a test, in a pull request), read the state again to verify, and only then answer, with
the fix and its proof. An answer that only explains is not done.

## The tests

- The tests of a crate drive it as its callers do, and assert exact values. A test never weakens an assertion to
  pass: a test that the code fails stays red until the code, or the rule that the test holds, is right.
- The tests of a module of Rust stand beside it, in its `mod tests`, as in `crates/mmo-protocol/src/codec.rs`.
- `cargo coverage` runs the tests of the workspace in the run that measures them, and fails under 100 percent of the
  lines, the functions and the regions. Each job of the gates that runs the tests of the Rust runs it, so no test runs
  with no measure. A gap closes with a test, or with the removal of the code that no caller needs, and never with a
  lower bar.
- A helper that only one file needs lives in that file.
- A test of speed compares two times that it measures in one run, and takes the least of several runs of each, or
  better counts the work that the code does, which no load changes. A bound in seconds fails on a machine under load.
  A test of a process that a timeout ends gives it a limit that a loaded machine cannot cross.

## Commands

Run every command from the root of the repository.

Set the timeout of each command, and the bound of each wait, to about twice the time that the step should take, never a
round 600 or 900 seconds in case: a ceiling that is too large hides a hang for as long. Make a step that can take
minutes print its progress, so that a hang shows at once.

Search the code with `git grep`, which reads the tracked files alone, and never with `grep -r` or `rg` on the root. A
worktree holds `target/`, and a walk of it takes minutes and ends at the timeout. To search an ignored or an untracked
file, give `grep -r` the path of a folder, as `crates`.

Never end a process by its name or by a pattern of its command line: not `pkill`, not `killall`, not `pkill -f`. A
pattern matches the windows of the owner, the processes of other workers and your own shell, whose command line holds
the pattern. Start a process that you may need to end with its pid written to `$TMPDIR`, or in a process group of its
own, and end that pid or that group alone.

The faults below come back on this machine:

- When git refuses a push over SSH, push over HTTPS with the credential helper of gh and a lease on the full sha of the
  remote branch: `git -c credential.helper= -c credential.helper='!gh auth git-credential' push
  --force-with-lease=<branch>:<sha> https://github.com/<owner>/<repo>.git HEAD:refs/heads/<branch>`. A push to a URL
  moves no tracking ref, so fetch the branch after it.
- `gh run view --log-failed` refuses while the run still runs. Take the id of a job from `gh run view <run> --json
  jobs`, then read `gh api repos/<owner>/<repo>/actions/jobs/<id>/logs`.
- macOS has no `timeout` and no `flock`, and its `sed` needs `sed -i ''` to edit in place.
- Every worktree of the clone shares one git stash and one set of ref locks. Run no `git stash`, since a pop can take
  the entry of another branch: to run a check on the old code, save your change with `git diff > $TMPDIR/p.patch`, run
  `git checkout -- <file>`, run the check, then `git apply $TMPDIR/p.patch`. A git command that fails with `cannot lock
  ref` met another worker: run it again after a few seconds.

Make a small change, run the checks that it touches, and compare a check that fails with the base before the change:
fix what your change broke, and report what was broken before.

- `script/worktree.sh <path> <branch> [base]`: make a git worktree whose build starts warm. It checks out a branch that
  stands on origin or in this clone, and moves a local branch that lags behind origin up to it; a new branch starts from
  `base`, `origin/main` by default. It copies on write the `target` of cargo of the main checkout, or of the worktree of
  this clone whose commit stands nearest, so cargo builds only the crates of the workspace again. It sets
  `core.hooksPath` of the clone to `script/hooks` when no value stands, so that the hooks of each worktree print only
  what fails. Copy on write needs APFS, Btrfs or XFS; on
  another file system, the script makes a plain worktree. Remove a worktree with `git worktree remove --force <path>`,
  then `git branch -D <branch>`. The checkout at the root can stand behind `origin/main`, so read and change code in a
  worktree.
- `sh script/build-slot.sh <command> [argument...]`: run a command that builds, as `cargo` or
  `git commit` (its hooks run cargo and the tests), in one of the slots of builds that all the worktrees of the machine
  share, with at most 4 jobs of cargo. A slot is a folder in `/tmp` that holds the pid of its holder, and the slot of a
  holder that ended goes to the next build. `FURB_BUILD_SLOTS` sets the slots (one for each 12 GB of memory, at least
  1; `FURB_BUILD_MEM_GB` replaces the memory the machine reports), `FURB_BUILD_JOBS` the cap of jobs (4),
  `FURB_BUILD_WAIT` the seconds to wait for a slot (1200, then exit 75) and `FURB_BUILD_DIR` the folder. The workers of
  the conductor build only through it: with no cap, 28 rustc processes took 13.8 GB, and the machine ran out of memory.
- `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`: the format and the lints of
  the Rust.
- `docker compose up -d`: start PostgreSQL and Redis for local work, as `compose.yaml` says. The tests of `mmo-store`
  run against the PostgreSQL that `DATABASE_URL` names, as `postgres://postgres:postgres@127.0.0.1:5432/postgres`, and
  each test makes a database of its own and drops it. So `cargo test`, `cargo coverage` and a commit that runs the
  hook need `DATABASE_URL`. Give them a server that holds no data of a player. The tests of `mmo-login` also start a
  Redis of their own for each test, with the program that `REDIS_SERVER` names, or else `redis-server` of the `PATH`,
  which `brew install redis` gives.
- `sh script/local.sh start`: build the servers and start the whole stack on this machine: PostgreSQL, Redis, the keeper,
  the login service, one layer writer and one gateway, each with the variables that its `config.rs` reads. It makes the TLS
  material for local work (an authority and one certificate for `localhost`, valid 30 days), and prints the addresses that
  a client uses. It runs no `docker compose`: it starts `postgres` and `redis-server` of the machine on the ports 55432
  and 56379, in `$TMPDIR/mmo-local`, so a store that already runs on 5432 or 6379 keeps its data. It needs initdb,
  postgres, psql, redis-server, redis-cli, openssl and nc, and it goes through `script/build-slot.sh`, so give its bash
  a timeout of 2400 seconds. Each process writes its log and its pid in that folder, and the other commands read them:
  `sh script/local.sh check` makes an account and a character through the login service, links to the gateway and waits
  for a snapshot of the zone, with the binary `mmo-check` and no window (the login service takes 5 new accounts an hour
  from one address, so restart the stack to make more); `sh script/local.sh status` lists the processes; `sh
  script/local.sh stop` ends those pids alone, then checks that none stays. The variables `MMO_LOCAL_DIR`,
  `MMO_LOCAL_PG_PORT`, `MMO_LOCAL_REDIS_PORT`, `MMO_LOCAL_KEEPER_PORT`, `MMO_LOCAL_LOGIN_PORT`, `MMO_LOCAL_ZONE_PORT`,
  `MMO_LOCAL_GATEWAY_PORT` and `MMO_LOCAL_PG_BIN` change the folder, the ports and the place of the programs of
  PostgreSQL. Set the same ports for `check` and `stop`. `sh script/local.sh bots <count>` runs the load test of
  `mmo-bots` against the stack, with the variables `MMO_BOTS_*` that its doc lists, and prints its report.
  `MMO_LOCAL_SHARDS` (1) sets the count of shards, `MMO_LOCAL_SHARD_CAP` (100) the cap of each shard and
  `MMO_LOCAL_LAYER_CAP` (20) the cap of a layer, and the stack runs a login service and a pool of layer writers for
  each shard, the writers on the ports from `MMO_LOCAL_ZONE_PORT` up. The bots go round the shards, and their report
  gives the layers that opened in each shard and the players on each. Set the same `MMO_LOCAL_SHARDS` for `bots` as for
  `start`. `sh script/report.sh <report> <count> <shards> [<waiting> [<recover>]]` reads that report and fails on a bot
  that did not play or wait in the queue, on a layer over its cap, on a shard with too few layers, on a queue that
  holds another count of bots than `<waiting>`, admits out of its order or gives no place each 7 s, and, when
  `<recover>` is 1, on a bot that did not play again in time or on its layer. `MMO_BOTS_WAITING` and
  `MMO_BOTS_RECOVER_SECONDS` set those two rules for the bots. `sh script/local.sh fault <gateway|keeper|writer>
  [seconds]` ends one process of the running stack with KILL, waits, and starts it again, `sh script/local.sh versions`
  prints the version of the save of each character, and `sh script/local.sh players` the players that the directory
  counts. `sh script/faults.sh <writer|gateway|keeper> <count> <log>` runs stage 6 for one fault on that stack. No
  stage and no test runs more than 500 bots.
  `MMO_LOCAL_PROFILE=release` builds and runs the release profile, which the numbers of `docs/architecture.md` need.
- `cargo coverage`: the tests of the workspace with their coverage, which must be whole, but for
  `crates/mmo-client/src/window.rs`, the one file that needs a window, which `.cargo/config.toml` leaves out. The tests of `mmo-render`
  draw to a texture and open no window, but they need a graphics adapter: the GPU of the machine, or lavapipe
  (`mesa-vulkan-drivers`) on Linux. It needs cargo-llvm-cov, which
  `cargo install cargo-llvm-cov --locked` installs. `cargo test --workspace` runs the tests with no measure, as a quick
  probe.
- `sh script/hooks/pre-commit run`: every guard of the commit hook, on the whole tree, each one the guard of furb for
  the same fault:
  - `stray`: no `*.rej`, `*.orig` or `*-E` file. `large-files`: no file that the change adds holds more than 500 KB.
  - `merge-conflict`, `trailing-whitespace`, `end-of-file` and `mixed-line-ending`: no marker of a merge conflict, no
    space or tab at the end of a line, one line end at the end of each text file, and no file that mixes CRLF and LF.
  - `toml`: each TOML file parses, with taplo. `yaml`: each YAML file parses with yq, with no key twice in one mapping.
  - `workflows`: each workflow parses and keeps its shape, with actionlint, which reads no shell and no python, so a
    machine and CI read the same.
  - `paths`: each path of the repository that a file of `script/` or of `.github/workflows/` names stands, as
    `sh script/paths.sh` says.
  - `rustwidth`: no comment line that the change adds to a Rust file goes past 100 characters, which rustfmt does not
    wrap, as `sh script/rustwidth.sh [base]` says. A commit reads the lines that it adds to `HEAD`, and a hand run
    the lines that the tree adds to the merge base with `origin/main`. The lines that main holds are no debt of a
    change: wrap them when you change their file.
  - `fmt`, `lock`, `clippy` and `coverage`: `cargo fmt --all --check`, `cargo metadata --locked`, which fails when
    `Cargo.lock` does not agree with a manifest, clippy with `--locked`, and `cargo coverage`.

  A commit checks the files that it stages, as the index holds them; the guards of the workflows, of the paths and of
  the width read the tree, as their tools do. `SKIP` names the guards that a run leaves out, with commas, as
  `SKIP=coverage`. The guards need `brew install actionlint taplo yq`, and the contract below needs
  `cargo install cargo-semver-checks --version 0.51.0 --locked`: the gates pin actionlint 1.7.12, taplo 0.10.0, yq
  4.54.2 and cargo-semver-checks 0.51.0. A `git commit` runs the hook, which runs the guards of the Rust only when the
  commit changes a file that the crates or their tests read, and `cargo coverage` takes minutes: give it a timeout in
  `bash` of twice the time that it took last. The hook `script/hooks/pre-commit`, which
  `git config core.hooksPath script/hooks` turns on, prints only what fails. The hook `script/hooks/pre-push` refuses
  each push that moves or deletes `main` of a remote: `main` changes only through the merge of a pull request,
  `gh pr merge`, which runs no hook here. A merge commit, which `git merge` makes or `git commit` ends after a conflict,
  runs the hook on each file that it changes against its first parent, as a commit does, through
  `script/hooks/pre-merge-commit` when no conflict stops it. Run the push as its own command after the commit, so that
  its result stands in the tail of that command. The gates run each guard on every file.
- `sh script/contract.sh [--base <tag>] [--since <ref>] [--title <title>]`: name each break of a public item of a crate
  against the last tag `v*`, with cargo-semver-checks, as the check `the contract` does. It fails on a break unless the
  title holds a `!` after the area, or the base that `--since` names holds the break already. With no tag it passes.
  It builds the documentation of each crate, so run it through `sh script/build-slot.sh`.
- `printf '%s\n' <paths> | sh script/scope.sh`, or `sh script/scope.sh --all`: say which jobs of the gates a change
  needs, as the job `the scope` does.
- `python3 script/title.py "<title>"`: check the title of a pull request, as the check `the title` does.

## The gates and the releases

Every pull request and every push to `main` runs the gates in `.github/workflows/gates.yml`:

- `the scope` runs `script/scope.sh` on the paths that the change touches. A runner of macOS waits in a queue for
  minutes, so the jobs on macOS and on Windows run only when a path can behave in another way there: each path but
  prose, the tools of `script/`, `rustfmt.toml`, the files of the agents and the workflows that hold their own proof.
  A run by hand and the run of each night run every job.
- `the hooks` runs `sh script/hooks/pre-commit run` on Linux, with `SKIP=coverage`: every guard of the commit hook,
  with the width read against the base of a pull request.
- `the suite` runs `cargo coverage` on Linux, and `the crate on` runs clippy and `cargo coverage` on Windows and on
  macOS, each when the scope says that the change needs it.
- `the bots` runs the short stage 1 of the load test on Linux: `sh script/local.sh start`, then
  `sh script/local.sh bots 200` with layers of cap 20, and it fails when a bot does not play or ends with a fault, when
  a layer passes its cap, or when the layers do not open as the stage says. It keeps the report as an artifact.
- `the gates` stands for all the jobs, and a job that the scope skips passes there. The ruleset of `main` requires two
  checks: `the gates` and `the title`. It does not require `the contract`, and it does not require a branch that is up
  to date with `main`, so GitHub accepts a green pull request even when `main` moved after its checks ran. The
  conductor holds that rule in its own merge: it merges a pull request only when its head holds the tip of `main` and
  `the gates` passed on that head, and it updates a branch that is behind before it merges. Do the same for a merge by
  hand: update the branch and read its checks first.

`.github/workflows/titles.yml` runs `script/title.py` on the title of each pull request, at each change of the pull
request and at each edit of its title. `.github/workflows/contract.yml` runs `script/contract.sh` on each pull request
against the last tag and its base, at each change and at each edit of its title, so a break of a public item needs a
`!` in the title. The ruleset does not require this check, so read its result before a merge by hand.

A release is one version of the workspace. `.github/workflows/release.yml` keeps the pull request of the next release
open on `main`, with `CHANGELOG.md` and the version in `Cargo.toml` and `Cargo.lock`, and
its job `daily` merges it each day when its gates and the gates of `main` passed. Below 1.0.0, a fix, a feat or a perf
raises the patch version and a breaking change the minor version, as `bump-patch-for-minor-pre-major` and
`bump-minor-pre-major` say. Only the owner raises the major version, with
`gh workflow run release.yml -f version=1.0.0`. The merge tags the release, and `.github/workflows/publish.yml` builds
the binaries of the workspace for Linux, macOS and Windows and attaches them to the release on GitHub. It publishes to
no registry, since each crate has `publish = false`. A pull request that changes what a release builds runs the same
build as a rehearsal, and attaches nothing. Each workflow uses the token of the workflow alone, and no secret.

`release-please-config.json` names each crate in the path by which it writes the version in `Cargo.lock`. A pull
request that adds a crate adds its name there, or the release leaves the old version of that crate in `Cargo.lock`.

## Prose

Write all prose in Simplified Technical English, for a general non-technical audience. The rules are ASD-STE100, at
<https://www.asd-ste100.org/>. They hold for a chat message, a commit message, a pull request, a review comment, a code
comment and a document.

Prose here uses no em dash and no en dash. A docstring is a document.

## Pull requests

Read `.github/pull_request_template.md` whole and apply it strictly.

It is the one home of the rules of a pull request: its scope, its title, which release-please reads, and its body. The
check `the title`, in `.github/workflows/titles.yml`, runs `script/title.py` on each title, at each change of the pull
request and at each edit of its title, and reads the types from their one home, the `changelog-sections` of
`release-please-config.json`.

## Comments

A comment says what the code cannot tell by itself. It is a disclaimer: the code does not behave as a reader would
assume, and one sentence spares them a debugging session. Documentation adds edges to symbols and enumerates the
invariants; a public item of a crate always gets it, because the developer of another crate reads it. The code, its
names, the module tree and `git log` say the rest.

A comment older than your change is not yours to remove. `TODO` and `FIXME` go with the change that completes the work
they name. Write for the reader a year out: state what is true, never what changed. Fill each line to the width that
the formatter of its language keeps, 100 columns in Rust and 120 in python and in prose, then wrap.
