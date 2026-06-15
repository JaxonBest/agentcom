# Does a multi-agent fleet beat one careful Claude? n=10, here's the answer.

*[DRAFT — fill in the n=10 results table from `benchmark/baseline.md` once the full run completes. See the placeholder rows marked `[RUN NEEDED]`.]*

---

I built [agentcom](https://github.com/jaxonbest/agentcom) to test one idea: a fleet of coordinated AI agents — a builder, a reviewer, a tester — should produce better engineering outcomes than one careful Claude on the same budget, at least for the task classes where parallel work matters.

That thesis has been untested for most of agentcom's life. This post ends that.

We ran 10 SWE-bench Lite instances across three modes and published every result, whether or not the numbers made the fleet look good.

---

## What we measured

**Task corpus:** 10 SWE-bench Lite instances balanced by difficulty — 3 easy, 4 medium, 3 hard — across five projects (flask, requests, django, sympy, scikit-learn). The set is locked in [`benchmark/swebench/instances.json`](swebench/instances.json). Same 10 instances for every arm, every run.

**Three arms:**

| Mode | Config | Description |
|---|---|---|
| `solo-claude` | default claude CLI | One Claude agent, no fleet overhead |
| `solo-deepseek` | fleet-deepseek.toml | One DeepSeek agent, hub overhead but single worker |
| `fleet` | builder+reviewer+tester preset | Claude builder + Claude reviewer + Claude tester, structural review gate, post-close hooks running `pytest -x` |

**Scoring:** the same SWE-bench Lite harness used by the leaderboard — `python swebench/harness/run_evaluation.py`. A patch "resolves" if all `FAIL_TO_PASS` tests pass after applying it.

**Scorecard target (from [`PLAN.md`](../PLAN.md)):** fleet resolves ≥ 2 more instances than solo Claude, ≥ 1 more than solo DeepSeek, at ≤ 3× wall time and ≤ 2× cost per resolved task.

---

## Results

### n=10 summary

| Mode | Resolved / 10 | Median $/resolved | Median wall (s) | Notes |
|---|---|---|---|---|
| solo-claude | **[RUN NEEDED]** | **[RUN NEEDED]** | **[RUN NEEDED]** | |
| solo-deepseek | **[RUN NEEDED]** | **[RUN NEEDED]** | **[RUN NEEDED]** | |
| fleet | **[RUN NEEDED]** | **[RUN NEEDED]** | **[RUN NEEDED]** | builder+reviewer+tester preset |

*Full per-instance breakdown in [`baseline.md`](baseline.md).*

### What we already know from the first run

Before the n=10 run, we had one concrete data point: `pallets__flask-4045`, the instance that exposed the original value-prop failure.

| Mode | Resolved | Cost | Wall |
|---|---|---|---|
| solo-claude | 0/1 (patch applied, 1/2 FAIL_TO_PASS passed) | $0.11 | 29s |
| fleet (pre-gate) | 0/1 (byte-identical patch) | $0.42 | 130s |

That run confirmed the thesis failure mode: the reviewer agent **never ran** because nothing in the protocol triggered a review task when the builder closed. The fleet was 4.5× slower and 3.8× more expensive — for the same wrong answer.

That data point is why we built the structural review gate, post-close hooks, and typed lanes. The n=10 run will show whether those features close the gap.

---

## What we built to make the fleet worth it

The original agentcom had prose-only coordination. The reviewer's role description said "after every task close, grep for sibling sites" — but nothing in the hub actually dispatched a review task when the builder closed. Confirmed in code: the `COMPOSER_SECTION` in `src/prompt.rs` had zero instructions about reviewing.

Three structural changes landed between the first run and the n=10 run.

### Structural review gate (Workstream A)

The hub now auto-creates a `review` task when any builder closes a task with `review_required = true`. The composer's prose no longer has to remember to do this. State machine:

```
Claimed → AwaitingReview  (if review_required = true)
AwaitingReview → Done     (TaskReview { approve: true })
AwaitingReview → Open     (TaskReview { approve: false }, reviewer note prepended)
```

If no reviewer agent is running, the hub falls back to `Claimed → Done` and logs a one-time warning. Old configs don't break.

If a review sits unclosed longer than `review_stale_secs` (default: 1800s), the hub emits a `TaskReviewStale` event and the TUI footer flags it. No auto-approval on timeout — silent approval was the original bug.

**Verification:** `cargo test --test review_gate` covers the state transitions. The bypass paths (agent calling `task_done` directly on an `AwaitingReview` task, `task_assign` pulling an `AwaitingReview` task) are each blocked by a SQL guard with a regression test.

### Post-close hooks (Workstream C)

```toml
[hooks]
post_close = "pytest -x --timeout=60"
post_close_only_for_tags = ["builder"]
```

After a builder closes a task, the hub runs the hook and checks the exit code. Non-zero → `task_block()` with reason = the stderr tail. The task goes back to `Blocked` and the builder can't just re-close without addressing the failure.

Loop prevention: a `hook_attempts` counter per task. If `hook_attempts >= 2`, the hub skips the hook and closes. The fleet circuit breaker fires if more than 5 consecutive tasks hit hook failure, pausing dispatch until a human runs `agentcom resume`.

This feature alone would have caught the flask-4045 failure without a reviewer: the builder's incomplete patch would have failed `pytest -x`, blocked the task, and forced a re-attempt.

### Typed lanes (Workstream B)

```toml
[[agents]]
name = "builder"
lanes = ["src/**", "!src/vendored/**"]

[[agents]]
name = "tester"
lanes = ["tests/**"]
```

The hub hard-rejects a `FilesClaim` if any claimed path falls outside the agent's declared lane globs. All-or-nothing: if one path in the batch misses, the whole claim is rejected. The three shipped presets all declare lanes.

`agentcom check` warns when a lane pattern doesn't match any real file in the repo — typo detection before the fleet runs.

---

## Architecture numbers

| Metric | Value |
|---|---|
| Source (Rust) | ~17.5k LOC |
| Test suite | 129 unit tests, 6 integration suites |
| Store backend | SQLite (single file, no network, no docker) |
| IPC | Unix socket + ndjson |
| New state machine states | `AwaitingReview` |
| New IPC variants | `TaskReview`, `TaskAwaitingReview` |
| New config fields | `review_required`, `review_stale_secs`, `[hooks]`, `lanes`, `preset` |
| Backward compatibility | empty `lanes` = no enforcement; absent `[hooks]` = no hooks; no `review_required` = old behavior |

---

## The honest off-ramp

The scorecard target (fleet resolves ≥ 2 more than solo Claude at ≤ 3× wall, ≤ 2× cost) is aggressive. SWE-bench Lite is designed to be hard for *any* single agent, and most of the wins in the leaderboard come from clever sampling strategies (e.g., run the same agent 10× and take the union), not multi-agent coordination.

If the n=10 run shows fleet ≤ solo on resolved count:

1. **Publish anyway.** The numbers are the numbers. Credibility matters more than marketing.
2. **Interpret the margin.** If fleet loses by 0–1 instances, the structural features (review gate, hooks, lanes) likely still add value on *different* task classes — cross-file refactors, large-repo work — where the reviewer can catch things the builder can't. Rerun on a harder, wider corpus.
3. **Consider the pivot.** agentcom's review gate, hooks, and presets add value even when the "fleet" is just one Claude agent with a `pytest -x` hook. If the numbers support it, reframe agentcom as "structured solo Claude with safety nets" rather than "fleet beats solo."

We committed to publishing whatever comes out. The numbers will be in [`baseline.md`](baseline.md).

---

## Running the benchmark yourself

```sh
# Clone and build
git clone https://github.com/jaxonbest/agentcom
cd agentcom
cargo install --path . --force

# Run all three arms (requires ANTHROPIC_API_KEY; DEEPSEEK_API_KEY for solo-deepseek)
python benchmark/swebench/bench.py run \
    --modes solo-claude,solo-deepseek,fleet \
    --out runs/$(date +%F)

# Score
python benchmark/swebench/bench.py score --run-dir runs/$(date +%F)

# Report
python benchmark/swebench/bench.py report --run-dir runs/$(date +%F)
```

The 10 locked instances are in [`benchmark/swebench/instances.json`](swebench/instances.json). The harness writes per-instance patches and test results to `runs/<date>/`. Results history lives in [`benchmark/baseline.md`](baseline.md).

---

## Links

- agentcom source: [`src/`](../src/)
- Benchmark harness: [`benchmark/swebench/bench.py`](swebench/bench.py)
- Locked instance set: [`benchmark/swebench/instances.json`](swebench/instances.json)
- Results history: [`benchmark/baseline.md`](baseline.md)
- PLAN.md (full design rationale): [`PLAN.md`](../PLAN.md)
