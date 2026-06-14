//! The autonomy loop: decides what (if anything) to feed an agent whenever
//! its turn ends or new work arrives. The hub drives the loop — idle agents
//! consume zero tokens and are woken here when messages or tasks appear.

use super::events::UiEvent;
use super::Hub;
use crate::agent::AgentState;
use crate::protocol::input::user_message;
use std::time::{Duration, Instant};

impl Hub {
    /// Feed an agent its next prompt if it's idle and there is work; route
    /// urgent messages to interrupt handling when it's mid-turn.
    pub(super) fn try_feed(&mut self, name: &str) {
        let Some(rt) = self.agents.get(name) else {
            return;
        };
        if rt.state != AgentState::Idle || self.shutting_down {
            return;
        }

        // Per-agent budget gate.
        if let Some(max) = rt.cfg.max_budget_usd {
            if rt.spent_usd >= max {
                let rt = self.agents.get_mut(name).expect("agent exists");
                rt.state = AgentState::Paused;
                rt.state_detail = Some(format!("budget ${max:.2} exhausted"));
                self.emit_state(name);
                self.log(format!("{name}: paused — budget exhausted"));
                return;
            }
        }

        // Per-agent RPM gate: sliding 60-second window of prompt sends.
        if let Some(max_rpm) = rt.cfg.max_rpm {
            let now = Instant::now();
            let window = self.rpm_window.entry(name.to_string()).or_default();
            window.retain(|&t| now.duration_since(t) < Duration::from_secs(60));
            if window.len() >= max_rpm as usize {
                let cooldown = window
                    .front()
                    .map(|&t| {
                        60u64.saturating_sub(now.duration_since(t).as_secs())
                    })
                    .unwrap_or(0);
                let rt = self.agents.get_mut(name).expect("agent exists");
                rt.state_detail = Some(format!(
                    "rate limited ({}/{} RPM) — {}s cooldown",
                    window.len(),
                    max_rpm,
                    cooldown
                ));
                self.emit_state(name);
                return;
            }
            window.push_back(now);
        }

        let inbox = self.store.msg_take_pending(name).unwrap_or_default();
        let claimed = self.store.claimed_task(name).ok().flatten();
        let suggested = if claimed.is_none() {
            // Don't offer a task that's in flight with someone else, or one
            // this agent already declined.
            let mut exclude: Vec<i64> = self
                .suggested
                .iter()
                .filter(|(agent, _)| agent.as_str() != name)
                .map(|(_, id)| *id)
                .collect();
            if let Some(declined) = self.declined.get(name) {
                exclude.extend(declined.iter().copied());
            }
            self.store.next_claimable(&exclude).ok().flatten()
        } else {
            None
        };

        let Some(prompt) = crate::prompt::turn_prompt(&inbox, claimed.as_ref(), suggested.as_ref())
        else {
            let rt = self.agents.get_mut(name).expect("agent exists");
            rt.state_detail = Some("waiting for work".into());
            self.emit_state(name);
            return;
        };

        if let Some(t) = &suggested {
            self.suggested.insert(name.to_string(), t.id);
        }
        if !inbox.is_empty() {
            let _ = self.ui_tx.send(UiEvent::MessagesChanged);
        }

        let rt = self.agents.get_mut(name).expect("agent exists");
        rt.write_line(user_message(&prompt));
        rt.state = AgentState::Working;
        rt.state_detail = claimed
            .as_ref()
            .map(|t| format!("task #{} {}", t.id, t.title));
        self.emit_state(name);
        tracing::debug!(agent = %name, "fed turn prompt");
    }

    /// Wake every idle agent (new task added, dependency completed, ...).
    pub(super) fn wake_idle(&mut self) {
        let idle: Vec<String> = self
            .agents
            .iter()
            .filter(|(_, rt)| rt.state == AgentState::Idle)
            .map(|(n, _)| n.clone())
            .collect();
        for name in idle {
            self.try_feed(&name);
        }
    }
}
