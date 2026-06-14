# agentcom — Session Handoff

**Session ended:** 2026-06-14 (clean — all tasks done, no in-flight work)

**Build:** clean, 0 warnings  
**Tests:** 18/18 e2e + 36 unit tests pass  
**Git:** 12 commits ahead of origin/main, working tree clean

---

## What Was Shipped This Session

| Task | Feature | Commit |
|------|---------|--------|
| #7 / #14 | `agentcom logs` — offline log reader across rotated daily files; `--agent` filter; `--follow` | `56c33d3` |
| #8 / #9 | `agentcom task edit/show` — PATCH semantics + full-detail view | `e09c259` |
| #10 | TUI polish — turns in sidebar, note column in tasks table, turn count in activity, working/idle header | `04a964c` |
| #11 / #13 | `agentcom task list --search` + `agentcom task remove` | `e09c259` |
| #12 | `agentcom completions <bash\|zsh\|fish\|elvish>` via clap_complete | `7a25391` |
| #15 | `agentcom task prune [--before 7d]` — delete old done/blocked tasks | `5666019` |
| #18 | `agentcom budget` — offline per-agent spend report from runs table | `d4912af` |
| #20 | `agentcom task export` — offline Markdown task board dump | `5666019` |
| #22 | `agentcom config show` — pretty-print agentcom.toml as JSON | `bf39fe1` |
| #21 / #28 | System prompt updated: task edit/show/remove/search/reopen/prune + logs | `56c33d3` / `384c8ac` |
| #23 / #26 | README CLI reference: offline tools section, all new commands documented | `77b4cde` / `609b331` |
| #27 | e2e tests: config_show, task_export, shell_completions, budget_command | `6e7dc18` |

---

## Current CLI Surface

### Hub management
`up`, `up --headless`, `up --free "<goal>" --for <t> --budget <usd>`, `stop`, `status [--json]`, `doctor`, `init [--template solo|team|mixed]`

### Task board (require running hub except marked)
`task add/list/claim/done/block/reopen/edit/show/remove`  
`task list --search <keyword>` — keyword filter  
`task prune [--before 7d]` — delete old done/blocked  
`task export` *(offline)* — Markdown checklist from DB

### Agents / Fleet
`agent add/list`, `send`, `interrupt`, `inbox`, `pause`, `resume`, `tail`, `files claim/release/list`

### Offline (no hub needed)
`logs [-n N] [--agent name] [--follow]`  
`budget` — per-agent spend from runs table  
`completions <shell>` — shell completion script  
`config show` — pretty-print agentcom.toml as JSON

---

## Task Board State

All 28 tasks are done or blocked-resolved. The board is cluttered with history.  
Run `agentcom task prune --before 0s` at the start of the next session to clear it, or leave it for reference.

---

## Suggested Next Work

- **CI pipeline** — add `.github/workflows/ci.yml` running `cargo build && cargo test`
- **`agentcom task export --format json`** — machine-readable board dump for scripting
- **DB schema migrations** — version the schema so future changes don't require a fresh DB
- **Rate limiting per agent** — `min_turn_interval_secs` config to prevent runaway agents
- **`agentcom push`** — optional remote sync of the task board / message history
