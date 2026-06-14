# agentcom

**agentcom** is a local coordination hub for fleets of AI coding agents. You describe what you want; the hub turns it into tasks, dispatches Claude Code, Codex, and DeepSeek agents to claim them, and keeps everything in sync through a shared task board, atomic file claims, and an inter-agent message bus.

```
  you
   │  (chat)
   ▼
composer ──────────────────────────────────┐
   │  (plans, delegates, reports)          │
   ├──► builder     ──► shared board       │
   ├──► reviewer    ──► shared board       │
   └──► junior      ──► shared board       │
                                           │
   hub: IPC bus · SQLite · ring buffers ◄──┘
```

**When agentcom is the right tool:**

- You want a cheap **DeepSeek** worker grinding mechanical tasks while a careful **Claude** agent owns the hard ones.
- You want a **builder** and a **reviewer** running concurrently so every change is read by a second pair of eyes the moment it lands.
- You want a free-mode fleet running overnight under a strict budget cap, working a backlog of small improvements while you sleep.
- You want hard guarantees that two agents will never clobber the same file mid-edit.

**When it's not:** if a single careful agent on a single feature is fast enough, use that. agentcom shines when you have *parallel* work, *heterogeneous* work, or *long-running* work.

---

## Prerequisites

| Requirement | Notes |
|---|---|
| **Rust toolchain** | `rustup` + stable channel — [rustup.rs](https://rustup.rs) |
| **claude CLI** | Required for Claude agents — [install guide](https://docs.anthropic.com/en/docs/claude-code) |
| **codex CLI** | Optional, for Codex agents — [OpenAI Codex](https://github.com/openai/codex) |
| **DEEPSEEK_API_KEY** | Optional, for DeepSeek agents — [platform.deepseek.com](https://platform.deepseek.com) |

Verify your setup with:

```
agentcom doctor
```

---

## Installation

### From this repository

```
cargo install --path . --force
```

This installs four binaries:

| Binary | Purpose |
|---|---|
| `agentcom` | Main CLI and hub |
| `agentcom-codex-adapter` | Protocol bridge for Codex agents |
| `agentcom-deepseek-adapter` | Protocol bridge for DeepSeek agents |
| `mock-claude`, `mock-codex` | Zero-cost test stubs |

Verify:

```
agentcom --version
```

> **Windows tip:** If install fails with "cannot replace agentcom.exe", a hub is still running. Run `agentcom stop` first.

---

## Quick Start

**1. Initialize a project**

```
cd my-project
agentcom init
```

This writes `agentcom.toml` with a starter two-agent fleet (builder + reviewer). Edit it to fit your project — see **[Fleet Recipes](#fleet-recipes)** below for examples that go beyond the default.

**2. Start the hub**

```
agentcom up
```

The TUI opens. The composer agent starts and waits for your instructions.

**3. Tell the composer what you want**

Type in the Chat tab and press Enter:

```
Add input validation to the signup form and write tests for the new logic
```

The composer breaks this into tasks, claims them on the board, and directs your agents.

**4. Watch progress**

- Press `2` or Tab to see the selected agent's live output
- Press `3` to watch tasks being claimed and completed
- Press `4` to see the message feed between agents

**5. Respond to questions**

If an agent needs a decision, the TUI marks it as **QUESTION** in the header and chat panel. Type your answer and press Enter.

---

## Writing strong agent roles

The `role` field is the single most important knob in `agentcom.toml`. A weak role makes agents fight for tasks; a strong role gives each agent a clear lane and lets the composer route work confidently.

A strong role specifies four things:

1. **Lane** — what files, directories, or work-types this agent owns.
2. **Coordination protocol** — when to wait for another agent, when to file a task instead of acting, when to escalate to a human.
3. **Done criteria** — what "task complete" means for this agent (tests pass, lint clean, doc updated, etc.).
4. **Anti-patterns** — what this agent must *not* do, so the composer can route around it.

### Bad vs. good roles

| ❌ Weak | ✅ Strong |
|---|---|
| `"A simple software engineer"` | `"Implements server-side features in src/api/ and src/services/. Owns database migrations. Before any schema change, file a task tagged 'schema:' and wait for reviewer ack. Run 'cargo test --lib' before marking a task done. Never edit src/web/ — that is frontend's lane."` |
| `"Reviews code"` | `"Read-only. After every task close, run 'cargo test' and 'cargo clippy --all-targets'. File follow-up tasks tagged 'review-fail:' for any regression, missing test, or unclear change. Approve silently — only speak up on problems."` |
| `"Helps with small tasks"` | `"Handles mechanical, well-scoped tasks: typo fixes, rustfmt cleanup, adding #[derive(Debug)], renaming variables. Do NOT claim tasks tagged 'complex:' or 'architecture:', or any task without explicit done-criteria. If a task feels ambiguous, leave it."` |

### A library of strong roles

Copy these into your `agentcom.toml` and tune the file paths to your project.

**Composer (lead coordinator) — only define this if you want to override the default:**

agentcom auto-injects a sensible composer if you don't declare one, so most fleets can skip this block entirely. Define it explicitly only when you need a different role description, a tighter `allowed_tools` list, or a cheaper model.

```toml
[[agent]]
name = "composer"
role = """
Lead coordinator. Convert human goals into 1-5 board tasks with explicit done-criteria
and priorities. Match tasks to agents by their declared lane. Intervene when:
two agents are in the same lane, a task has been open >30min with no activity,
or a file-claim deadlock develops. Never edit code yourself — your job is routing,
not implementation. Report blockers to the human immediately; do not try to debug
them yourself.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
```

**Backend builder:**
```toml
[[agent]]
name = "backend"
role = """
Implements server-side features in src/api/, src/services/, and src/db/.
Owns database migrations in migrations/. Before any schema change, file a task
tagged 'schema:' and wait for reviewer ack before merging. Run 'cargo test --lib'
before marking a task done. Never edit src/web/ — that is frontend's lane.
If a task requires touching both backend and frontend, split it into two
linked tasks and notify the composer.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_turns_per_prompt = 60
max_budget_usd = 15.0
```

**Frontend builder:**
```toml
[[agent]]
name = "frontend"
role = """
Owns src/web/ and all React components. Coordinates with backend via the OpenAPI
contract in src/api/openapi.yaml — if the contract needs to change, file a task
instead of editing the file directly. Verify changes by running 'npm run dev' and
confirming the affected route renders without console errors. Never touch
src/api/ or src/db/.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
```

**Reviewer (silent approver, vocal critic):**
```toml
[[agent]]
name = "reviewer"
role = """
Read-only watchdog. After every task close, read the diff, run 'cargo test' and
'cargo clippy --all-targets -- -D warnings', and check that any new public API
has a doc comment. File follow-up tasks tagged 'review-fail:<short reason>' for
any regression, missing test, or unclear change. Approve silently — only message
the author on problems. Never edit code or claim implementation tasks.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
```

**Test engineer:**
```toml
[[agent]]
name = "tester"
role = """
Owns tests/ and #[cfg(test)] modules. After each task close by another agent,
ask: 'is this regression-proof?' If not, write the missing test in the same
session. Run 'cargo test' after every addition. File tasks tagged 'coverage-gap:'
when you find untested branches in code others wrote. Do not modify production
code to make tests pass — that is the original author's job; send them a message
instead.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
```

**Security auditor (read-only):**
```toml
[[agent]]
name = "security"
role = """
Read-only. Scan newly committed code for: hardcoded secrets, SQL/command injection,
unvalidated user input, missing auth checks, unsafe deserialization, and CSRF gaps.
File tasks tagged 'security:<severity>' where severity is low|med|high|critical.
For critical findings, also send the file's owner an --urgent interrupt with the
exact line and a one-sentence exploit sketch. Never edit code yourself.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
```

**Performance scout:**
```toml
[[agent]]
name = "perf"
role = """
Run 'cargo bench' in benches/ after each task close. If any benchmark regresses
>10% vs. the previous run, file a task tagged 'perf-regression:' with the bench
name, before/after numbers, and the offending commit hash. Do NOT optimize
speculatively — only respond to measured regressions. Keep a running log of
benchmark history in target/criterion/.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
```

**Docs writer:**
```toml
[[agent]]
name = "docs"
role = """
Owns README.md, docs/, CHANGELOG.md, and rustdoc comments on public items.
After any public API change (new pub fn, changed signature, new CLI flag, new
config field), update docs and any affected example. Run 'cargo doc --no-deps'
to verify nothing breaks. Tag your tasks 'docs:' so others can route review
appropriately. Never touch src/ for anything except doc comments.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
```

**Refactor specialist:**
```toml
[[agent]]
name = "refactor"
role = """
Spot and remove duplication, dead code, and over-engineered abstractions.
NEVER start a refactor without an open task — if you spot something worth
fixing, file the task with a clear scope and wait for it to be claimed.
Keep diffs under 200 lines per task; split bigger changes. Run 'cargo test'
after every change. If a refactor would touch >5 files, send the composer
a message before claiming.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
```

**Junior (cheap DeepSeek worker):**
```toml
[[agent]]
name = "junior"
role = """
Handle mechanical, well-scoped tasks: typo fixes, rustfmt cleanup, adding
#[derive(Debug)] or #[derive(Clone)], renaming variables for clarity, adding
missing match arms with todo!(). Do NOT claim tasks tagged 'complex:',
'architecture:', or 'security:'. Do NOT claim tasks without explicit
done-criteria in the description. If a task feels ambiguous, leave it for
the senior agents.
"""
provider = "deepseek"
model = "deepseek-chat"
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 2.0
```

---

## Fleet Recipes

Complete `agentcom.toml` files for common shapes. Drop one in your project root and tune from there.

> **Heads-up:** every recipe below uses the auto-injected composer. agentcom adds one for you whenever your config doesn't declare an `[[agent]] name = "composer"` block — you only need to define one yourself to customize its role or tighten its tools.

### Recipe 1 — Solo + safety net (2 agents)

The simplest useful fleet: one builder, one reviewer running concurrently. Reviewer catches what the builder missed. (Plus the auto-injected composer = 3 processes total.)

```toml
project_name = "my-project"
default_provider = "claude"
default_model = "sonnet"
max_total_budget_usd = 10.0

[[agent]]
name = "builder"
role = """
Implement features across the whole codebase. Run 'cargo test' before marking
any task done. If a task touches more than 5 files, send the human a plan
first and wait for approval.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]

[[agent]]
name = "reviewer"
role = """
Read-only. After every task close, run 'cargo test' and 'cargo clippy'. File
follow-up tasks tagged 'review-fail:' for any regression. Approve silently.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
```

### Recipe 2 — Backend team (3 workers + auto composer)

For server-side work where tests matter as much as the code.

```toml
project_name = "api-service"
default_provider = "claude"
default_model = "sonnet"
max_total_budget_usd = 25.0

[[agent]]
name = "backend"
role = """
Owns src/api/, src/services/, src/db/, and migrations/. Run 'cargo test --lib'
before marking tasks done. File 'schema:' tasks before any migration.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 12.0

[[agent]]
name = "tester"
role = """
Owns tests/ and #[cfg(test)] modules. Add missing tests for every task closed
by backend. Run 'cargo test' after each addition.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]

[[agent]]
name = "reviewer"
role = """
Read-only. After every task close, run 'cargo test' and 'cargo clippy
--all-targets -- -D warnings'. File 'review-fail:' tasks for regressions.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
```

### Recipe 3 — Full-stack with a cheap junior (4 workers + auto composer)

Backend and frontend run in parallel; a DeepSeek junior chews through small cleanups so the senior agents stay focused on features.

```toml
project_name = "full-stack-app"
default_provider = "claude"
default_model = "sonnet"
max_total_budget_usd = 30.0

[[agent]]
name = "backend"
role = """
Owns src/api/, src/db/. Coordinate with frontend via src/api/openapi.yaml —
file a 'contract-change:' task before editing it. Run 'cargo test --lib' before done.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]

[[agent]]
name = "frontend"
role = """
Owns web/. Verify with 'npm run dev' and check console for errors before marking
done. Never touch src/. Treat src/api/openapi.yaml as read-only.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]

[[agent]]
name = "junior"
role = """
Mechanical fixes only: typos, formatting, derive macros, renames. Skip tasks
tagged 'complex:' or without clear done-criteria.
"""
provider = "deepseek"
model = "deepseek-chat"
allowed_tools = ["Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 2.0

[[agent]]
name = "reviewer"
role = """
Read-only. Run tests + lint after every close. File 'review-fail:' tasks.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
```

### Recipe 4 — Overnight audit fleet (3 read-only workers + auto composer)

No code changes — just findings. Run with `--free` overnight on a stable branch and wake up to a triaged backlog.

```toml
project_name = "audit-pass"
default_provider = "claude"
default_model = "sonnet"
max_total_budget_usd = 8.0

[[agent]]
name = "security"
role = """
Hunt for: hardcoded secrets, injection vectors, missing auth checks,
unsafe deserialization. File 'security:<severity>' tasks. Severities:
low|med|high|critical. Never edit code.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]

[[agent]]
name = "perf"
role = """
Run 'cargo bench' and compare against target/criterion/ history. File
'perf-regression:' tasks for >10% regressions with bench name + numbers.
Also flag obvious algorithmic issues (O(n²) in hot paths) as 'perf-smell:' tasks.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]

[[agent]]
name = "docs-auditor"
role = """
Check every pub item in src/ has a rustdoc comment. Check README CLI section
matches actual 'agentcom --help' output. File 'docs-gap:' tasks for mismatches.
Never edit code.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
```

Start with:
```
agentcom up --free "find every issue worth fixing in this codebase" --for 8h --budget 8
```

### Recipe 5 — Cost-tuned tiered fleet (2 workers + custom composer)

Cheap models do most of the work; expensive Opus only claims tasks tagged `hard:`. This recipe **does** override the composer — we want it on Haiku (cheap) and we want it to use the `hard:` tag convention, which the default composer doesn't know about.

```toml
project_name = "cost-tuned"
default_provider = "claude"
default_model = "haiku"
max_total_budget_usd = 12.0

# Override the default composer: cheaper model + explicit tagging convention.
[[agent]]
name = "composer"
role = """
Lead coordinator. Tag tasks 'hard:' only when they require careful reasoning,
cross-file changes, or architectural judgment. Default to plain tasks for
mechanical work.
"""
model = "haiku"
allowed_tools = ["Bash", "Read", "Glob", "Grep"]

[[agent]]
name = "junior"
role = """
Mechanical fixes. Skip any task tagged 'hard:'. Stop if you find yourself
reading more than 3 files for a single task — that means it should be 'hard:'
and you should leave it.
"""
provider = "deepseek"
model = "deepseek-chat"
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 3.0

[[agent]]
name = "senior"
role = """
Claim ONLY tasks tagged 'hard:'. Run 'cargo test' before marking done.
If you discover a task is actually mechanical mid-work, untag and reopen
it for the junior.
"""
model = "opus"
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 8.0
```

### Recipe 6 — Production-grade pipeline (8 workers + custom composer)

The "ship to prod with confidence" fleet. Every task flows through a quality pipeline: architect gate → builder → tester → reviewer → security scan → perf gate → docs sync. Tiered models keep costs reasonable while putting Opus where it pays off (deep reasoning, gatekeeping).

**Expected cost:** $50–$120 per session of focused work. Pair with `--for 4h --budget 100` for a hard ceiling.

```toml
project_name = "production-rust-service"
default_provider = "claude"
default_model = "sonnet"
max_total_budget_usd = 120.0
max_agents = 10

# Custom composer: knows the tag taxonomy this fleet uses.
[[agent]]
name = "composer"
role = """
Lead coordinator. Tag every task with the right routing label:
  - 'arch:'       architectural decision; requires architect approval before any builder claims it
  - 'hard:'       complex reasoning, cross-file refactor, or perf-critical — goes to senior-builder
  - 'schema:'     database migration or public API contract change — wait for architect + reviewer ack
  - 'mechanical:' typos, formatting, derive macros, simple renames — junior territory
  - 'security:'   findings filed by the security agent; must be claimed by the file owner
Open at most 3 concurrent tasks per lane to prevent thrash. Never edit code.
If two agents file conflicting reviews on the same diff, escalate to the human.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]

# Read-only gatekeeper. Opus because architectural calls are where careful reasoning pays.
[[agent]]
name = "architect"
role = """
Read-only gatekeeper for tasks tagged 'arch:' or 'schema:'. When the composer
files one, read the spec, the affected code, and respond with either:
  - 'arch-approved:<id>' with a one-paragraph rationale, or
  - 'arch-block:<id>' with concrete redesign requirements.
Builders must NOT start an arch:/schema: task until you approve. While reading
other diffs, file 'arch-debt:' tasks when you spot smells (god objects, leaky
abstractions, circular deps). Never edit code yourself.
"""
provider = "claude"
model = "opus"
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
max_budget_usd = 30.0

# Senior builder — claims only the hard stuff. Opus to justify the cost.
[[agent]]
name = "senior-builder"
role = """
Claim ONLY tasks tagged 'hard:', 'arch:' (after arch-approved), or 'schema:'
(after arch-approved). Run the full check chain before marking done:
  cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
If you discover mid-work that a task is actually mechanical, untag and reopen
it for the junior. Never claim 'mechanical:' tasks — that is junior territory.
"""
provider = "claude"
model = "opus"
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_turns_per_prompt = 80
max_budget_usd = 40.0

# Workhorse builder — normal feature work.
[[agent]]
name = "builder"
role = """
Implement normal-difficulty features across src/. Skip tasks tagged 'hard:',
'arch:', 'schema:', 'mechanical:', or 'security:' — those belong to other lanes.
Run 'cargo test --lib' before marking done. If a task balloons past 200 LOC of
diff, stop and send the composer a message — it should probably be split.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 25.0

# Cheap mechanical worker.
[[agent]]
name = "junior"
role = """
Mechanical fixes ONLY: typos, rustfmt, derive macros, missing match arms with
todo!(), simple variable renames. Skip any task tagged 'complex:', 'hard:',
'arch:', 'schema:', or 'security:'. Skip tasks without explicit done-criteria.
Don't read more than 3 files for a single task; if you need to, stop and file
'needs-senior:<id>' instead.
"""
provider = "deepseek"
model = "deepseek-chat"
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 5.0

# Test engineer — owns regression coverage.
[[agent]]
name = "tester"
role = """
Owns tests/ and #[cfg(test)] modules. After every task closed by a builder,
ask: 'is this regression-proof?' If not, add the missing test in the same
session. Run 'cargo test' after each addition. Tag 'coverage-gap:' when you
find untested branches in code others wrote. Do NOT modify production code to
make tests pass — message the original author instead.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 15.0

# Diff reviewer — runs the full check chain after every close.
[[agent]]
name = "reviewer"
role = """
Read-only. After every task close, run the full check chain:
  cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
If any fails, file 'review-fail:<id>' and send the author an --urgent interrupt.
For diffs >100 LOC, also read the diff and look for: unhandled errors,
unwrap()/expect() in non-test code, panic! in library paths, missing rustdoc on
new pub items, TODO/FIXME without a tracking task. Approve silently — only
speak up on problems.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]

# Security scanner — read-only.
[[agent]]
name = "security"
role = """
Read-only. After every task close that touches src/, scan for:
  - hardcoded credentials or API keys (grep the diff)
  - SQL or command injection vectors
  - missing input validation at handler boundaries
  - unsafe deserialization (serde over untrusted input)
  - CORS misconfigurations
  - missing rate limits on auth endpoints
File 'security:<severity>' tasks where severity is low|med|high|critical.
Critical findings → send the file's owner an --urgent interrupt with the exact
line and a one-sentence exploit sketch. Never edit code.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
max_budget_usd = 10.0

# Performance gate — cheap, runs benchmarks.
[[agent]]
name = "perf"
role = """
After every task close on src/, run 'cargo bench' and compare against the
target/criterion/ baseline. >5% regression on any bench → file
'perf-regression:' with bench name, before/after numbers, and the commit hash.
Also flag obvious algorithmic issues (O(n²) loops in src/core/, unnecessary
clones in hot paths, blocking I/O in async functions) as 'perf-smell:' tasks.
Read-only. Use Haiku reasoning — this is pattern matching, not deep analysis.
"""
provider = "claude"
model = "haiku"
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
max_budget_usd = 5.0
```

**Recommended invocation:**
```
agentcom up --free "ship the planned features with zero regressions" --for 4h --budget 100
```

The pipeline naturally backpressures: a builder can't close a task without the reviewer's check chain passing, the reviewer flags regressions before they accumulate, and the architect blocks risky changes before code gets written.

### Recipe 7 — Hyperscale sprint fleet (12 workers + custom composer)

For weekend backlog crushes on a monorepo. Three builders work in disjoint lanes; two juniors handle different kinds of mechanical work; the full quality pipeline guards every close. Requires bumping `max_agents`.

**Expected cost:** $150–$400 per session. Always pair with `--budget` and `--for`.

```toml
project_name = "monorepo-sprint"
default_provider = "claude"
default_model = "sonnet"
max_total_budget_usd = 400.0
max_agents = 14
interrupt_timeout_secs = 30

# Custom composer — knows the full tag taxonomy and the per-lane file boundaries.
[[agent]]
name = "composer"
role = """
Lead coordinator for a monorepo sprint. Tag every task with BOTH a routing
label and a lane:
  Routing: arch: | hard: | schema: | mechanical: | security:
  Lane:    api:  | web:  | cli:    | shared:
Hand 'api:' tasks to builder-api, 'web:' to builder-web, 'cli:' to builder-cli.
'shared:' tasks (touch >1 lane) must go to senior-builder. Cap concurrent tasks
at 2 per lane to keep diffs reviewable. Watch the message bus for review-fail/
security-critical and escalate to the human if the same file fails review twice.
Never edit code.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]

# Opus gatekeeper.
[[agent]]
name = "architect"
role = """
Read-only gatekeeper for 'arch:' and 'schema:'. Approve with 'arch-approved:<id>'
or block with 'arch-block:<id>' + redesign notes. Also enforce the lane
boundaries: if a 'shared:' task is actually doable inside one lane, push it
back. File 'arch-debt:' tasks when you spot architectural smells during review.
"""
provider = "claude"
model = "opus"
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
max_budget_usd = 60.0

# Opus senior — only claims hard: or post-approved arch:/schema:/shared: work.
[[agent]]
name = "senior-builder"
role = """
Claim ONLY 'hard:', approved 'arch:'/'schema:', or any 'shared:' task that
touches more than one lane. Run the full check chain before done:
  cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
Coordinate with the affected lane builders via message bus before editing files
in their lane. Never claim 'mechanical:' tasks.
"""
provider = "claude"
model = "opus"
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_turns_per_prompt = 100
max_budget_usd = 80.0

# Three lane-scoped builders, all Sonnet. They never step on each other.
[[agent]]
name = "builder-api"
role = """
Owns src/api/, src/services/, src/db/, and migrations/. Only claim tasks tagged
'api:'. NEVER edit src/web/, src/cli/, or any file outside your lane. Coordinate
schema changes through 'schema:' tasks — never edit migrations/ without
architect approval. Run 'cargo test -p api' before done.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 40.0

[[agent]]
name = "builder-web"
role = """
Owns web/. Only claim tasks tagged 'web:'. Treat src/api/openapi.yaml as
read-only — if the contract needs to change, file a 'shared:' task. Verify
with 'npm run dev' and check the browser console for errors before done.
Never touch src/.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 40.0

[[agent]]
name = "builder-cli"
role = """
Owns src/cli/ and src/bin/. Only claim tasks tagged 'cli:'. Update shell
completions in completions/ after any flag change. Run
'cargo test -p cli && cargo run -- --help' to sanity-check before done.
Never touch src/api/, src/db/, or web/.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 30.0

# Two DeepSeek juniors with different specializations — keeps them out of each other's way.
[[agent]]
name = "junior-fmt"
role = """
Mechanical fixes across the whole repo: rustfmt, prettier, clippy autofixes,
derive macros, simple renames. Run formatters from the project root.
Skip any task tagged 'complex:', 'hard:', 'arch:', 'schema:', or 'security:'.
Never read more than 3 files for one task.
"""
provider = "deepseek"
model = "deepseek-chat"
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 8.0

[[agent]]
name = "junior-docs"
role = """
Mechanical doc work: add missing rustdoc on pub items, fix dead links in
markdown, add CHANGELOG entries for closed tasks. Never edit code logic.
Allowed file patterns: *.md, rustdoc /// comments only. Skip anything tagged
'complex:' or requiring judgment about wording.
"""
provider = "deepseek"
model = "deepseek-chat"
allowed_tools = ["Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 8.0

# Quality pipeline — same as Recipe 6 but tuned for parallel close-rate.
[[agent]]
name = "tester"
role = """
Owns tests/ and #[cfg(test)]. Add coverage for every task closed by any
builder. Run 'cargo test --workspace' after each addition. Tag 'coverage-gap:'
when finding untested branches. Do NOT edit production code to make tests
pass — message the author.
"""
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 25.0

[[agent]]
name = "reviewer"
role = """
Read-only. After every task close, run:
  cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
  && cargo fmt --check
Fail loud: file 'review-fail:<id>' + urgent interrupt to the author. For diffs
>100 LOC also check for unwrap()/expect() outside tests, missing rustdoc on
pub items, panic! in library paths, and TODO without a tracking task.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
max_budget_usd = 20.0

[[agent]]
name = "security"
role = """
Read-only. Scan diffs from every task close for: secrets, SQL/cmd injection,
missing input validation, unsafe deserialization, CORS misconfigs, missing
rate limits, unsafe blocks without SAFETY comments. Severity tags:
low|med|high|critical. Critical → urgent interrupt to the file's owner with
exact line + exploit sketch.
"""
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
max_budget_usd = 15.0

[[agent]]
name = "perf"
role = """
Run 'cargo bench --workspace' after each close on src/ paths. >5% regression
on any bench → 'perf-regression:' with bench name, numbers, commit hash.
Also flag O(n²) in hot paths, unnecessary clones, blocking I/O in async as
'perf-smell:'. Read-only. Use Haiku — this is pattern matching, not analysis.
"""
provider = "claude"
model = "haiku"
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
max_budget_usd = 8.0

# Docs syncer — cheap, runs after every close.
[[agent]]
name = "docs"
role = """
Owns README.md, docs/, CHANGELOG.md, and rustdoc on pub items. After every
task close that adds/changes public API, update docs in the same session and
add a CHANGELOG entry. Run 'cargo doc --no-deps' to verify. Never touch src/
except rustdoc comments. Tag your tasks 'docs:'.
"""
provider = "claude"
model = "haiku"
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_budget_usd = 6.0
```

**Recommended invocation:**
```
agentcom up --free "burn down the sprint backlog" --for 12h --budget 350 --usage 75
```

The triple-stop (time + budget + provider usage %) keeps an overnight run safe-by-default. The lane separation means the three builders never claim the same files, the two juniors stay out of each other's way, and every close runs through tester + reviewer + security + perf + docs. If one of those gates files a `review-fail:`, the original author gets an urgent interrupt and is forced to fix before claiming new work.

### Budget guidance for heavy recipes

Per-session cost ranges (rough — depends on task size and how many tasks fire):

| Recipe | Workers | Opus seats | Sonnet seats | Cheap seats | Typical session | Hard ceiling |
|---|---|---|---|---|---|---|
| 6 — Production pipeline | 8 | 2 | 4 | 2 | $50–120 | `--budget 100` |
| 7 — Hyperscale sprint | 12 | 2 | 6 | 4 | $150–400 | `--budget 350` |

Always set `max_total_budget_usd` in the config AND pass `--budget` to `agentcom up --free`. The first is a hard kill switch; the second is the free-mode trigger. Belt and braces.

---

## Configuration Reference

`agentcom.toml` lives in your project root. You can run `agentcom` commands from any subdirectory — it walks upward to find the config.

### Top-level fields

| Field | Type | Default | Description |
|---|---|---|---|
| `project_name` | string | *(required)* | Displayed in the TUI header and used for the data directory path |
| `default_provider` | `"claude"` \| `"codex"` \| `"deepseek"` | `"claude"` | Provider used by agents that don't set their own |
| `default_model` | string | *(none)* | Model used by agents that don't set their own (e.g. `"sonnet"`) |
| `max_total_budget_usd` | float | *(none)* | Stop everything once total tracked spend crosses this USD amount |
| `interrupt_timeout_secs` | integer | `15` | Seconds to wait for an agent to abort before force-killing it |
| `max_agents` | integer | `8` | Fleet size cap; prevents recruitment spirals |
| `partial_messages` | bool | `true` | Enable live streaming of partial agent output to the TUI |
| `auto_commit` | bool | `true` | Auto-commit any changed files when an agent releases its file claims. Author is set to the agent's name |
| `auto_commit_author_name` | string | *(agent name)* | Git author name for auto-commits (overridable per agent) |
| `auto_commit_author_email` | string | `<agent>@agentcom.local` | Git author email for auto-commits |
| `auto_commit_skip_hooks` | bool | `false` | Skip pre-commit hooks on auto-commits (`--no-verify`). Off by default — hooks enforce project policy |
| `commit_exclude_patterns` | string[] | `["agentcom.toml", ".agentcom/**"]` | Glob patterns for files to skip during auto-commit. Defaults protect hub state files |
| `webhook_url` | string | *(none)* | HTTP/HTTPS endpoint to POST hub events to (task done, agent crash, hub start/stop). Leave unset to disable |
| `webhook_secret` | string | *(none)* | Optional HMAC-SHA256 secret for webhook payload signing. Delivered as `X-Agentcom-Signature: sha256=<hex>` |

### `[[agent]]` fields

Each agent is defined by an `[[agent]]` table. You can have as many as `max_agents` allows.

| Field | Type | Default | Description |
|---|---|---|---|
| `name` | string | *(required)* | Unique agent handle. Lowercase letters, digits, `-`, `_` only. Reserved: `all`, `human`, `hub` |
| `role` | string | *(required)* | Appended to the system prompt as the agent's identity and responsibilities. **See [Writing strong agent roles](#writing-strong-agent-roles).** |
| `provider` | `"claude"` \| `"codex"` \| `"deepseek"` | `default_provider` | Runtime for this agent |
| `model` | string | `default_model` | Model override for this agent |
| `cwd` | path | project root | Working directory for the agent process (relative paths resolve from project root) |
| `allowed_tools` | string[] | `["Bash","Read","Edit","Write","Glob","Grep"]` | Tools the agent may use. Any tool not listed is auto-denied. Supports Bash patterns like `"Bash(npm test:*)"` |
| `permission_mode` | string | `"acceptEdits"` | Claude permission mode: `acceptEdits`, `plan`, `default`, `bypassPermissions` |
| `max_turns_per_prompt` | integer | *(none)* | Max autonomous turns per fed prompt. Caps a single work stretch |
| `max_budget_usd` | float | *(none)* | Cumulative USD spend cap for this agent across the hub's lifetime |
| `auto_restart` | bool | `true` | Automatically restart the agent if it crashes |
| `auto_commit` | bool | *(inherits global)* | Per-agent override for auto-commit. Set to `false` to opt this agent out even when the global setting is `true` |
| `auto_commit_author_name` | string | *(agent name)* | Git author name for this agent's auto-commits |
| `auto_commit_author_email` | string | `<agent>@agentcom.local` | Git author email for this agent's auto-commits |
| `max_rpm` | integer | *(none)* | Max API requests per minute. Hub skips feeding a new prompt if the agent exceeds this rate in the last 60 seconds |
| `env` | table | `{}` | Extra environment variables injected into this agent's process. Useful for per-agent API keys or tool flags. Example: `env = { ANTHROPIC_API_KEY = "sk-...", DEBUG = "1" }` |

---

## Provider Setup

### Claude (default)

Claude agents run as persistent `claude` CLI sessions using the stream-json protocol.

1. Install the Claude CLI: follow the [official guide](https://docs.anthropic.com/en/docs/claude-code)
2. Authenticate: `claude` (first run opens a browser login)
3. Verify: `claude --version`

Recommended models (set via `model` or `default_model`):

| Value | Use case |
|---|---|
| `sonnet` | Default — best balance of speed and quality |
| `opus` | Complex reasoning, architecture decisions |
| `haiku` | Fast, cheap tasks (linting, simple edits) |

### Codex

Codex agents run through `agentcom-codex-adapter`, which bridges the hub protocol to `codex exec --json`.

1. Install the Codex CLI from [github.com/openai/codex](https://github.com/openai/codex)
2. Authenticate with your OpenAI account
3. Verify: `codex --version`

**Windows note:** agentcom looks for Codex at `%LOCALAPPDATA%\OpenAI\Codex\bin\...\codex.exe` before falling back to `PATH`. If it can't be found:

```
$env:AGENTCOM_CODEX_EXE = "$env:LOCALAPPDATA\OpenAI\Codex\bin\<id>\codex.exe"
```

Interrupts work by tree-killing the active `codex exec` process and replaying the urgent message on the next turn.

### DeepSeek

DeepSeek agents run through `agentcom-deepseek-adapter`, which calls the DeepSeek OpenAI-compatible chat API. The adapter executes shell commands the model places in fenced code blocks, bounded by `allowed_tools`.

> **Security note:** DeepSeek agents execute shell commands from model output. Always set a tight `allowed_tools` allowlist and avoid pointing a DeepSeek agent at untrusted input files — a prompt-injection in the input could try to coax the model into running something dangerous. Treat DeepSeek agents like a CI runner: scope them narrowly.

1. Get an API key at [platform.deepseek.com](https://platform.deepseek.com)
2. Set the environment variable:
   ```
   # Windows
   $env:DEEPSEEK_API_KEY = "sk-..."
   # Linux/macOS
   export DEEPSEEK_API_KEY=sk-...
   ```
3. Add an agent to your config with `provider = "deepseek"`

Recommended models:

| Value | Use case |
|---|---|
| `deepseek-chat` | General tasks, review, triage |
| `deepseek-reasoner` | Complex reasoning and planning |

Optional overrides:

```
$env:DEEPSEEK_BASE_URL                 = "https://api.deepseek.com"   # default
$env:AGENTCOM_DEEPSEEK_INPUT_PER_MTOK  = "0.27"  # for budget tracking
$env:AGENTCOM_DEEPSEEK_OUTPUT_PER_MTOK = "1.10"
```

---

## CLI Reference

### Hub management

| Command | Description |
|---|---|
| `agentcom init [--force] [--template solo\|team\|mixed]` | Write a starter `agentcom.toml` in the current directory |
| `agentcom up` | Start the hub, spawn agents, open TUI |
| `agentcom up --restart` | Stop a running hub first, then start a fresh one — useful after config changes |
| `agentcom up --headless` | Start hub without TUI |
| `agentcom up --agents builder,reviewer` | Start only the named agents |
| `agentcom up --task "..."` | Seed a task before agents start (repeatable) |
| `agentcom status [--json]` | Fleet state, spend, turns, pending messages, open tasks |
| `agentcom stop` | Gracefully shut down all agents and the hub |
| `agentcom doctor` | Pre-flight check: CLIs, API keys, config validity |

### Offline tools (no hub required)

These commands read local files directly and work without a running hub.

| Command | Description |
|---|---|
| `agentcom logs [-n 100] [--agent <name>] [--follow]` | Read hub log files; reads across rotated daily logs; `-n` controls line count |
| `agentcom budget` | Per-agent spend and turn report from the run history database |
| `agentcom completions <bash\|zsh\|fish\|elvish>` | Print shell completion script to stdout |
| `agentcom config show` | Print the loaded `agentcom.toml` as pretty JSON — useful for scripting and debugging |
| `agentcom config set <key> <value>` | Update a config value in-place without editing TOML manually. Supports dotted paths: `agent.builder.model`, `auto_commit`, etc. |

### Real-time control

| Command | Description |
|---|---|
| `agentcom send <agent\|all> "<msg>"` | Queue a message for an agent |
| `agentcom send <agent> "<msg>" --urgent` | Queue with interrupt flag |
| `agentcom interrupt <agent> "<msg>"` | Abort current turn and deliver message immediately |
| `agentcom inbox` | Read and consume your pending messages |
| `agentcom agent pause <name>` | Pause after the current turn completes |
| `agentcom agent resume <name>` | Resume a paused agent |
| `agentcom tail <agent> [-n 50] [-f]` | Stream recent output (follow with `-f`) |
| `agentcom logs [-n <N>] [--agent <name>] [--follow]` | Read hub log files offline (useful for post-mortems) |

### Task board

| Command | Description |
|---|---|
| `agentcom task add "<title>" [-d "<desc>"] [-p 0-4] [--dep <id>]` | Add a task (0 = highest priority) |
| `agentcom task list [--status open\|claimed\|done\|blocked]` | List tasks, optionally filtered by status |
| `agentcom task list --search "<keyword>"` | Filter tasks by keyword in title or description |
| `agentcom task show <id>` | Show full details of a single task |
| `agentcom task claim <id>` | Claim a task (used by agents) |
| `agentcom task done <id> --note "<note>"` | Mark a task complete |
| `agentcom task block <id> --reason "<reason>"` | Mark a task blocked |
| `agentcom task reopen <id>` | Reopen a blocked or stuck-claimed task |
| `agentcom task edit <id> [-t title] [-d desc] [-p priority]` | Update task fields (PATCH — omitted fields unchanged) |
| `agentcom task remove <id>` | Permanently delete a task (not allowed if claimed) |
| `agentcom task prune [--before 7d]` | Delete all done/blocked tasks older than the given duration |
| `agentcom task export [--format md\|json]` | Dump the task board offline — Markdown checklist (default) or JSON array for scripting |
| `agentcom task stats [--json]` | Velocity metrics: avg completion time, throughput, blocked rate, top contributors |
| `agentcom task assign <id> <agent>` | Route a task directly to a specific agent; delivers a message so they pick it up |
| `agentcom task clone <id>` | Clone a task (copies title, description, priority into a new open task) |
| `agentcom task pin <id>` | Pin a task so it sorts before all non-pinned tasks |
| `agentcom task unpin <id>` | Unpin a task |
| `agentcom task tag <id> <label>` | Add a label to a task |
| `agentcom task untag <id> <label>` | Remove a label from a task |
| `agentcom task comment <id> "<body>"` | Append a timestamped comment to a task's activity log |
| `agentcom task due <id> <timestamp\|clear>` | Set or clear a due date for a task (Unix timestamp) |

### Agent fleet

| Command | Description |
|---|---|
| `agentcom agent add <name> --role "<role>" [--provider claude\|codex\|deepseek] [--model <m>] [--budget <usd>]` | Add an agent to config and spawn it live |
| `agentcom agent add <name> --role "<role>" --env KEY=VALUE` | Add agent with extra env vars (repeatable flag) |
| `agentcom agent add <name> --role "<role>" --initial-prompt "<msg>"` | Send a kickoff message immediately after spawning |
| `agentcom agent add <name> --role "<role>" --no-auto-restart` | Disable automatic restart on crash |
| `agentcom agent add <name> --role "<role>" --no-spawn` | Add to config only; starts on next `agentcom up` |
| `agentcom agent list` | List configured agents with live state |
| `agentcom agent remove <name>` | Remove agent from config (and stop it if hub is running) |
| `agentcom agent pause <name>` | Suspend an agent after its current turn; `resume` to wake it |
| `agentcom agent resume <name>` | Resume a paused agent |

### File claims

| Command | Description |
|---|---|
| `agentcom files claim <paths...>` | Claim files before editing (rejected if another agent holds any) |
| `agentcom files release <paths...>` | Release specific file claims |
| `agentcom files release --all` | Release all claims held by the current agent |
| `agentcom files list` | Show all current file claims |

---

## TUI Guide

`agentcom up` opens a full-terminal dashboard.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ agentcom - my-project  | $0.18 total | claude $0.12/4t | codex $0.06/2t      │  ← header: cost/usage
├────────────────┬─────────────────────────────────────────────────────────────┤
│ agents         │ 1 Chat  2 Output  3 Tasks  4 Messages  5 Hub Log            │  ← tab bar
│                ├─────────────────────────────────────────────────────────────┤
│ > | composer   │                                                             │
│   [claude]     │  composer: I've created 3 tasks. Builder is working on     │
│   . builder    │  the validation logic now.                                  │  ← chat panel
│   [claude]     │                                                             │
│   > reviewer   ├─────────────────────────────────────────────────────────────┤
│   [codex]      │ > working on task #2...           ← activity/agent status  │
│                │ - builder claimed src/auth.rs                               │
│                │ - reviewer filed task #4                                    │
├────────────────┴─────────────────────────────────────────────────────────────┤
│ > type message here|   (Enter send - Tab panes - ? help - Ctrl+C quit)      │  ← footer/input
└──────────────────────────────────────────────────────────────────────────────┘
  ↑ sidebar                                                          ↑ flash/hints
```

**Panels:**

- **Sidebar** — agent list with live state glyph (`>` working, `.` idle, `||` paused, `x` crashed) and provider badge
- **Chat** — your conversation with the composer; unread questions shown in yellow
- **Output** — live output stream for the selected agent; scroll with PgUp/PgDn
- **Tasks** — the shared board with status, priority, and assignee; board title shows open/wip/done/blocked counts. Press `/` to filter by keyword, `d` to hide done tasks, `Enter` on a row to open a full-screen detail popup
- **Messages** — full inter-agent and human message feed
- **Hub Log** — hub-level events (starts, stops, crashes, recruits)

### Keybindings

| Key | Action |
|---|---|
| `Tab` / `1`–`5` | Switch tabs |
| `Up` / `Down` / `j` / `k` | Select agent in sidebar (non-Tasks tabs) or navigate task list (Tasks tab) |
| `Enter` | Send chat message (Chat tab) or open task detail popup (Tasks tab) |
| `/` | Open task filter (Tasks tab) — type to search, Enter to apply, empty to clear |
| `F` | Clear task filter immediately |
| `d` | Toggle hiding done tasks (Tasks tab) |
| `m` | Message selected agent |
| `u` | Interrupt selected agent (urgent) |
| `M` | Broadcast message to all agents |
| `a` | Add a task directly to the board |
| `p` | Pause / resume selected agent |
| `s` | Stop selected agent |
| `PgUp` / `PgDn` | Scroll agent output |
| `End` | Jump to live output (stop scrolling) |
| `Esc` | Close task detail popup / clear chat input / cancel modal |
| `?` | Toggle this keybinding help overlay |
| `q` / `Ctrl+C` | Quit (prompts for confirmation) |

---

## Free Mode

Free mode lets the fleet pursue a broad goal autonomously until a stopping condition fires. When every agent goes idle, the hub nudges the composer to review the goal, the board, and the codebase, then queue the next useful work.

```
agentcom up --free "find bugs, improve tests, document what changed" --for 2h
agentcom up --free "polish the docs and examples" --budget 5.0
agentcom up --free "general code health" --for 90m --budget 10 --usage 80
```

### Stop conditions

| Flag | Format | Description |
|---|---|---|
| `--for <duration>` | `2h`, `90m`, `1h30m`, `45s` | Wall-clock limit from hub start |
| `--budget <usd>` | `5.0` | Stop when total tracked spend reaches this USD amount |
| `--usage <percent>` | `80` | Stop when the provider's 5-hour usage limit hits this percentage |

Conditions are **OR**'d — the first one to fire wins. Combine them for safe-by-default sessions:

```
agentcom up --free "refactor the payment module" --for 1h --budget 8
```

The composer is instructed to prefer high-value work and to report when nothing worth doing remains, preventing busy-work loops.

**Stall detection:** The hub monitors every Working agent. If an agent stays in the Working state for 10 minutes without completing a turn, a warning is logged. At 20 minutes the hub sends an urgent interrupt — "STALL DETECTED: finish your current turn now" — so the agent returns to idle and can pick up fresh work. Stall timers reset on each new turn.

> **Tip:** Pair free mode with **[Recipe 4 — Overnight audit fleet](#recipe-4--overnight-audit-fleet-4-agents-all-read-only)** for a zero-risk, finding-only run. Wake up to a triaged backlog with no surprise edits.

---

## Auto-commit

When an agent runs `agentcom files release --all`, the hub automatically stages and commits any files they modified, using the agent's name as the git author:

```
builder: task #12 implement rate limiting — src/auth.rs, src/config.rs, src/main.rs
Author: builder <builder@agentcom.local>

  src/auth.rs
  src/config.rs
  src/main.rs

Task: add per-route token expiry and refresh logic
```

The commit subject includes the task number and title (truncated to 60 chars) followed by the changed file names. The body lists each path on its own line and appends the first line of the task description. New and untracked files are staged automatically alongside modified files.

**Configuration:**

```toml
# agentcom.toml

# Enable/disable globally (default: true)
auto_commit = true

# Skip pre-commit hooks (default: false — hooks should run)
auto_commit_skip_hooks = false

# Files to never auto-commit (glob patterns)
commit_exclude_patterns = ["agentcom.toml", ".agentcom/**", "*.lock"]

# Override commit author for all agents
auto_commit_author_name = "agentcom bot"
auto_commit_author_email = "bot@example.com"

[[agent]]
name = "builder"
role = "..."
# Per-agent override — opt this agent out
auto_commit = false
# Or override just the author
auto_commit_author_email = "builder-bot@example.com"
```

Commit messages include the agent's currently claimed task title when available. New and untracked files are staged automatically alongside modified files.

---

## Architecture

```
agentcom up
│
├── Hub (tokio runtime)
│   ├── IPC server  — Unix/named-pipe socket; agents connect on startup
│   ├── Agent pool  — one child process per agent (Claude/Codex/DeepSeek)
│   ├── Store       — SQLite via rusqlite: tasks, messages, file claims
│   ├── Ring buffers — lock-free per-agent output buffers (TUI streaming)
│   └── Free-mode loop — idle-detection + composer nudge
│
├── TUI  — ratatui; reads ring buffers directly; sends IPC messages
│
└── Agents (child processes)
    ├── Claude  — persistent `claude -p --input-format stream-json` session
    ├── Codex   — agentcom-codex-adapter wraps `codex exec --json` per turn
    └── DeepSeek — agentcom-deepseek-adapter calls DeepSeek API; executes tool calls locally
```

**How coordination works:** Every agent has `AGENTCOM_PORT`, `AGENTCOM_TOKEN`, and `AGENTCOM_AGENT` injected into its environment. When an agent runs `agentcom task claim 5`, that `agentcom` invocation connects back to the hub over IPC, identifies itself with the token, and the hub enforces the claim atomically. File claims work the same way — the hub rejects a claim if another agent already holds any of the requested paths.

**Data lives outside the project:** The SQLite database and WAL files are stored under `%LOCALAPPDATA%\agentcom\data\<project-id>\` on Windows (or `~/.local/share/agentcom/` on Linux/macOS). This keeps state away from git and cloud sync. Your project only needs `agentcom.toml`.

---

## Testing

```
cargo test
```

The suite runs a real headless hub against zero-cost `mock-claude` and `mock-codex` binaries and the DeepSeek adapter's mock mode. It covers task lifecycle, message passing, interrupts, ignored-interrupt escalation, pause/resume, provider switching, file claims, free mode, recruitment, composer chat, and graceful shutdown.

The protocol codec is locked against NDJSON fixtures captured from a live Claude Code CLI session.

---

## Troubleshooting

**`cargo install` fails with "cannot replace agentcom.exe"**

A hub is running. Stop it first:
```
agentcom stop
cargo install --path . --force
```

**`agentcom up` says "claude not found"**

Install and authenticate the Claude CLI, then verify `claude --version` works from the same terminal.

**Codex agents stay in "starting" state**

On Windows, the Microsoft Store app alias may shadow the real executable. Point agentcom to it directly:
```
$env:AGENTCOM_CODEX_EXE = "$env:LOCALAPPDATA\OpenAI\Codex\bin\<version-id>\codex.exe"
```
Run `agentcom doctor` to check what agentcom actually finds.

**DeepSeek agent crashes immediately**

Verify your API key is set: `echo $env:DEEPSEEK_API_KEY`. Check the Hub Log tab (press `5`) for the specific error. Common causes: missing key, invalid key, or network proxy blocking `api.deepseek.com`.

**Two agents keep grabbing the same kind of task**

Their roles overlap. Tighten the lanes — give each one a specific directory or task tag, and add an explicit "never claim X" line to at least one. See **[Writing strong agent roles](#writing-strong-agent-roles)**.

**Task is stuck as "claimed" after a crash**

Tasks are reset to `open` automatically on the next `agentcom up`. To reopen one in a live session:
```
agentcom task reopen <id>
```

**File claim is blocking an agent**

See who holds it, then coordinate:
```
agentcom files list
agentcom send <agent> "please release src/foo.rs when you're done"
```
As a last resort, restart the hub — `agentcom up` clears all stale claims.

**The TUI is blank or corrupted on Windows**

This can happen if a previous run left the terminal in raw mode. Open a fresh terminal window. If the issue persists, check that Windows Terminal or ConPTY is being used (not a legacy `cmd.exe` window).

---

## License

MIT — see [LICENSE](LICENSE).
