#!/usr/bin/env python3
"""
agentcom SWE-bench Lite harness.

Compares agentcom (fleet) against a solo `claude -p` session on the same
SWE-bench Lite instance. Captures the model_patch, cost, and wall time.
Scoring is delegated to the official `swebench` harness.

Usage:
  bench.py run --instances 5 --modes solo,fleet --out runs/2026-06-15
  bench.py run --ids django__django-11099,sympy__sympy-13031 --modes fleet
  bench.py score --run-dir runs/2026-06-15
  bench.py report --run-dir runs/2026-06-15
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Iterable, Optional

HERE = Path(__file__).resolve().parent
FLEET_TOML = HERE / "fleet.toml"
DATASET = "princeton-nlp/SWE-bench_Lite"
LOCKED_INSTANCES_FILE = HERE / "instances.json"

# How long we let either mode work on a single instance, in seconds.
DEFAULT_TIMEOUT_SECS = 30 * 60
# Hard ceiling on agentcom spend per instance (USD). Solo mode uses time-only.
DEFAULT_BUDGET_USD = 3.0
# After the fleet's open-task count hits zero, wait this long for it to come
# back up before declaring "done" and stopping the hub.
FLEET_QUIESCE_SECS = 60


# ---------------------------------------------------------------- data classes


@dataclasses.dataclass
class Instance:
    instance_id: str
    repo: str
    base_commit: str
    problem_statement: str
    # FAIL_TO_PASS / PASS_TO_PASS are not used by this harness directly — they
    # are consumed by the swebench eval. We keep them so we can pass-through
    # to predictions.jsonl if the dataset variant requires it.
    fail_to_pass: list[str]
    pass_to_pass: list[str]


@dataclasses.dataclass
class RunResult:
    instance_id: str
    mode: str
    patch: str
    cost_usd: Optional[float]
    wall_secs: float
    exit_reason: str  # "ok", "timeout", "budget", "error:<msg>"
    log_path: str

    def to_dict(self) -> dict:
        return dataclasses.asdict(self)


# ---------------------------------------------------------------- dataset


def load_instances(ids: Optional[list[str]], n: Optional[int]) -> list[Instance]:
    try:
        from datasets import load_dataset  # type: ignore
    except ImportError:
        sys.exit(
            "missing dep: pip install datasets\n"
            "(or: pip install -r benchmark/swebench/requirements.txt)"
        )

    if LOCKED_INSTANCES_FILE.exists() and not ids:
        with open(LOCKED_INSTANCES_FILE) as f:
            ids = json.load(f)

    ds = load_dataset(DATASET, split="test")
    out: list[Instance] = []
    for row in ds:
        out.append(
            Instance(
                instance_id=row["instance_id"],
                repo=row["repo"],
                base_commit=row["base_commit"],
                problem_statement=row["problem_statement"],
                fail_to_pass=_parse_test_list(row.get("FAIL_TO_PASS")),
                pass_to_pass=_parse_test_list(row.get("PASS_TO_PASS")),
            )
        )

    if ids:
        wanted = set(ids)
        out = [i for i in out if i.instance_id in wanted]
        missing = wanted - {i.instance_id for i in out}
        if missing:
            sys.exit(f"unknown instance ids: {sorted(missing)}")
        if n is not None:
            out = out[:n]
        return out

    if n is not None:
        out = out[:n]
    return out


def _parse_test_list(raw) -> list[str]:
    if raw is None:
        return []
    if isinstance(raw, list):
        return list(raw)
    # Some dataset variants store these as JSON strings.
    try:
        v = json.loads(raw)
        return list(v) if isinstance(v, list) else []
    except (TypeError, ValueError):
        return []


# ---------------------------------------------------------------- repo setup


def clone_repo(instance: Instance, dest: Path) -> None:
    """Clone the GitHub repo at the instance's base_commit into dest."""
    if dest.exists():
        shutil.rmtree(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    url = f"https://github.com/{instance.repo}.git"
    _run(["git", "clone", "--quiet", url, str(dest)])
    _run(["git", "-C", str(dest), "checkout", "--quiet", instance.base_commit])
    # Detach config so commits inside the worktree don't try to identify the user.
    _run(["git", "-C", str(dest), "config", "user.email", "bench@agentcom.local"])
    _run(["git", "-C", str(dest), "config", "user.name", "agentcom-bench"])


def capture_patch(repo_dir: Path, base_commit: str) -> str:
    """git diff <base_commit> -- producing a unified diff suitable for swebench."""
    r = subprocess.run(
        ["git", "-C", str(repo_dir), "diff", base_commit],
        capture_output=True, text=True, check=True,
    )
    return r.stdout


# ---------------------------------------------------------------- runners


def run_solo(instance: Instance, repo_dir: Path, artifacts: Path,
             timeout_secs: int, dry_run: bool) -> RunResult:
    """One headless `claude -p` session inside the repo."""
    log_path = artifacts / "run.log"
    if dry_run:
        return RunResult(
            instance_id=instance.instance_id, mode="solo", patch="",
            cost_usd=0.0, wall_secs=0.0, exit_reason="ok (dry-run)",
            log_path=str(log_path),
        )
    prompt = _build_prompt(instance)
    (artifacts / "prompt.txt").write_text(prompt)

    cmd = [
        "claude", "-p", prompt,
        "--dangerously-skip-permissions",
        "--output-format", "json",
    ]
    start = time.perf_counter()
    exit_reason = "ok"
    cost: Optional[float] = None
    try:
        with log_path.open("w") as logf:
            r = subprocess.run(
                cmd, cwd=str(repo_dir), stdout=subprocess.PIPE,
                stderr=logf, text=True, timeout=timeout_secs,
            )
        cost = _parse_claude_cost(r.stdout)
        # Persist the raw JSON for debugging.
        (artifacts / "claude_output.json").write_text(r.stdout or "")
    except subprocess.TimeoutExpired:
        exit_reason = "timeout"
    except Exception as e:
        exit_reason = f"error:{e}"

    wall = time.perf_counter() - start
    patch = capture_patch(repo_dir, instance.base_commit)
    (artifacts / "patch.diff").write_text(patch)
    return RunResult(
        instance_id=instance.instance_id, mode="solo", patch=patch,
        cost_usd=cost, wall_secs=wall, exit_reason=exit_reason,
        log_path=str(log_path),
    )


def run_fleet(instance: Instance, repo_dir: Path, artifacts: Path,
              timeout_secs: int, budget_usd: float, dry_run: bool) -> RunResult:
    """Drop the bench fleet config in, run `agentcom up --headless --free`,
    poll until quiesced or budget/time hits, then stop and capture cost."""
    log_path = artifacts / "run.log"
    if dry_run:
        return RunResult(
            instance_id=instance.instance_id, mode="fleet", patch="",
            cost_usd=0.0, wall_secs=0.0, exit_reason="ok (dry-run)",
            log_path=str(log_path),
        )
    prompt = _build_prompt(instance)
    (artifacts / "prompt.txt").write_text(prompt)
    shutil.copy(FLEET_TOML, repo_dir / "agentcom.toml")
    # Wipe any stale state inherited from a prior fleet run on this path —
    # agentcom's data dir is keyed by canonical path hash, so a re-run of
    # the same instance reads the previous run's tasks/cost without this.
    subprocess.run(["agentcom", "stop"], cwd=str(repo_dir),
                   capture_output=True, timeout=20)
    subprocess.run(["agentcom", "clean", "--yes"], cwd=str(repo_dir),
                   capture_output=True, timeout=20)

    duration = f"{max(1, timeout_secs // 60)}m"
    cmd = [
        "agentcom", "up", "--headless", "--restart",
        "--free", prompt,
        "--for", duration,
        "--budget", f"{budget_usd}",
        "--finish-tasks",
    ]
    start = time.perf_counter()
    exit_reason = "ok"
    proc: Optional[subprocess.Popen] = None
    try:
        with log_path.open("w") as logf:
            proc = subprocess.Popen(
                cmd, cwd=str(repo_dir), stdout=logf, stderr=subprocess.STDOUT,
                # Own process group so we can SIGTERM the whole tree on stop.
                start_new_session=True,
            )
            exit_reason = _wait_for_fleet_quiesce(repo_dir, proc, timeout_secs)
            _stop_fleet(repo_dir, proc)
    except Exception as e:
        exit_reason = f"error:{e}"
        if proc:
            _stop_fleet(repo_dir, proc)

    wall = time.perf_counter() - start
    cost = _agentcom_cost(repo_dir)
    patch = capture_patch(repo_dir, instance.base_commit)
    (artifacts / "patch.diff").write_text(patch)
    return RunResult(
        instance_id=instance.instance_id, mode="fleet", patch=patch,
        cost_usd=cost, wall_secs=wall, exit_reason=exit_reason,
        log_path=str(log_path),
    )


# ---------------------------------------------------------------- fleet plumbing


def _wait_for_fleet_quiesce(repo_dir: Path, proc: subprocess.Popen,
                            timeout_secs: int) -> str:
    """Poll task list. Return 'ok' when the fleet has had zero open tasks
    for FLEET_QUIESCE_SECS *after* at least one task has been created during
    this run; 'timeout' on wall-clock; 'budget' if the agentcom process exits
    on its own (budget cap or crash). The composer needs time to think and
    file its first task — we never quiesce before that happens."""
    deadline = time.perf_counter() + timeout_secs
    zero_since: Optional[float] = None
    saw_work = False  # set true the first time we see any task on the board
    poll_secs = 10

    while time.perf_counter() < deadline:
        if proc.poll() is not None:
            return "budget"
        try:
            r = subprocess.run(
                ["agentcom", "task", "list", "--json"],
                cwd=str(repo_dir), capture_output=True, text=True, timeout=15,
            )
            total = _count_tasks(r.stdout)
            r2 = subprocess.run(
                ["agentcom", "task", "list", "--json", "--status", "open"],
                cwd=str(repo_dir), capture_output=True, text=True, timeout=15,
            )
            open_count = _count_tasks(r2.stdout)
        except Exception:
            total, open_count = -1, -1

        if total > 0:
            saw_work = True

        if saw_work and open_count == 0:
            if zero_since is None:
                zero_since = time.perf_counter()
            elif time.perf_counter() - zero_since >= FLEET_QUIESCE_SECS:
                return "ok"
        else:
            zero_since = None

        time.sleep(poll_secs)
    return "timeout"


def _count_tasks(raw: str) -> int:
    if not raw.strip():
        return -1
    try:
        v = json.loads(raw)
    except json.JSONDecodeError:
        return -1
    if isinstance(v, list):
        return len(v)
    if isinstance(v, dict) and "tasks" in v:
        return len(v["tasks"])
    return -1


def _stop_fleet(repo_dir: Path, proc: subprocess.Popen) -> None:
    # First ask agentcom nicely.
    try:
        subprocess.run(
            ["agentcom", "stop"], cwd=str(repo_dir),
            capture_output=True, timeout=20,
        )
    except Exception:
        pass
    # Then make sure the process tree is gone.
    try:
        if proc.poll() is None:
            os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
            try:
                proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
    except ProcessLookupError:
        pass


def _agentcom_cost(repo_dir: Path) -> Optional[float]:
    try:
        r = subprocess.run(
            ["agentcom", "cost", "--json"], cwd=str(repo_dir),
            capture_output=True, text=True, timeout=20,
        )
    except Exception:
        return None
    if r.returncode != 0 or not r.stdout.strip():
        return None
    try:
        v = json.loads(r.stdout)
    except json.JSONDecodeError:
        return None
    # Tolerant of a few possible shapes.
    if isinstance(v, dict):
        for k in ("total_cost_usd", "total_usd", "total"):
            if k in v and isinstance(v[k], (int, float)):
                return float(v[k])
        # Sum per-agent if that's all we got.
        agents = v.get("agents")
        if isinstance(agents, list):
            s = sum(float(a.get("cost_usd") or 0) for a in agents)
            return s if s > 0 else None
    return None


def _parse_claude_cost(raw: str) -> Optional[float]:
    """`claude -p --output-format json` returns a JSON object whose final
    `result` block contains usage / cost info."""
    if not raw:
        return None
    try:
        v = json.loads(raw)
    except json.JSONDecodeError:
        # Fallback: scrape "$0.1234" from a final line.
        m = re.search(r"\$\s*([0-9]+\.[0-9]+)", raw)
        return float(m.group(1)) if m else None
    for k in ("total_cost_usd", "cost_usd", "total_cost"):
        if k in v and isinstance(v[k], (int, float)):
            return float(v[k])
    usage = v.get("usage") or v.get("result", {}).get("usage")
    if isinstance(usage, dict) and "cost_usd" in usage:
        return float(usage["cost_usd"])
    return None


# ---------------------------------------------------------------- prompt


def _build_prompt(instance: Instance) -> str:
    return (
        "You are fixing a real bug in an open-source Python project.\n\n"
        f"Repository: {instance.repo}\n"
        f"Base commit: {instance.base_commit}\n\n"
        "Problem statement (verbatim from the upstream issue):\n"
        "---\n"
        f"{instance.problem_statement}\n"
        "---\n\n"
        "Make the smallest correct code change that resolves the issue. "
        "Do not refactor unrelated code. Do not edit test files unless the "
        "problem statement explicitly asks for it. When you believe the fix "
        "is complete, stop.\n"
    )


# ---------------------------------------------------------------- subprocess


def _run(cmd: list[str]) -> None:
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd)}\n{r.stderr}")


# ---------------------------------------------------------------- subcommands


def cmd_run(args: argparse.Namespace) -> int:
    modes = [m.strip() for m in args.modes.split(",") if m.strip()]
    for m in modes:
        if m not in ("solo", "fleet"):
            sys.exit(f"unknown mode: {m}")

    ids = [s.strip() for s in args.ids.split(",")] if args.ids else None
    # When no --ids and no instances.json, default to 2 so we don't pull all 300.
    n = args.instances
    if n is None and not ids and not LOCKED_INSTANCES_FILE.exists():
        n = 2
    instances = load_instances(ids, n)
    if not instances:
        sys.exit("no instances selected")

    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    manifest_path = out_dir / "manifest.json"
    manifest = _read_manifest(manifest_path)

    print(f"running {len(instances)} instance(s) × {len(modes)} mode(s) "
          f"→ {out_dir}")

    for inst in instances:
        for mode in modes:
            key = f"{inst.instance_id}::{mode}"
            if not args.force and key in manifest.get("done", {}):
                print(f"  skip {key} (already done)")
                continue
            print(f"  ▸ {key} …", flush=True)
            artifacts = out_dir / inst.instance_id / mode
            artifacts.mkdir(parents=True, exist_ok=True)
            repo_dir = artifacts / "repo"
            if not args.dry_run:
                try:
                    clone_repo(inst, repo_dir)
                except Exception as e:
                    print(f"    clone failed: {e}")
                    continue

            if mode == "solo":
                res = run_solo(inst, repo_dir, artifacts,
                               args.timeout, args.dry_run)
            else:
                res = run_fleet(inst, repo_dir, artifacts,
                                args.timeout, args.budget, args.dry_run)

            (artifacts / "meta.json").write_text(json.dumps(res.to_dict(), indent=2))
            manifest.setdefault("done", {})[key] = {
                "cost_usd": res.cost_usd,
                "wall_secs": res.wall_secs,
                "exit_reason": res.exit_reason,
            }
            _write_manifest(manifest_path, manifest)
            cost_str = f"${res.cost_usd:.4f}" if isinstance(res.cost_usd, (int, float)) else "$?"
            print(f"    {res.exit_reason}  cost={cost_str}  "
                  f"wall={res.wall_secs:.0f}s  "
                  f"patch={len(res.patch)} bytes")

    _write_predictions(out_dir, instances, modes)
    print(f"\nwrote predictions → {out_dir}/predictions-*.jsonl")
    return 0


def cmd_score(args: argparse.Namespace) -> int:
    """Invoke the official swebench evaluation harness over predictions.jsonl
    for each mode. Requires `pip install swebench` and Docker."""
    out_dir = Path(args.run_dir)
    preds = sorted(out_dir.glob("predictions-*.jsonl"))
    if not preds:
        sys.exit(f"no predictions-*.jsonl under {out_dir} — run `bench.py run` first")
    for p in preds:
        mode = p.stem.split("-", 1)[1]
        run_id = f"agentcom-{mode}-{int(time.time())}"
        print(f"scoring {p.name} → run_id={run_id}")
        cmd = [
            sys.executable, "-m", "swebench.harness.run_evaluation",
            "--predictions_path", str(p),
            "--max_workers", str(args.workers),
            "--run_id", run_id,
            "--dataset_name", DATASET,
        ]
        r = subprocess.run(cmd)
        if r.returncode != 0:
            print(f"  swebench eval exited {r.returncode}")
        # The swebench harness writes <run_id>.<model_name>.json with results.
        # We don't try to slurp it here — the user runs `bench.py report`.
    return 0


def cmd_report(args: argparse.Namespace) -> int:
    out_dir = Path(args.run_dir)
    manifest = _read_manifest(out_dir / "manifest.json")
    done = manifest.get("done", {})
    if not done:
        sys.exit(f"empty manifest under {out_dir}")

    by_instance: dict[str, dict[str, dict]] = {}
    for key, rec in done.items():
        inst_id, mode = key.split("::", 1)
        by_instance.setdefault(inst_id, {})[mode] = rec

    # Optionally fold in swebench eval results if present.
    resolved = _load_swebench_resolutions(out_dir)

    lines = [
        "# agentcom SWE-bench Lite results",
        "",
        f"Run dir: `{out_dir}`",
        "",
        "| instance | mode | resolved | cost ($) | wall (s) | exit |",
        "|---|---|---|---|---|---|",
    ]
    totals: dict[str, dict] = {}
    for inst_id in sorted(by_instance):
        for mode, rec in by_instance[inst_id].items():
            r = resolved.get((inst_id, mode))
            r_cell = "✓" if r is True else "✗" if r is False else "—"
            cost = rec.get("cost_usd")
            cost_cell = f"{cost:.3f}" if isinstance(cost, (int, float)) else "—"
            lines.append(
                f"| {inst_id} | {mode} | {r_cell} | {cost_cell} | "
                f"{rec.get('wall_secs', 0):.0f} | {rec.get('exit_reason', '?')} |"
            )
            t = totals.setdefault(mode, {"n": 0, "cost": 0.0, "wall": 0.0, "resolved": 0})
            t["n"] += 1
            if isinstance(cost, (int, float)):
                t["cost"] += cost
            t["wall"] += rec.get("wall_secs") or 0
            if r is True:
                t["resolved"] += 1

    lines += ["", "## Totals", "", "| mode | n | resolved | cost ($) | wall (s) |",
              "|---|---|---|---|---|"]
    for mode, t in totals.items():
        rate = f"{t['resolved']}/{t['n']}"
        lines.append(f"| {mode} | {t['n']} | {rate} | {t['cost']:.2f} | {t['wall']:.0f} |")

    out_md = out_dir / "report.md"
    out_md.write_text("\n".join(lines) + "\n")
    print(f"wrote {out_md}")
    return 0


def _load_swebench_resolutions(out_dir: Path) -> dict[tuple[str, str], bool]:
    """Look for swebench harness output (*.json) under out_dir and map
    (instance_id, mode) → resolved bool."""
    resolved: dict[tuple[str, str], bool] = {}
    for path in out_dir.glob("*.json"):
        try:
            v = json.loads(path.read_text())
        except Exception:
            continue
        # swebench result shape: {"resolved_ids": [...], "model_name_or_path": "..."}
        model = v.get("model_name_or_path") or ""
        mode = None
        for m in ("solo", "fleet"):
            if m in model:
                mode = m
                break
        if not mode:
            continue
        for inst_id in v.get("resolved_ids", []):
            resolved[(inst_id, mode)] = True
        for inst_id in v.get("unresolved_ids", []):
            resolved.setdefault((inst_id, mode), False)
    return resolved


# ---------------------------------------------------------------- predictions


def _write_predictions(out_dir: Path, instances: list[Instance],
                       modes: list[str]) -> None:
    for mode in modes:
        path = out_dir / f"predictions-{mode}.jsonl"
        with path.open("w") as f:
            for inst in instances:
                patch_file = out_dir / inst.instance_id / mode / "patch.diff"
                if not patch_file.exists():
                    continue
                patch = patch_file.read_text()
                f.write(json.dumps({
                    "instance_id": inst.instance_id,
                    "model_name_or_path": f"agentcom-{mode}",
                    "model_patch": patch,
                }) + "\n")


# ---------------------------------------------------------------- manifest


def _read_manifest(path: Path) -> dict:
    if not path.exists():
        return {}
    try:
        return json.loads(path.read_text())
    except json.JSONDecodeError:
        return {}


def _write_manifest(path: Path, manifest: dict) -> None:
    path.write_text(json.dumps(manifest, indent=2))


# ---------------------------------------------------------------- entrypoint


def main(argv: Iterable[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    run = sub.add_parser("run", help="run benchmark instances")
    run.add_argument("--instances", type=int, default=None,
                     help="number of instances to run (default: all locked instances, or 2 from dataset head)")
    run.add_argument("--ids", default="",
                     help="comma-separated instance ids (overrides --instances)")
    run.add_argument("--modes", default="solo,fleet",
                     help="comma-separated modes: solo,fleet (default: both)")
    run.add_argument("--out", default=f"runs/{int(time.time())}",
                     help="output directory (default: runs/<unix-ts>)")
    run.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT_SECS,
                     help=f"per-instance wall-clock cap in seconds (default {DEFAULT_TIMEOUT_SECS})")
    run.add_argument("--budget", type=float, default=DEFAULT_BUDGET_USD,
                     help=f"per-instance USD cap for fleet mode (default ${DEFAULT_BUDGET_USD})")
    run.add_argument("--force", action="store_true",
                     help="re-run instances already recorded in the manifest")
    run.add_argument("--dry-run", action="store_true",
                     help="don't invoke claude/agentcom — verify plumbing only")
    run.set_defaults(func=cmd_run)

    score = sub.add_parser("score", help="invoke swebench eval over the predictions")
    score.add_argument("--run-dir", required=True)
    score.add_argument("--workers", type=int, default=1)
    score.set_defaults(func=cmd_score)

    rep = sub.add_parser("report", help="render a markdown summary")
    rep.add_argument("--run-dir", required=True)
    rep.set_defaults(func=cmd_report)

    args = p.parse_args(list(argv))
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
