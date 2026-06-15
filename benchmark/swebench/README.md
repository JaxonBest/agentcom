# agentcom SWE-bench Lite harness

A small harness that runs **agentcom** (multi-agent fleet) and a **solo `claude -p`** session against the same SWE-bench Lite instances, captures their patches, and hands them to the official `swebench` evaluator for scoring.

The goal is to answer one question with hard numbers: **does an agentcom fleet produce more correct fixes per dollar than one careful Claude session on the same task?**

---

## What it does

For each SWE-bench Lite instance and each mode (`solo`, `fleet`):

1. Clone the upstream repo at the instance's `base_commit` into an isolated worktree.
2. Build a problem prompt from the issue text.
3. Run the mode in that worktree:
   - **solo** — `claude -p "<prompt>" --dangerously-skip-permissions --output-format json`
   - **fleet** — drop `fleet.toml` in as `agentcom.toml`, then `agentcom up --headless --restart --free "<prompt>" --for <T> --budget <B> --finish-tasks`. The harness polls the task board and stops the hub when the fleet has been idle (zero open tasks) for 60s, or when the wall-clock / budget cap fires.
4. Capture `git diff <base_commit>` as the patch. Capture cost (`agentcom cost --json` or the `claude` JSON result) and wall time.
5. Write per-mode `predictions-<mode>.jsonl` in the SWE-bench format.
6. (Optionally) invoke `python -m swebench.harness.run_evaluation` over each predictions file.
7. Render a markdown report comparing modes.

The harness is **resumable**: instance × mode pairs already recorded in `manifest.json` are skipped unless `--force` is set.

---

## Setup

```sh
pip install -r benchmark/swebench/requirements.txt
# Required CLIs on PATH:
#   - agentcom  (cargo install --path .)
#   - claude    (claude code CLI)
# Required env:
#   - ANTHROPIC_API_KEY     for both solo and fleet modes
#   - HF_TOKEN              only if your HF account is gated on the dataset
# For scoring you also need Docker running (the swebench harness uses it).
```

---

## Run

Smoke test the plumbing without spending money:

```sh
python benchmark/swebench/bench.py run \
  --instances 1 --modes solo,fleet --dry-run \
  --out benchmark/swebench/runs/smoke
```

A real 2-instance comparison (~$5–$20 and ~30–60 min depending on instance):

```sh
python benchmark/swebench/bench.py run \
  --instances 2 --modes solo,fleet \
  --timeout 1800 --budget 3.0 \
  --out benchmark/swebench/runs/$(date +%Y%m%d-%H%M)
```

Target specific instances:

```sh
python benchmark/swebench/bench.py run \
  --ids django__django-11099,sympy__sympy-13031 \
  --modes fleet
```

---

## Score

The harness writes `predictions-solo.jsonl` and `predictions-fleet.jsonl`. Score them with the official evaluator (requires Docker):

```sh
python benchmark/swebench/bench.py score \
  --run-dir benchmark/swebench/runs/20260615-1830 \
  --workers 1
```

Equivalent to running, for each predictions file:

```sh
python -m swebench.harness.run_evaluation \
  --predictions_path predictions-fleet.jsonl \
  --max_workers 1 \
  --run_id agentcom-fleet-<ts> \
  --dataset_name princeton-nlp/SWE-bench_Lite
```

The eval writes per-run JSON files into the working directory; copy them next to `manifest.json` so `report` can pick them up.

---

## Report

```sh
python benchmark/swebench/bench.py report \
  --run-dir benchmark/swebench/runs/20260615-1830
```

Renders `report.md` with a per-instance table (resolved? cost? wall?) and totals per mode.

---

## What lives where

```
benchmark/swebench/
├── README.md          # this file
├── requirements.txt   # datasets + swebench
├── fleet.toml         # agentcom.toml dropped into each instance for fleet mode
├── bench.py           # the harness — run / score / report
└── runs/<dir>/
    ├── manifest.json
    ├── predictions-solo.jsonl
    ├── predictions-fleet.jsonl
    ├── report.md
    └── <instance_id>/<mode>/
        ├── prompt.txt
        ├── patch.diff
        ├── meta.json
        ├── run.log
        └── repo/            # cloned worktree (kept for debugging)
```

---

## Honest caveats

- Each real run touches the network (clones a repo, calls Anthropic). Budget caps in `--budget` are advisory — `claude -p` doesn't expose a hard cost ceiling, so solo mode is bounded by `--timeout` only.
- The fleet's "idle for 60s" quiesce signal is heuristic. The composer may briefly clear the board while still planning the next task. Tune `FLEET_QUIESCE_SECS` in `bench.py` if you see premature stops.
- Patch capture is `git diff <base_commit>` over the worktree the agent edited. If an agent commits inside the worktree the diff still reflects the final tree, which is what swebench wants.
- The fleet mode uses *agentcom's own* `auto_commit` behavior; if you want a no-commit run, set `auto_commit = false` in `fleet.toml`.
- This compares **agentcom-fleet vs solo Claude with the same underlying model**. It does not control for prompt engineering of the solo run; if you want a fairer baseline, edit `_build_prompt` and add `--append-system-prompt` to the solo invocation.
