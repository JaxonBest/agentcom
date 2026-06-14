# agentcom

`agentcom` is a local coordination hub for fleets of coding agents. It can run
Claude Code and Codex agents in the same project, gives them a shared task
board, lets them message or interrupt each other, and provides a chat-first TUI
where you talk to a coordinator agent called the composer.

The core idea is simple: you describe what you want done, the composer breaks it
into work, workers claim tasks and files, and the hub keeps everyone moving
without letting agents silently overwrite each other.

## Current Shape

- **Mixed providers**: per-agent `provider = "claude"`, `provider = "codex"`, or `provider = "deepseek"`.
- **Composer-first UI**: `agentcom up` opens a chat with the composer; agent
  output is one tab away.
- **Shared board**: tasks are persisted in SQLite and claimed before work starts.
- **File claims**: agents claim paths before editing so conflicting work is
  rejected and rerouted.
- **Self-scaling**: agents can recruit more agents with `agentcom agent add`,
  bounded by `max_agents` and budgets.
- **Free mode**: give the fleet a broad goal plus a stopping condition, and it
  keeps generating useful work until the stop fires.
- **Fresh sessions**: every `agentcom up` resets stale claimed tasks and file
  claims before spawning agents. Done and blocked task history stays intact.

## Install

From this repository:

```powershell
cargo install --path . --force
```

If Windows says it cannot replace `agentcom.exe`, a hub is still running:

```powershell
agentcom stop
cargo install --path . --force
```

The install includes:

- `agentcom`
- `agentcom-codex-adapter`
- `agentcom-deepseek-adapter`
- `mock-claude` and `mock-codex` for tests

## Quick Start

In a project you want agents to work on:

```powershell
agentcom init
agentcom up
```

Then type your request into the TUI chat. The composer receives it, plans, files
tasks, delegates, and reports progress back to you.

You can also seed work from the command line:

```powershell
agentcom up --task "Fix the failing tests and explain the root cause"
agentcom task add "Audit src/auth for unsafe edge cases" -p 0
```

Use `--headless` when you want the hub without the TUI:

```powershell
agentcom up --headless --task "Run tests and fix failures"
```

## Configuration

`agentcom.toml` lives in the project root. Commands can be run from any
subdirectory; agentcom walks upward to find the config.

```toml
project_name = "my-project"

# Runtime for agents that do not set one directly.
# default_provider = "claude"        # or "codex" or "deepseek"

# Model for agents that do not set one directly.
# For Claude this is a Claude Code model name; for Codex this is a Codex model.
# For DeepSeek this is usually "deepseek-chat" or "deepseek-reasoner".
# default_model = "sonnet"

# Stop all work once total tracked spend crosses this.
# max_total_budget_usd = 20.0

# Seconds to wait for an interrupt to complete before force-killing.
# interrupt_timeout_secs = 15

# Worker cap. The injected composer gets its own slot.
# max_agents = 8

[[agent]]
name = "builder"
role = "Implements features and fixes. Claims files before editing and keeps tests green."
# provider = "claude"
# cwd = "."
# model = "sonnet"
allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
# permission_mode = "acceptEdits"
# max_turns_per_prompt = 50
# max_budget_usd = 10.0
# auto_restart = true

[[agent]]
name = "reviewer"
role = "Reviews changes, runs tests, and files follow-up tasks for problems found."
provider = "codex"
model = "gpt-5.4"
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
```

If you do not define a `composer`, `agentcom up` injects one automatically. To
customize it, add your own:

```toml
[[agent]]
name = "composer"
role = "Lead engineer. Turn human goals into tasks, prevent file conflicts, recruit specialists, and report progress."
provider = "claude"
allowed_tools = ["Bash", "Read", "Glob", "Grep"]
```

## Mixed Claude, Codex, and DeepSeek Fleets

You can mix providers freely. DeepSeek is useful as a lower-cost lane for
review, triage, planning, and simple scripted work:

```toml
default_provider = "claude"

[[agent]]
name = "builder"
role = "Primary implementer"
provider = "claude"

[[agent]]
name = "tester"
role = "Independent test runner and regression hunter"
provider = "codex"
model = "gpt-5.4"

[[agent]]
name = "reviewer"
role = "Reviews the final diff and checks for missed edge cases"
provider = "deepseek"
model = "deepseek-chat"
```

They share the same board, message bus, file claims, composer, and TUI.

Provider differences:

- Claude agents run as persistent `claude -p --input-format stream-json
  --output-format stream-json` sessions.
- Codex agents run through `agentcom-codex-adapter`, which keeps the hub protocol
  persistent and launches `codex exec --json` for each fed turn.
- DeepSeek agents run through `agentcom-deepseek-adapter`, which calls the
  DeepSeek OpenAI-compatible chat API. Because the API does not run local tools
  natively, the adapter executes commands the model places in fenced shell
  blocks, limited by the agent's `allowed_tools`.
- Claude supports native mid-turn control requests. Codex turns are stopped by
  tree-killing the active `codex exec` process and delivering the urgent message
  on the next turn.

## TUI

`agentcom up` opens the dashboard.

Tabs:

| Key | View |
|---|---|
| `1` | Chat with composer |
| `2` or Tab | Selected agent output |
| `3` | Task board |
| `4` | Message feed |
| `5` | Hub log |

Common keys:

| Key | Action |
|---|---|
| Enter in chat | Send message to composer |
| `a` | Add a direct task |
| `m` | Message selected agent |
| `M` | Broadcast to all agents |
| `u` | Interrupt selected agent |
| `p` | Pause selected agent |
| `s` | Stop selected agent / hub |
| Ctrl+C | Quit / graceful shutdown |

The chat view includes an activity panel so you can see what agents are doing
while they are thinking: current state, recent tool activity, task claims,
completions, recruitment, interrupts, and crashes.

The dashboard header shows total usage plus per-provider usage:

```text
$0.18 total | claude $0.12/4t | codex $0.06/2t
```

Each agent row also shows its provider badge, such as `[claude]`, `[codex]`,
or `[deepseek]`, so mixed fleets are easy to scan. Human-directed messages are highlighted in
the header, chat, and message feed. If an agent appears to be asking you a
question, the TUI labels it as `QUESTION` and keeps a visible count in the chat
footer until you handle it.

## Free Mode

Free mode is for broad goals where the fleet should keep working until a limit
is reached:

```powershell
agentcom up --free "find bugs, improve tests, and document what changed" --for 2h
agentcom up --free "polish docs and examples" --budget 5.0
agentcom up --free "general code health" --for 90m --budget 10 --usage 80
```

Stop conditions:

- `--for 2h`, `90m`, `1h30m`, or `45s`: wall-clock limit
- `--budget 5.0`: tracked spend limit
- `--usage 80`: provider usage-limit percentage when available

When the entire fleet is idle, the hub nudges the composer to review the goal,
the board, and the codebase, then queue the next useful work. The composer is
instructed to prefer valuable work over busywork and to tell you when nothing
worth doing remains.

## Sessions and Stopping Work

To stop all active work:

```powershell
agentcom stop
```

On the next `agentcom up`, agentcom starts a fresh session:

- claimed tasks are reset to `open`
- file claims are cleared
- done and blocked tasks remain as history

Useful commands while running:

```powershell
agentcom status
agentcom pause builder
agentcom resume builder
agentcom task list --status claimed
agentcom task reopen 12
agentcom files list
```

## CLI Reference

| Command | What it does |
|---|---|
| `agentcom init` | Write a starter `agentcom.toml` |
| `agentcom up [agents...]` | Start the hub and TUI |
| `agentcom up --headless` | Start the hub without TUI |
| `agentcom status` | Fleet state, spend, turns, pending messages, open tasks |
| `agentcom send <agent\|all> "<msg>"` | Queue a message for an agent |
| `agentcom interrupt <agent> "<msg>"` | Stop current work and deliver urgent message |
| `agentcom inbox` | Read and consume human-directed messages |
| `agentcom task add "<title>" [-d desc] [-p 0-4] [--dep id]` | Add a task |
| `agentcom task list [--status open\|claimed\|done\|blocked]` | List tasks |
| `agentcom task claim <id>` | Claim a task for the current agent |
| `agentcom task done <id> --note "<note>"` | Mark task done |
| `agentcom task block <id> --reason "<reason>"` | Block a task |
| `agentcom task reopen <id>` | Reopen a blocked/claimed/done task |
| `agentcom agent add <name> --role "<role>" [--provider claude\|codex\|deepseek]` | Add an agent |
| `agentcom agent list` | List configured agents and live states |
| `agentcom files claim <paths...>` | Claim files before editing |
| `agentcom files release <paths...>` | Release file claims |
| `agentcom files release --all` | Release all claims for current agent |
| `agentcom files list` | Show file claims |
| `agentcom tail <agent> [-n 50] [-f]` | Show recent output |
| `agentcom pause <agent>` | Pause after current turn |
| `agentcom resume <agent>` | Resume a paused agent |
| `agentcom stop [agent]` | Stop one agent or the entire hub |

## Environment Variables

| Variable | Purpose |
|---|---|
| `AGENTCOM_CLAUDE_EXE` | Override `claude` executable path |
| `AGENTCOM_CODEX_EXE` | Override `codex` executable path |
| `AGENTCOM_CODEX_ADAPTER_EXE` | Override bundled Codex adapter path |
| `AGENTCOM_CODEX_VERBOSE_STDERR` | Show raw Codex adapter stderr chatter |
| `AGENTCOM_DEEPSEEK_ADAPTER_EXE` | Override bundled DeepSeek adapter path |
| `DEEPSEEK_API_KEY` | API key for DeepSeek agents |
| `DEEPSEEK_BASE_URL` | Override API base URL; defaults to `https://api.deepseek.com` |
| `AGENTCOM_DEEPSEEK_INPUT_PER_MTOK` | Estimated input price for budget tracking |
| `AGENTCOM_DEEPSEEK_OUTPUT_PER_MTOK` | Estimated output price for budget tracking |
| `AGENTCOM_INTERRUPT_SUBTYPE` | Override Claude control-request interrupt subtype |
| `AGENTCOM_CAPTURE_RAW` | Tee raw child stdout lines to `<dir>/<agent>.ndjson` |
| `AGENTCOM_FREE_NUDGE_SECS` | Override free-mode idle nudge delay |

On Windows, `agentcom` first looks for the Codex app executable under
`%LOCALAPPDATA%\OpenAI\Codex\bin\...\codex.exe`, then falls back to `codex` on
`PATH`. If startup says Codex is missing, verify the CLI directly:

```powershell
codex --version
codex exec --json "say hello"
```

If the Microsoft Store `WindowsApps` alias is present but cannot run, point
agentcom at the real Codex executable:

```powershell
$env:AGENTCOM_CODEX_EXE = "$env:LOCALAPPDATA\OpenAI\Codex\bin\<id>\codex.exe"
agentcom up
```

The hub injects these into child agents:

- `AGENTCOM_PORT`
- `AGENTCOM_TOKEN`
- `AGENTCOM_AGENT`

That is how `agentcom <cmd>` calls from an agent identify and authenticate
against the running hub.

## Data Storage

Mutable runtime state is stored outside the project directory:

```text
%LOCALAPPDATA%\agentcom\data\<project-id>\
```

This keeps SQLite WAL files away from OneDrive sync. The project only needs
`agentcom.toml`.

## How This Differs From Similar Tools

Tools like Claude Squad and Crystal/Nimbalyst are strong parallel session
managers: they run multiple coding agents in isolated terminals or git
worktrees. `agentcom` is more of a local team coordinator:

- one shared task board instead of independent prompts only
- composer agent as the front door
- inter-agent messages and human inbox
- advisory file claims for same-worktree coordination
- optional mixed Claude/Codex fleet
- free mode for broad, bounded autonomy

The tradeoff: worktree isolation is harder safety; agentcom's file claims are
advisory and rely on agent compliance. The upside is tighter collaboration in a
single working tree.

## Testing

```powershell
cargo test
```

The test suite runs a real headless hub against zero-cost `mock-claude` and
`mock-codex` binaries plus the DeepSeek adapter's mock mode. It covers task lifecycle, message passing, interrupts,
ignored-interrupt escalation, pause/resume, provider switching, file claims,
free mode, recruitment, composer chat, and graceful shutdown.

The protocol codec is also locked against NDJSON fixtures captured from a live
Claude Code CLI session, including a real interrupted turn.
