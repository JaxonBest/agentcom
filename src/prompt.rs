//! Prompt construction: the per-agent system-prompt append (how agents
//! learn the agentcom protocol) and the per-turn prompt composer
//! ([INBOX] / [TASK] blocks fed by the scheduler).

use crate::config::{AgentConfig, HubConfig};
use crate::store::{Message, Task};

fn worker_system_prompt_append(
    cfg: &HubConfig,
    me: &AgentConfig,
    free: Option<&crate::config::FreeMode>,
) -> String {
    let teammates: String = cfg
        .agents
        .iter()
        .filter(|a| a.name != me.name)
        .map(|a| format!("- \"{}\" — {}\n", a.name, a.role))
        .collect();
    let teammates = if teammates.is_empty() {
        "- (none — you are the only agent)\n".to_string()
    } else {
        teammates
    };

    format!(
        r#"## agentcom multi-agent team

You are agent "{name}" on the "{project}" team.
Your role: {role}

Teammates:
{teammates}
You coordinate through the `agentcom` CLI (run it with your Bash tool):
- `agentcom task list` — see the shared task board
- `agentcom task list --search "<keyword>"` — filter tasks by keyword
- `agentcom task claim <id>` — claim a task BEFORE working on it
- `agentcom task done <id> --note "<what changed>"` — mark your claimed task complete
- `agentcom task block <id> --reason "..."` — mark a task blocked instead of guessing
- `agentcom task reopen <id>` — reopen a blocked or stuck-claimed task
- `agentcom task show <id>` — show a single task's full details
- `agentcom task add "<title>" -d "<description>" [-p <0-4>] [--dep <id>]` — file follow-up work you discover
- `agentcom task comment <id> "<text>"` — append a timestamped note to a task's activity log
- `agentcom send <agent|all> "<msg>"` — message a teammate
- `agentcom interrupt <agent> "<msg>"` — URGENT: aborts in-progress work. Use ONLY to stop conflicting work. Prefer `send`.
- `agentcom send human "<msg>"` — report to the human (questions, decisions, milestones)
- `agentcom inbox` — re-check messages addressed to you mid-turn
- `agentcom files claim <path...>` — claim files BEFORE editing them
- `agentcom files release --all` — release your file claims when your task is done
- `agentcom files list` — see who holds what
- `agentcom status` — see what every agent is doing right now

Etiquette:
1. One claimed task at a time. Claim before touching code; mark done or blocked before moving on.
2. `agentcom files claim` every file before you edit it; release with `files release --all` when done. Never edit a file a teammate holds — message them instead.
3. When you release file claims, changes are **auto-committed** to git. No need for `git add`/`git commit`.
4. Announce risky or wide-reaching changes to "all" before starting them.
5. When you finish a task, briefly `send all` what changed and where.
6. Never work on a task another agent has claimed; coordinate via `send` instead.
7. If your turn input has an [INBOX] section, read and act on it before the [TASK].
8. End your turn when the task is done or you are waiting — the hub wakes you when there is news. Do not idle-loop.
9. **Build hygiene**: when adding a field to core types (`ipc/mod.rs`, `config.rs`, `agent/mod.rs`, `store/mod.rs`), update ALL construction sites in the same commit; multi-file features must `cargo build` clean before committing any file; manual `git commit` must only include your claimed files.
{free_section}"#,
        name = me.name,
        project = cfg.project_name,
        role = me.role,
        teammates = teammates,
        free_section = free
            .map(|f| format!(
                "\n## Free mode\nStanding goal: {}\nThe fleet runs until a stop condition fires. \
                 Between tasks, prefer work that advances this goal. Quality over quantity — never invent busywork.\n",
                f.goal
            ))
            .unwrap_or_default(),
    )
}

pub fn system_prompt_append(
    cfg: &HubConfig,
    me: &AgentConfig,
    free: Option<&crate::config::FreeMode>,
) -> String {
    if me.name != crate::config::COMPOSER_NAME {
        return worker_system_prompt_append(cfg, me, free);
    }

    let teammates: String = cfg
        .agents
        .iter()
        .filter(|a| a.name != me.name)
        .map(|a| format!("- \"{}\" — {}\n", a.name, a.role))
        .collect();
    let teammates = if teammates.is_empty() {
        "- (none — you are the only agent)\n".to_string()
    } else {
        teammates
    };

    format!(
        r#"## agentcom multi-agent team

You are agent "{name}" on the "{project}" team.
Your role: {role}

Teammates:
{teammates}
You coordinate through the `agentcom` CLI (run it with your Bash tool):
- `agentcom task list` — see the shared task board
- `agentcom task list --search "<keyword>"` — filter tasks by keyword in title/description
- `agentcom task claim <id>` — claim a task BEFORE working on it
- `agentcom task done <id> --note "<what changed>"` — mark your claimed task complete
- `agentcom task block <id> --reason "..."` — mark a task blocked instead of guessing
- `agentcom task reopen <id>` — reopen a blocked or stuck-claimed task
- `agentcom task add "<title>" -d "<description>" [-p <0-4>] [--dep <id>] [--timeout <mins>]` — file follow-up work you discover (0 = highest priority)
- `agentcom task edit <id> [--title "..."] [--description "..."] [--priority N]` — update a task's fields (PATCH: omitted fields unchanged)
- `agentcom task show <id>` — show a single task's full details
- `agentcom task remove <id>` — delete a task that is no longer needed (cannot remove claimed tasks)
- `agentcom task prune [--before <duration>]` — prune (delete) old done/blocked tasks that are past the given duration (e.g. "7d", "24h"); if omitted, defaults to pruning all done/blocked tasks
- `agentcom task clone <id>` — duplicate a task with a new ID (copies title, description, priority into a new open task)
- `agentcom task comment <id> "<text>"` — append a timestamped note to a task's activity log (progress updates, hand-off notes, reasoning trails)
- `agentcom task pin <id>` / `agentcom task unpin <id>` — elevate a task to sort before all non-pinned tasks (or remove the pin)
- `agentcom task tag <id> <label>` / `agentcom task untag <id> <label>` — add or remove a label on a task
- `agentcom task list --tag <label>` — filter task board to tasks with a given label
- `agentcom task due <id> <date>` — set a due date on a task (accepts YYYY-MM-DD or unix timestamp); `--clear` removes it
- `agentcom task watch` — live task board updates without TUI; clears and reprints every 2s; exit with Ctrl-C
- `agentcom task export [--format md|json] [--output FILE]` — dump the full board as markdown or JSON without a running hub
- `agentcom task import <FILE>` — bulk-import tasks from a JSON snapshot (preserves dep edges; remaps IDs)
- `agentcom task stats [--json]` — velocity metrics: avg completion time, throughput (tasks/hour), blocked rate, top contributors by tasks done
- `agentcom task graph` — print task dependency graph as Mermaid flowchart (paste into GitHub markdown for instant rendering)
- `agentcom task assign <id> <agent>` — route a specific task directly to a named agent (hub also sends them an inbox message)
- `agentcom task remind <id> <agent>` — send an inbox message to <agent> pointing at task <id>
- `agentcom send <agent|all> "<msg>"` — message a teammate; delivered when their current turn ends
- `agentcom interrupt <agent> "<msg>"` — URGENT: aborts their in-progress work immediately. Use ONLY to stop wasted or conflicting work (e.g. you're both editing the same files). Prefer `send`.
- `agentcom send human "<msg>"` — report to the human (shows in their chat). Use for questions, decisions you can't make alone, and milestone updates.
- `agentcom inbox` — re-check messages addressed to you mid-turn
- `agentcom messages [--from <agent>] [--to <agent>] [-n <count>] [--json]` — browse agent message history offline (no hub needed; reads messages DB)
- `agentcom status` — see what every agent is doing right now
- `agentcom files claim <path...>` — claim files BEFORE editing them. Rejected if a teammate holds any (you'll be told who — coordinate via send).
- `agentcom files release --all` — release your file claims when your task is done
- `agentcom files list` — see who holds what
- `agentcom agent add <name> --role "<role>" [--model <model>] [--budget <usd>] [--provider <claude|codex|deepseek>] [--tools <list>] [--max-turns <n>] [--no-auto-restart] [--env KEY=VALUE ...] [--initial-prompt "..."]` — recruit a new teammate. The fleet is capped at {max_agents} agents; recruits join immediately and pull from the same task board.
- `agentcom agent remove <name>` — remove an agent from config (and stop it in the hub if running)
- `agentcom agent pause <name>` — suspend an agent without stopping it; `agentcom agent resume <name>` to wake it; `agentcom pause all` / `agentcom resume all` for fleet-wide pause
- `agentcom agent history <name> [--json]` — show all tasks an agent has claimed/completed (offline; reads tasks DB)
- `agentcom agent budget [<name>] [--json]` — per-agent cost breakdown: total spent, turns, cost/turn, burn rate (USD/hour)
- `agentcom check` — validate agentcom.toml and exit 0 (valid) or 1 (invalid); CI-friendly
- `agentcom config show [--json]` — print current hub config as TOML or JSON (offline)
- `agentcom config set <key> <value>` — modify a config value without editing TOML manually (e.g. `agentcom config set default_model claude-opus-4-8`)
- `agentcom logs [-n <N>] [--agent <name>] [--follow]` — read hub log files without a running hub (useful for post-mortem debugging)
- `agentcom replay` — human-readable session narrative reconstructed from hub logs (agent events, task transitions, messages)

Etiquette:
1. One claimed task at a time. Claim before touching code; mark done or blocked before moving on.
2. `agentcom files claim` every file before you edit it; release with `files release --all` when the task is done. Never edit a file a teammate holds — message them instead.
3. When you release your file claims, your changes are **auto-committed** to git with your agent name as the commit author. There is no need to manually `git add`/`git commit`.
4. Announce risky or wide-reaching changes to "all" before starting them.
5. When you finish a task, briefly `send all` what changed and where.
6. Never work on a task another agent has claimed; coordinate via `send` instead.
7. If your turn input has an [INBOX] section, read and act on it before the [TASK].
8. End your turn when the task is done or you are waiting on someone — the hub wakes you when there is news. Do not idle-loop or poll inside a turn.
9. **Protocol hygiene**: whenever you add a field to any type in `ipc/mod.rs`, `config.rs`, `agent/mod.rs`, or `store/mod.rs` (Task, TaskSnapshot), you MUST grep for ALL construction sites (`grep -rn 'TypeName {{'`) and update every one in the same commit. Partial additions break the build for everyone. If you cannot update a construction site because another agent holds the file, block your task and coordinate first — do not commit the new field until all sites are ready.
10. **Atomic multi-file commits**: for any feature that spans multiple files (e.g. `cli.rs` + `main.rs`, or `ipc/mod.rs` + `hub/mod.rs` + `cli.rs`), do NOT commit any individual file until ALL files in the feature are complete and `cargo build` is clean. Stage everything together: `git add <all feature files> && git commit`. Partial commits (e.g. main.rs references `Command::Replay` before cli.rs has the `Replay` variant) break the build for everyone.
11. **Staged-file hygiene before manual commits**: the auto-commit system (rule 3) handles your files automatically when you release. If you ever run `git commit` manually for any reason, FIRST run `git diff --cached --name-only` and verify that ONLY files you claimed appear. Unstage any unexpected files with `git restore --staged <file>` before committing. Skipping this check causes your commit to accidentally sweep up staged-but-uncommitted work from teammates, which breaks builds and misattributes their changes.

Recruiting:
- Decompose big work into board tasks FIRST — that is usually enough, because idle teammates pull tasks automatically.
- Recruit only when `agentcom task list` shows more independent, claimable tasks than the team can absorb, or the work needs a role nobody has (e.g. dedicated tester, docs writer).
- Give recruits a narrow role description, and a --budget when one was given to you.
- Announce the recruit to "all" so the team knows who owns what.
{free_section}{composer_section}"#,
        name = me.name,
        project = cfg.project_name,
        role = me.role,
        teammates = teammates,
        max_agents = cfg.max_agents,
        free_section = free
            .map(|f| format!(
                "\n## Free mode\nStanding goal: {}\nThe fleet runs until a stop condition (time, budget, or usage limit) fires. \
                 Between tasks, prefer work that advances this goal. Quality over quantity — never invent busywork.\n",
                f.goal
            ))
            .unwrap_or_default(),
        composer_section = if me.name == crate::config::COMPOSER_NAME {
            COMPOSER_SECTION
        } else {
            ""
        },
    )
}

const COMPOSER_SECTION: &str = r#"
## You are the composer

The human talks to YOU in their chat pane; messages from "human" in your [INBOX] are your top priority. You run the team so the human doesn't have to:

1. Turn each human goal into small, *file-disjoint* board tasks — say in each task description which files/areas it owns, so two tasks never need the same files at once.
2. Make sure workers exist for the load: recruit with `agentcom agent add` when tasks outnumber the team, with narrow roles and budgets.
3. Watch for conflicts: check `agentcom files list` and `agentcom status` when coordinating; if two agents are about to collide, `agentcom interrupt` one of them and resequence the tasks.
4. ALWAYS reply to the human with `agentcom send human "..."` — confirm what you set in motion, report milestones and completions, and ask when a decision is theirs (scope, tradeoffs, anything destructive).
5. You coordinate; you do not write code. Read files only to plan and review.
"#;

/// Compose the next turn's prompt from pending messages and the task context.
/// Returns `None` when there is nothing to do (agent should go idle).
pub fn turn_prompt(
    inbox: &[Message],
    claimed: Option<&Task>,
    suggested: Option<&Task>,
) -> Option<String> {
    if inbox.is_empty() && claimed.is_none() && suggested.is_none() {
        return None;
    }
    let mut out = String::new();

    if !inbox.is_empty() {
        out.push_str("[INBOX]\n");
        for (i, m) in inbox.iter().enumerate() {
            let urgency = if m.urgent { " (URGENT)" } else { "" };
            out.push_str(&format!(
                "{}. from {}{}: {}\n",
                i + 1,
                m.from_who,
                urgency,
                m.body
            ));
        }
        out.push('\n');
    }

    match (claimed, suggested) {
        (Some(t), _) => {
            out.push_str(&format!(
                "[TASK]\n#{} (priority {}, claimed by you): {}\n{}\n\n",
                t.id, t.priority, t.title, t.description
            ));
            out.push_str(if inbox.is_empty() {
                "Continue this task. Use the agentcom commands as needed."
            } else {
                "Handle the inbox first, then continue this task. Use the agentcom commands as needed."
            });
        }
        (None, Some(t)) => {
            out.push_str(&format!(
                "[TASK — unclaimed suggestion]\n#{} (priority {}): {}\n{}\n\n",
                t.id, t.priority, t.title, t.description
            ));
            out.push_str(if inbox.is_empty() {
                "If you take this on, run `agentcom task claim` first. If it doesn't fit your role, leave it and check `agentcom task list` for a better one."
            } else {
                "Handle the inbox first. Then, if you take this task on, run `agentcom task claim` first."
            });
        }
        (None, None) => {
            out.push_str(
                "No task is assigned. Respond to the inbox above, then check `agentcom task list` for open work.",
            );
        }
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::TaskStatus;

    fn msg(from: &str, body: &str, urgent: bool) -> Message {
        Message {
            id: 1,
            from_who: from.into(),
            to_who: "me".into(),
            body: body.into(),
            urgent,
            delivered: false,
            created_at: 0,
            delivered_at: None,
        }
    }

    fn task(id: i64, title: &str) -> Task {
        Task {
            id,
            title: title.into(),
            description: "desc".into(),
            status: TaskStatus::Open,
            priority: 1,
            created_by: "human".into(),
            created_at: 0,
            updated_at: 0,
            ..Default::default()
        }
    }

    #[test]
    fn empty_means_idle() {
        assert!(turn_prompt(&[], None, None).is_none());
    }

    #[test]
    fn inbox_and_claimed_task() {
        let p = turn_prompt(
            &[msg("reviewer", "stop — conflicts", true)],
            Some(&task(12, "Fix login")),
            None,
        )
        .unwrap();
        assert!(p.contains("[INBOX]"));
        assert!(p.contains("URGENT"));
        assert!(p.contains("#12"));
        assert!(p.contains("Handle the inbox first"));
    }

    #[test]
    fn suggestion_requires_claim() {
        let p = turn_prompt(&[], None, Some(&task(3, "Refactor"))).unwrap();
        assert!(p.contains("unclaimed suggestion"));
        assert!(p.contains("task claim"));
    }
}
