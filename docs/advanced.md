# Advanced Topics

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

> **Tip:** Pair free mode with [Recipe 4 — Overnight audit fleet](recipes.md#recipe-4--overnight-audit-fleet-3-read-only-workers--auto-composer) for a zero-risk, finding-only run. Wake up to a triaged backlog with no surprise edits.

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

### Configuration

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

### Security model

agentcom assumes a **trusted local fleet** — all agent processes run under the same operator's control on a single machine (or a shared LAN with a trusted network). Authentication is token-based but not agent-specific:

- Every agent is issued the same `AGENTCOM_TOKEN` at startup.
- IPC requests include an `identity` field that the agent sets to its own name (e.g. `"builder"`, `"security"`).
- The hub trusts whatever identity the agent declares — it does **not** verify that the caller *is* that agent.

**Consequence:** any agent that knows `AGENTCOM_TOKEN` can impersonate any other agent by sending IPC requests with a forged `identity`. This is by design for the target use case (single-operator laptop/server). The primary threat model is accidental misconfiguration, not a targeted adversary.

**If you need multi-tenant isolation** (e.g. untrusted agents, agents running on separate hosts, or a CI pipeline where agents come from different contexts), agentcom's authentication layer would need a redesign before it is safe to use in that environment.

---

## Testing

```
cargo test
```

The suite runs a real headless hub against zero-cost `mock-claude` and `mock-codex` binaries and the DeepSeek adapter's mock mode. It covers task lifecycle, message passing, interrupts, ignored-interrupt escalation, pause/resume, provider switching, file claims, free mode, recruitment, composer chat, and graceful shutdown.

The protocol codec is locked against NDJSON fixtures captured from a live Claude Code CLI session.
