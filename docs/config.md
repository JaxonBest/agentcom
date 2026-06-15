# Configuration Reference

`agentcom.toml` lives in your project root. You can run `agentcom` commands from any subdirectory — it walks upward to find the config.

## Top-level fields

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

## `[[agent]]` fields

Each agent is defined by an `[[agent]]` table. You can have as many as `max_agents` allows.

| Field | Type | Default | Description |
|---|---|---|---|
| `name` | string | *(required)* | Unique agent handle. Lowercase letters, digits, `-`, `_` only. Reserved: `all`, `human`, `hub` |
| `role` | string | *(required)* | Appended to the system prompt as the agent's identity and responsibilities. **See [Writing strong agent roles](roles.md).** |
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

## `[hooks]` fields

| Field | Type | Default | Description |
|---|---|---|---|
| `post_close` | string | *(none)* | Shell command to run in the project root after a task transitions to Done. Non-zero exit re-blocks the task with the hook's stderr as the reason. Example: `post_close = "pytest -x --timeout=60"` |
| `post_close_timeout_secs` | integer | `120` | Timeout in seconds for the post_close hook before it is killed |
| `post_close_only_for_tags` | string[] | `[]` | Only run the hook when the closing agent has one of these capability tags. Empty means run for all agents |

> **Security:** Never interpolate `$AGENTCOM_TASK_TITLE` or other task env vars unquoted into shell commands within hook scripts.
