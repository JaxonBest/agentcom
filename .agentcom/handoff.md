# agentcom — Session Handoff

**Goal (task #2):** Make agentcom production-ready — advanced setup, onboarding wizard, customization, clearer UI.

**Session ended:** mid-flight. Three agents were actively working when the session closed.

---

## What's Done

| Task | Agent | Result |
|------|-------|--------|
| #1 | builder | DeepSeek adapter upgraded: real regex, output_mode, context lines, replace_all — all tests pass |
| #5 | reviewer | TUI help overlay: `?` key shows two-column keybinding popup; footer hints updated |

---

## In Flight (pick up exactly here)

### builder — task #3 (in progress, UNCOMPLETED)
**Claim:** `agentcom task claim 3`

**What to do — `src/config.rs`:**

1. Replace the static `EXAMPLE_CONFIG: &str` constant with a function `example_config(template: Template, project_name: &str) -> String` that takes a `Template` enum and the project name (extracted from the cwd directory name).

2. Add this enum above the function:
```rust
pub enum Template { Solo, Team, Mixed }
```

3. The `Team` variant (default) should expand the current `EXAMPLE_CONFIG` with:
   - A top comment block explaining the workflow: init → edit → up → chat → done
   - Every field documented with type + default inline
   - A third `[[agent]]` block for a `deepseek` junior (commented out by default) showing provider/model options
   - The builder and reviewer blocks fully annotated with every optional field shown (commented out with defaults)

4. `Solo` = composer + builder only (no reviewer block).
5. `Mixed` = composer + builder + reviewer + (uncommented) deepseek junior.

6. Change `write_example(project_root, force)` signature to `write_example(project_root, force, template: Template)` — extract project name from `project_root.file_name()`.

7. Update the `example_config_parses_and_validates` test to call `write_example` with the new signature and verify all three templates parse.

8. **Build:** `cargo build -p agentcom --bin agentcom 2>&1` — must compile.

9. **Commit:** `git add src/config.rs && git commit -m "feat: richer init templates with per-template configs and cwd project name"`

10. Then mark done: `agentcom task done 3 --note "config.rs: Template enum, three template variants, cwd project name"`

---

### builder-assistant — task #4 (in progress, UNCOMPLETED)
**Files claimed:** `src/cli.rs`, `src/main.rs`

**What to do:**

**A. Doctor command in `src/cli.rs`:**

Add to the `Command` enum:
```rust
/// Run pre-flight checks: verify provider CLIs, API keys, and agentcom.toml
Doctor,
```

**B. Doctor handler in `src/main.rs`** (local, no IPC):

In the `match cli.command` block, add:
```rust
Command::Doctor => run_doctor().await,
```

Implement `async fn run_doctor() -> Result<()>` that:
1. Check `claude --version` (run via `std::process::Command`) → print `[OK] claude X.Y.Z` or `[!!] claude not found — install from https://claude.ai/download`
2. Check `codex --version` → print `[ok] codex X.Y.Z` or `[-] codex not found (optional — needed for Codex agents)`
3. Check env var `OPENAI_API_KEY` → `[ok] OPENAI_API_KEY set` or `[-] OPENAI_API_KEY not set (optional — needed for Codex)`
4. Check env var `DEEPSEEK_API_KEY` → `[ok] DEEPSEEK_API_KEY set` or `[-] DEEPSEEK_API_KEY not set (optional — needed for DeepSeek agents)`
5. Walk up from cwd to find `agentcom.toml`. If found: parse + validate it → `[OK] agentcom.toml valid (3 agents)` or `[!!] agentcom.toml error: <message>`. If not found: `[-] no agentcom.toml — run: agentcom init`
6. Print a blank line then a summary: `all required checks passed` or `X issue(s) found`

Use `ansi_term` or just raw ANSI codes (`\x1b[32m` green, `\x1b[33m` yellow, `\x1b[31m` red, `\x1b[0m` reset) since ansi_term may not be in Cargo.toml.

**C. `--template` flag for `agentcom init`** (since you own cli.rs):

In the `Init` variant of `Command`, add:
```rust
/// Starting template: solo (builder only), team (builder+reviewer, default), mixed (builder+reviewer+deepseek-junior)
#[arg(long, default_value = "team")]
template: String,
```

In `src/main.rs` init handler, pass `template` to `config::write_example()` — map "solo"/"team"/"mixed" to the `config::Template` enum.

**D. Build and commit:**
```
cargo build -p agentcom --bin agentcom 2>&1
git add src/cli.rs src/main.rs
git commit -m "feat: agentcom doctor command and --template flag for init"
agentcom task done 4 --note "doctor command: checks claude/codex/keys/config; --template flag added to init"
```

---

### composer — task #7 (claimed, NOT STARTED)
**File:** `src/cli.rs` — wait until builder-assistant releases it (task #4 done), then claim.

**What to do:**

1. **Friendly "hub not running" errors**: In `run_client()`, wrap the `Client::connect()` call. If it fails, instead of propagating the error, print:
   ```
   hub not running — start it with: agentcom up
   ```
   and return `Ok(())`. Apply to all client-mode subcommands except `Init`/`Up`/`Agent`/`Doctor`.

2. **`print_status()` improvements**: Add a header row, align columns consistently. Show free-mode goal on a separate indented line if active.

3. **`print_tasks()` summary line**: After the task list, print: `  X open · Y claimed · Z done · W blocked`

4. **`--help` long descriptions** for key subcommands:
   - `Up`: add example `agentcom up --free "keep improving tests" --for 2h --budget 5`
   - `Task add`: add example `agentcom task add "Fix auth bug" -p 0 -d "JWT tokens expire too early"`
   - `Agent add`: add example `agentcom agent add tester --role "Runs tests, reports failures" --provider claude`

5. **Build + commit:**
   ```
   cargo build -p agentcom --bin agentcom 2>&1
   git add src/cli.rs
   git commit -m "feat: friendly hub-not-running errors, richer status/task output, help examples"
   agentcom task done 7 --note "cli.rs: hub-not-running friendly errors, status header, task summary, help examples"
   ```

---

## Open Tasks (not started)

### task #6 — README overhaul (open, nobody claimed it)
**Agent:** builder or reviewer (whoever is free after their current task)
**File:** `README.md` only — no Rust needed.

Structure to write:
1. **Hero**: 2-sentence description + ASCII diagram of the fleet (human → composer → [builder, reviewer] → shared board)
2. **Prerequisites**: Rust toolchain, `claude` CLI, optional codex/DeepSeek key
3. **Install**: `cargo install --path . --force`, verify with `agentcom --version`, note the adapter binaries
4. **Quick Start**: 5 steps (init → edit config → up → chat → see results)
5. **Configuration Reference**: table of all `agentcom.toml` fields; table of all `[[agent]]` fields; complete annotated example
6. **Provider Setup**: Claude (login via `claude`), Codex (`OPENAI_API_KEY`), DeepSeek (`DEEPSEEK_API_KEY`, model recommendations)
7. **CLI Reference**: table of all commands grouped: hub management / task board / agent fleet / file claims / real-time control
8. **TUI Guide**: ASCII-art screenshot of the 5-tab TUI with callouts; keybinding table
9. **Free Mode**: explain `--free "goal" --for 2h --budget 5 --usage 80` with examples
10. **Troubleshooting**: "hub already running", "permission prompts", "agent crashed", "DEEPSEEK_API_KEY not set", "can't reinstall on Windows", "agents not picking up tasks"
11. **Architecture note**: hub / agents / store / IPC in 3–4 sentences

Commit: `git add README.md && git commit -m "docs: comprehensive README — onboarding, config reference, CLI guide, TUI guide"`

### task #8 — template CLI wire-up (open, deps: #3 and #4)
**Agent:** whoever finishes #3 or #4 last.

This is likely already handled by builder-assistant (task #4). If `agentcom init --template team` works after #3+#4 are done, close this task: `agentcom task done 8 --note "handled in task #4"`. If not, implement it.

---

## Sequence to Resume

```
agentcom up   # starts hub and TUI — agents auto-resume their claimed tasks
```

The board state is preserved in SQLite. Agents will see their claimed tasks and pending inbox messages on startup and pick up exactly where they left off.

**Remaining work order:**
1. builder (#3) and builder-assistant (#4) finish in parallel
2. composer takes #7 once builder-assistant releases cli.rs
3. builder or reviewer takes #6 (README)
4. reviewer does a final review pass over all changes
5. Close task #2 once all subtasks are done and build passes

---

## agent #2 master task

Task #2 is held by **composer**. When all subtasks (#3, #4, #5 ✅, #6, #7, #8) are complete and `cargo test` passes, run:

```
agentcom task done 2 --note "production-ready: TUI help overlay, doctor command, init templates, richer CLI output, comprehensive README"
```
