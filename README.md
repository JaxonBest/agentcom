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

## Pick a preset

`agentcom init --preset <name>` writes a ready-to-use `agentcom.toml`. Three presets ship out of the box:

**`solo+watchdog`** *(default)* — one Claude builder plus one read-only reviewer. The reviewer runs `cargo test` and `cargo clippy` after every task close and files `review-fail:` tasks for regressions. Best starting point for any new project. ~$5–15/session.

**`builder+reviewer+tester`** — adds a dedicated test engineer that owns `tests/` and `#[cfg(test)]` modules. Every task close triggers both a test addition pass and a full lint review. Use this when coverage matters as much as features. ~$15–30/session.

**`cheap-grunt+claude-lead`** — a DeepSeek junior handles mechanical work (typos, formatting, derive macros) at pennies per task while a Claude senior claims only tasks tagged `hard:`. Cuts cost by 60–80% on mixed backlogs. ~$8–20/session.

For production pipelines, monorepo sprints, overnight audit fleets, and more, see **[docs/recipes.md](docs/recipes.md)**.

---

## 60-second quickstart

**1. Initialize a project**

```
cd my-project
agentcom init
```

This writes `agentcom.toml` with the default `solo+watchdog` preset (builder + reviewer). Edit roles or swap to another preset to fit your project — see [docs/recipes.md](docs/recipes.md).

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

If an agent needs a decision, the TUI marks it as **QUESTION** in the header. Type your answer and press Enter.

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

Their roles overlap. Tighten the lanes — give each one a specific directory or task tag, and add an explicit "never claim X" line to at least one. See [docs/roles.md](docs/roles.md).

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

Open a fresh terminal window. If the issue persists, check that Windows Terminal or ConPTY is being used (not a legacy `cmd.exe` window).

---

## Where to next

| Topic | File |
|---|---|
| Writing strong agent roles | [docs/roles.md](docs/roles.md) |
| Fleet recipes (7 complete configs) | [docs/recipes.md](docs/recipes.md) |
| Configuration reference | [docs/config.md](docs/config.md) |
| Provider setup (Claude / Codex / DeepSeek) | [docs/providers.md](docs/providers.md) |
| CLI reference | [docs/cli.md](docs/cli.md) |
| TUI guide & keybindings | [docs/tui.md](docs/tui.md) |
| Free mode, auto-commit, architecture | [docs/advanced.md](docs/advanced.md) |

---

## License

MIT — see [LICENSE](LICENSE).
