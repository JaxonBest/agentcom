# CLI Reference

## Hub management

| Command | Description |
|---|---|
| `agentcom init [--force] [--preset solo+watchdog\|builder+reviewer+tester\|cheap-grunt+claude-lead]` | Write a starter `agentcom.toml` in the current directory |
| `agentcom up` | Start the hub, spawn agents, open TUI |
| `agentcom up --restart` | Stop a running hub first, then start a fresh one — useful after config changes |
| `agentcom up --headless` | Start hub without TUI |
| `agentcom up --agents builder,reviewer` | Start only the named agents |
| `agentcom up --task "..."` | Seed a task before agents start (repeatable) |
| `agentcom status [--json]` | Fleet state, spend, turns, pending messages, open tasks |
| `agentcom stop` | Gracefully shut down all agents and the hub |
| `agentcom doctor` | Pre-flight check: CLIs, API keys, config validity |
| `agentcom clean [--yes] [--keep-runs]` | Wipe all tasks, messages, and file claims from the DB (hub must not be running). Prompts for confirmation unless `--yes`. `--keep-runs` preserves run history (cost/turn data) |

## Offline tools (no hub required)

These commands read local files directly and work without a running hub.

| Command | Description |
|---|---|
| `agentcom logs [-n 100] [--agent <name>] [--follow]` | Read hub log files; reads across rotated daily logs; `-n` controls line count |
| `agentcom budget` | Per-agent spend and turn report from the run history database |
| `agentcom completions <bash\|zsh\|fish\|elvish>` | Print shell completion script to stdout |
| `agentcom config show` | Print the loaded `agentcom.toml` as pretty JSON — useful for scripting and debugging |
| `agentcom config set <key> <value>` | Update a config value in-place without editing TOML manually. Supports dotted paths: `agent.builder.model`, `auto_commit`, etc. |
| `agentcom replay [-n <N>] [--agent <name>]` | Human-readable session narrative reconstructed from hub logs (agent events, task transitions, messages) |

## Real-time control

| Command | Description |
|---|---|
| `agentcom send <agent\|all> "<msg>"` | Queue a message for an agent |
| `agentcom send <agent> "<msg>" --urgent` | Queue with interrupt flag |
| `agentcom interrupt <agent> "<msg>"` | Abort current turn and deliver message immediately |
| `agentcom inbox` | Read and consume your pending messages |
| `agentcom messages [--from <agent>] [--to <agent>] [-n <count>] [--json]` | Browse agent message history offline (no hub needed; reads messages DB) |
| `agentcom agent pause <name>` | Pause after the current turn completes (use `all` to pause the entire fleet) |
| `agentcom agent resume <name>` | Resume a paused agent (use `all` to resume the entire fleet) |
| `agentcom tail <agent> [-n 50] [-f]` | Stream recent output (follow with `-f`) |
| `agentcom logs [-n <N>] [--agent <name>] [--follow]` | Read hub log files offline (useful for post-mortems) |

## Task board

| Command | Description |
|---|---|
| `agentcom task add "<title>" [-d "<desc>"] [-p 0-4] [--dep <id>]` | Add a task (0 = highest priority) |
| `agentcom task list [--status open\|claimed\|done\|blocked]` | List tasks, optionally filtered by status |
| `agentcom task list --search "<keyword>"` | Filter tasks by keyword in title or description |
| `agentcom task list --tag <label>` | Filter tasks to those with a given label |
| `agentcom task show <id>` | Show full details of a single task |
| `agentcom task claim <id>` | Claim a task (used by agents) |
| `agentcom task done <id> --note "<note>"` | Mark a task complete |
| `agentcom task block <id> --reason "<reason>"` | Mark a task blocked |
| `agentcom task reopen <id>` | Reopen a blocked or stuck-claimed task |
| `agentcom task edit <id> [-t title] [-d desc] [-p priority]` | Update task fields (PATCH — omitted fields unchanged) |
| `agentcom task remove <id>` | Permanently delete a task (not allowed if claimed) |
| `agentcom task prune [--before 7d]` | Delete all done/blocked tasks older than the given duration |
| `agentcom task export [--format md\|json] [--output <FILE>]` | Dump the task board offline — Markdown checklist (default) or JSON array for scripting; `--output` writes to a file instead of stdout |
| `agentcom task stats [--json]` | Velocity metrics: avg completion time, throughput, blocked rate, top contributors |
| `agentcom task assign <id> <agent>` | Route a task directly to a specific agent; delivers a message so they pick it up |
| `agentcom task clone <id>` | Clone a task (copies title, description, priority into a new open task) |
| `agentcom task pin <id>` | Pin a task so it sorts before all non-pinned tasks |
| `agentcom task unpin <id>` | Unpin a task |
| `agentcom task tag <id> <label>` | Add a label to a task |
| `agentcom task untag <id> <label>` | Remove a label from a task |
| `agentcom task comment <id> "<body>"` | Append a timestamped comment to a task's activity log |
| `agentcom task due <id> [<YYYY-MM-DD\|timestamp>] [--clear]` | Set or clear a due date for a task (accepts YYYY-MM-DD or Unix timestamp; `--clear` removes it) |
| `agentcom task watch [<id>] [--interval <secs>]` | Live task board updates (Ctrl-C to exit); pass an id to poll a single task until done |
| `agentcom task remind <id> <agent>` | Send an inbox message to an agent pointing at a specific task |
| `agentcom task graph` | Print the task dependency graph as a Mermaid flowchart (paste into GitHub markdown for instant rendering) |
| `agentcom task import <FILE>` | Bulk-import tasks from a JSON snapshot (preserves dependency edges; remaps IDs) |

## Agent fleet

| Command | Description |
|---|---|
| `agentcom agent add <name> --role "<role>" [--provider claude\|codex\|deepseek] [--model <m>] [--budget <usd>]` | Add an agent to config and spawn it live |
| `agentcom agent add <name> --role "<role>" --env KEY=VALUE` | Add agent with extra env vars (repeatable flag) |
| `agentcom agent add <name> --role "<role>" --initial-prompt "<msg>"` | Send a kickoff message immediately after spawning |
| `agentcom agent add <name> --role "<role>" --no-auto-restart` | Disable automatic restart on crash |
| `agentcom agent add <name> --role "<role>" --no-spawn` | Add to config only; starts on next `agentcom up` |
| `agentcom agent list` | List configured agents with live state |
| `agentcom agent remove <name>` | Remove agent from config (and stop it if hub is running) |
| `agentcom agent pause <name>` | Suspend an agent after its current turn; `resume` to wake it (use `all` for fleet-wide pause) |
| `agentcom agent resume <name>` | Resume a paused agent (use `all` for fleet-wide resume) |
| `agentcom agent budget [<name>] [--json]` | Per-agent cost breakdown: total spent, turns, cost/turn, hourly burn rate (offline; no hub needed) |

## File claims

| Command | Description |
|---|---|
| `agentcom files claim <paths...>` | Claim files before editing (rejected if another agent holds any) |
| `agentcom files release <paths...>` | Release specific file claims |
| `agentcom files release --all` | Release all claims held by the current agent |
| `agentcom files list` | Show all current file claims |
