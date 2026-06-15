# TUI Guide

`agentcom up` opens a full-terminal dashboard.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ agentcom - my-project  | $0.18 total | claude $0.12/4t | codex $0.06/2t      │  ← header: cost/usage
├────────────────┬─────────────────────────────────────────────────────────────┤
│ agents         │ 1 Chat  2 Output  3 Tasks  4 Messages  5 Hub Log            │  ← tab bar
│                ├─────────────────────────────────────────────────────────────┤
│ > | composer   │                                                             │
│   [claude]     │  composer: I've created 3 tasks. Builder is working on     │
│   . builder    │  the validation logic now.                                  │  ← chat panel
│   [claude]     │                                                             │
│   > reviewer   ├─────────────────────────────────────────────────────────────┤
│   [codex]      │ > working on task #2...           ← activity/agent status  │
│                │ - builder claimed src/auth.rs                               │
│                │ - reviewer filed task #4                                    │
├────────────────┴─────────────────────────────────────────────────────────────┤
│ > type message here|   (Enter send - Tab panes - ? help - Ctrl+C quit)      │  ← footer/input
└──────────────────────────────────────────────────────────────────────────────┘
  ↑ sidebar                                                          ↑ flash/hints
```

## Panels

- **Sidebar** — agent list with live state glyph (`>` working, `.` idle, `||` paused, `x` crashed) and provider badge
- **Chat** — your conversation with the composer; unread questions shown in yellow
- **Output** — live output stream for the selected agent; scroll with PgUp/PgDn
- **Tasks** — the shared board with status, priority, and assignee; board title shows open/wip/done/blocked counts. Press `/` to filter by keyword, `d` to hide done tasks, `Enter` on a row to open a full-screen detail popup
- **Messages** — full inter-agent and human message feed
- **Hub Log** — hub-level events (starts, stops, crashes, recruits)

## Keybindings

| Key | Action |
|---|---|
| `Tab` / `1`–`5` | Switch tabs |
| `Up` / `Down` / `j` / `k` | Select agent in sidebar (non-Tasks tabs) or navigate task list (Tasks tab) |
| `Enter` | Send chat message (Chat tab) or open task detail popup (Tasks tab) |
| `/` | Open task filter (Tasks tab) — type to search, Enter to apply, empty to clear |
| `F` | Clear task filter immediately |
| `d` | Toggle hiding done tasks (Tasks tab) |
| `m` | Message selected agent |
| `u` | Interrupt selected agent (urgent) |
| `M` | Broadcast message to all agents |
| `a` | Add a task directly to the board |
| `p` | Pause / resume selected agent |
| `s` | Stop selected agent |
| `PgUp` / `PgDn` | Scroll agent output |
| `End` | Jump to live output (stop scrolling) |
| `Esc` | Close task detail popup / clear chat input / cancel modal |
| `?` | Toggle this keybinding help overlay |
| `q` / `Ctrl+C` | Quit (prompts for confirmation) |
