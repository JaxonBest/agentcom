//! Message sending and the interrupt state machine.
//!
//! Urgent delivery sequence:
//! 1. persist the message (single source of truth — delivery always reads
//!    the store, so nothing is lost across crashes/restarts)
//! 2. send `control_request{interrupt}` on the target's stdin
//! 3. the aborted turn's `result` event is the true barrier — `handle_result`
//!    sees state `Interrupting` and immediately feeds, which picks the urgent
//!    message up from the store
//! 4. if no result arrives within `interrupt_timeout_secs`, the tick handler
//!    tree-kills the child; the exit handler restarts it with `--resume` and
//!    the message is delivered as the first prompt of the resumed session.

use super::Hub;
use crate::agent::AgentState;
use crate::ipc::{is_human_identity, Response};
use crate::store::transcript::EventKind;
use std::time::{Duration, Instant};

impl Hub {
    pub(super) fn do_send(&mut self, from: &str, to: &str, body: &str, urgent: bool) -> Response {
        // Persist conversational traffic to the durable transcript at the
        // authoritative send point (so it survives a hub restart and is
        // consistent with the live MessagesChanged broadcast).
        if is_human_identity(from) {
            // The human typed to the composer (or directly to an agent).
            self.record(EventKind::HumanMsg, "human", body, None, Some(from));
        } else if to == "human" {
            let kind = if from == crate::config::COMPOSER_NAME {
                EventKind::ComposerMsg
            } else {
                EventKind::AgentLine
            };
            self.record(kind, from, body, None, None);
        }

        // Agents report to the human through the same message system; those
        // messages land in the TUI chat / `agentcom inbox`.
        if to == "human" {
            if let Err(e) = self
                .store
                .msg_send(from, &["human".to_string()], body, urgent)
            {
                return Response::err(e.to_string());
            }
            let _ = self.ui_tx.send(super::events::UiEvent::MessagesChanged);
            return Response::ok_msg("delivered to the human's inbox");
        }

        let recipients: Vec<String> = if to == "all" {
            self.agents
                .keys()
                .filter(|n| n.as_str() != from)
                .cloned()
                .collect()
        } else if self.agents.contains_key(to) {
            vec![to.to_string()]
        } else {
            return Response::err(format!(
                "unknown recipient {to:?} (agents: {}, \"all\", or \"human\")",
                self.agent_names().join(", ")
            ));
        };
        if recipients.is_empty() {
            return Response::err("no recipients (you are the only agent)");
        }

        if let Err(e) = self.store.msg_send(from, &recipients, body, urgent) {
            return Response::err(e.to_string());
        }
        let _ = self.ui_tx.send(super::events::UiEvent::MessagesChanged);

        let mut notes = Vec::new();
        for r in recipients {
            let state = self
                .agents
                .get(&r)
                .map(|rt| rt.state.clone())
                .unwrap_or(AgentState::Stopped);
            match state {
                AgentState::Idle => {
                    self.try_feed(&r);
                    notes.push(format!("{r}: delivered"));
                }
                AgentState::Working if urgent => {
                    self.start_interrupt(&r);
                    notes.push(format!("{r}: interrupting"));
                }
                AgentState::Working | AgentState::Interrupting => {
                    notes.push(format!("{r}: queued (delivered when its turn ends)"));
                }
                AgentState::Paused => notes.push(format!("{r}: queued (agent is paused)")),
                AgentState::Crashed | AgentState::Stopped => {
                    notes.push(format!("{r}: queued (agent is {})", state.as_str()))
                }
            }
        }
        Response::ok_msg(notes.join("; "))
    }

    pub(super) fn start_interrupt(&mut self, name: &str) {
        let timeout = Duration::from_secs(self.cfg.interrupt_timeout_secs);
        let Some(rt) = self.agents.get_mut(name) else {
            return;
        };
        if rt.state != AgentState::Working {
            return;
        }
        let request_id = format!("agentcom-int-{}", uuid::Uuid::new_v4());
        rt.write_line(crate::protocol::control::interrupt_request(&request_id));
        rt.state = AgentState::Interrupting;
        rt.state_detail = Some("interrupt requested".into());
        rt.interrupt_deadline = Some(Instant::now() + timeout);
        rt.pending_urgent = true;
        self.emit_state(name);
        self.log(format!("{name}: interrupt requested"));
    }

    fn agent_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.agents.keys().cloned().collect();
        v.sort();
        v
    }
}
