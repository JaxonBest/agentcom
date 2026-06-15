# Fleet Recipes

Complete `agentcom.toml` files for common shapes. Drop one in your project root and tune from there.

> **Heads-up:** every recipe below uses the auto-injected composer. agentcom adds one for you whenever your config doesn't declare an `[[agent]] name = "composer"` block — you only need to define one yourself to customize its role or tighten its tools.

## Recipe 1 — Solo + safety net (2 agents)

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

## Recipe 2 — Backend team (3 workers + auto composer)

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

## Recipe 3 — Full-stack with a cheap junior (4 workers + auto composer)

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

## Recipe 4 — Overnight audit fleet (3 read-only workers + auto composer)

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

## Recipe 5 — Cost-tuned tiered fleet (2 workers + custom composer)

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

## Recipe 6 — Production-grade pipeline (8 workers + custom composer)

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

## Recipe 7 — Hyperscale sprint fleet (12 workers + custom composer)

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

## Budget guidance for heavy recipes

Per-session cost ranges (rough — depends on task size and how many tasks fire):

| Recipe | Workers | Opus seats | Sonnet seats | Cheap seats | Typical session | Hard ceiling |
|---|---|---|---|---|---|---|
| 6 — Production pipeline | 8 | 2 | 4 | 2 | $50–120 | `--budget 100` |
| 7 — Hyperscale sprint | 12 | 2 | 6 | 4 | $150–400 | `--budget 350` |

Always set `max_total_budget_usd` in the config AND pass `--budget` to `agentcom up --free`. The first is a hard kill switch; the second is the free-mode trigger. Belt and braces.
