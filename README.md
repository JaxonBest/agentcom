# agentcom

A communication hub for fleets of [Claude Code](https://claude.com/claude-code) agents. agentcom spawns multiple `claude` instances as supervised child processes, gives them a **shared task board** and **inter-agent messaging** (including true mid-turn **interrupts**), and keeps them working autonomously for as long as there is work — so they can manage large codebases together.
───────────────────────────────────────────────────┘
```

## How it works

- **The hub drives the loop.** When an agent's turn ends, the hub composes its next prompt from pending inbox messages and the task board, and feeds it. Idle agents cost nothing; the hub wakes them when messages or tasks arrive. No polling, no stop-hook tricks.
- **Agents coordinate themselves.** Each agent's system prompt teaches it the `agentcom` CLI: claim tasks before working, file follow-up tasks it discovers, message teammates when it finishes something, and *interrupt* a teammate only to stop wasted or conflicting work.
- **Interrupts are real.** `agentcom interrupt <agent> "<msg>"` sends a `control_request` over the agent's stdin that aborts its in-progress turn; the urgent message is delivered the moment the aborted turn ends. If the child doesn't abort within `interrupt_timeout_secs`, the hub force-kills the process tree and restarts the session with `--resume`, delivering the message as the first prompt.
- **Everything survives restarts.** Tasks, messages, and run history live in SQLite under `%LOCALAPPDATA%\agentcom\<project-id>\` (deliberately outside OneDrive-synced folders).

## Quick start

```powershell
cargo install --path .       # or: cargo build --release

cd your-project
agentcom init                # writes agentcom.toml — edit your fleet
agentcom up --task "Refactor the auth module; keep tests green"
```

`agentcom up` opens the TUI dashboard: live agent output panes, the task board, the message feed, and keybindings to message (`m`), interrupt (`u`), broadcast (`M`), add tasks (`a`), pause (`p`), and stop (`s`) agents. Use `--headless` to run without it.

## Configuration (`agentcom.toml`)

```toml
project_name = "my-project"
# default_model = "sonnet"
# max_total_budget_usd = 20.0      # hub shuts down when total spend crosses this
# interrupt_timeout_secs = 15

[[agent]]
name = "builder"
role = "Implements features and fixes. Owns src/. Coordinates with reviewer before large refactors."
# cwd = "."                        # working dir, relative to this file
# model = "sonnet"
# allowed_tools = ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
# permission_mode = "acceptEdits"
# max_turns_per_prompt = 50
# max_budget_usd = 10.0            # this agent pauses when it has spent this much
# auto_restart = true              # respawn with --resume on crash

[[agent]]
name = "reviewer"
role = "Reviews changes made by other agents, runs tests, files follow-up tasks."
```

## CLI

From any terminal in the project (and from the agents themselves, via their Bash tool):

| Command | What it does |
|---|---|
| `agentcom status` | Fleet overview: states, spend, turns, open tasks |
| `agentcom send <agent\|all> "<msg>"` | Message an agent; delivered when its current turn ends |
| `agentcom interrupt <agent> "<msg>"` | Abort the agent's turn and deliver immediately |
| `agentcom inbox` | Read (and consume) your pending messages |
| `agentcom task add "<title>" [-d desc] [-p 0-4] [--dep id]` | Add work to the board (0 = highest priority) |
| `agentcom task list [--status open\|claimed\|done\|blocked]` | Show the board |
| `agentcom task claim/done/block/reopen <id>` | Task lifecycle |
| `agentcom agent add <name> --role "<role>"` | Add an agent: writes agentcom.toml and hot-spawns it if the hub is running |
| `agentcom agent list` | Configured agents with live state |
| `agentcom tail <agent> [-n 50] [-f]` | An agent's recent output (`-f` to follow live) |
| `agentcom pause/resume <agent>` | Pause after the current turn / resume |
| `agentcom stop [agent]` | Stop one agent, or the whole hub |

## Design notes

- Each agent is `claude -p --input-format stream-json --output-format stream-json` with stdin kept open; the hub is the single writer, so prompts and control requests never interleave.
- The stream-json codec tolerates unknown event types, unknown fields, and unparseable lines — a Claude Code upgrade degrades gracefully instead of crashing the hub. If the interrupt subtype is ever renamed, override it without recompiling: `AGENTCOM_INTERRUPT_SUBTYPE=<name>`.
- Agent discovery is zero-config: the hub injects `AGENTCOM_PORT` / `AGENTCOM_TOKEN` / `AGENTCOM_AGENT` into each child, so `agentcom <cmd>` invoked from an agent's Bash tool finds the hub and self-identifies. Human terminals discover via `hub.json` in the project data dir.
- Children are spawned with `CREATE_NEW_PROCESS_GROUP` (Ctrl+C in the hub's console doesn't nuke them); shutdown closes stdin, waits 5 s, then tree-kills via `taskkill /T`.
- `AGENTCOM_CAPTURE_RAW=<dir>` tees every raw NDJSON line from each agent to `<dir>/<agent>.ndjson` — handy for debugging and protocol fixtures.

## Testing

```powershell
cargo test
```

Integration tests run a real headless hub against `mock-claude` (a scripted stream-json test double in this repo), covering the task lifecycle, message passing, interrupts, ignored-interrupt escalation (kill + resume), pause/resume, and graceful shutdown — at zero API cost. The protocol codec is additionally locked against NDJSON fixtures captured from a live claude CLI session, including a real interrupted turn.

A cheap live test (one haiku agent, a few cents):

```powershell
agentcom init && agentcom up --headless --task "Say pong. That is the whole task."
```
