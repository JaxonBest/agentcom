# agentcom

**agentcom** is a local coordination hub for fleets of AI coding agents. You describe what you want; the hub turns it into tasks, dispatches Claude Code, Codex, and DeepSeek agents to claim them, and keeps everything in sync through a shared task board, file-claim system, and inter-agent message bus.

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

This writes `agentcom.toml` with a starter two-agent fleet (builder + reviewer). Edit it to fit your project.

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

### `[[agent]]` fields

Each agent is defined by an `[[agent]]` table. You can have as many as `max_agents` allows.

| Field | Type | Default | Description |
|---|---|---|---|
| `name` | string | *(required)* | Unique agent handle. Lowercase letters, digits, `-`, `_` only. Reserved: `all`, `human`, `hub` |
| `role` | string | *(required)* | Appended to the system prompt as the agent's identity and responsibilities |
| `provider` | `"claude"` \| `"codex"` \| `"deepseek"` | `default_provider` | Runtime for this agent |
| `model` | string | `default_model` | Model override for this agent |
| `cwd` | path | project root | Working directory for the agent process (relative paths resolve from project root) |
| `allowed_tools` | string[] | `["Bash","Read","Edit","Write","Glob","Grep"]` | Tools the agent may use. Any tool not listed is auto-denied. Supports Bash patterns like `"Bash(npm test:*)"` |
| `permission_mode` | string | `"acceptEdits"` | Claude permission mode: `acceptEdits`, `plan`, `default`, `bypassPermissions` |
| `max_turns_per_prompt` | integer | *(none)* | Max autonomous turns per fed prompt. Caps a single work stretch |
| `max_budget_usd` | float | *(none)* | Cumulative USD spend cap for this agent across the hub's lifetime |
| `auto_restart` | bool | `true` | Automatically restart the agent if it crashes |

### Annotated example config

```toml
# ─── Project ──────────────────────────────────────────────────────────────────
project_name = "my-project"

# Default provider and model for agents that don't specify their own.
default_provider = "claude"
default_model    = "sonnet"

# Hub-wide spend guard (USD). Comment out for unbounded sessions.
max_total_budget_usd = 20.0

# How long to wait for a graceful interrupt before force-killing (seconds).
interrupt_timeout_secs = 15

# Maximum number of agents allowed at once (including the composer).
max_agents = 8

# ─── Agents ───────────────────────────────────────────────────────────────────

# The composer is optional — agentcom injects a default one if you omit it.
# Define it here to customize the role or restrict its tools.
[[agent]]
name    = "composer"
role    = "Lead coordinator. Turn human goals into tasks, prevent file conflicts, recruit specialists, and report progress. Never edit code yourself."
allowed_tools = ["Bash", "Read", "Glob", "Grep"]

[[agent]]
name    = "builder"
role    = "Implements features and fixes. Owns src/. Coordinates with reviewer before large refactors."
# allowed_tools lists EVERY tool the agent may use — anything not listed is denied.
allowed_tools     = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
max_turns_per_prompt = 50
max_budget_usd    = 10.0

[[agent]]
name    = "reviewer"
role    = "Reviews changes made by builder, runs tests, and files follow-up tasks for problems found."
# Read-only tools: reviewer should not edit files.
allowed_tools = ["Bash", "Read", "Glob", "Grep"]

# Optional: lower-cost DeepSeek agent for triage and simple tasks.
# [[agent]]
# name     = "junior"
# role     = "Handles well-scoped, low-complexity tasks from the board."
# provider = "deepseek"
# model    = "deepseek-chat"
# allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
# max_budget_usd = 2.0
```

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

### Real-time control

| Command | Description |
|---|---|
| `agentcom send <agent\|all> "<msg>"` | Queue a message for an agent |
| `agentcom send <agent> "<msg>" --urgent` | Queue with interrupt flag |
| `agentcom interrupt <agent> "<msg>"` | Abort current turn and deliver message immediately |
| `agentcom inbox` | Read and consume your pending messages |
| `agentcom pause <agent>` | Pause after the current turn completes |
| `agentcom resume <agent>` | Resume a paused agent |
| `agentcom tail <agent> [-n 50] [-f]` | Stream recent output (follow with `-f`) |

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

### Agent fleet

| Command | Description |
|---|---|
| `agentcom agent add <name> --role "<role>" [--provider claude\|codex\|deepseek] [--model <m>] [--budget <usd>]` | Add an agent to config and spawn it live |
| `agentcom agent add <name> --role "<role>" --no-spawn` | Add to config only; starts on next `agentcom up` |
| `agentcom agent list` | List configured agents with live state |

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
- **Tasks** — the shared board with status, priority, and assignee
- **Messages** — full inter-agent and human message feed
- **Hub Log** — hub-level events (starts, stops, crashes, recruits)

### Keybindings

| Key | Action |
|---|---|
| `Tab` / `1`–`5` | Switch tabs |
| `Up` / `Down` / `j` / `k` | Select agent in sidebar |
| `Enter` | Send chat message (Chat tab) |
| `m` | Message selected agent |
| `u` | Interrupt selected agent (urgent) |
| `M` | Broadcast message to all agents |
| `a` | Add a task directly to the board |
| `p` | Pause / resume selected agent |
| `s` | Stop selected agent |
| `PgUp` / `PgDn` | Scroll agent output |
| `End` | Jump to live output (stop scrolling) |
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
