# agentcom benchmark results history

Tracks per-run results across three modes (solo-claude, solo-deepseek, fleet) on the
locked set of 10 SWE-bench Lite instances defined in `benchmark/swebench/instances.json`.
After each run, append a new row to the table below. Scorecard target: fleet resolves
≥2 more instances than solo Claude AND ≥1 more than solo DeepSeek, with ≤3× wall-time
and ≤2× cost premium on resolved tasks.

| Date | SHA | Instances | solo-claude resolved | solo-deepseek resolved | fleet resolved | fleet median $/resolved | fleet median wall (s) | notes |
|---|---|---|---|---|---|---|---|---|
| 2026-06-16 | 3983cab | 10 | 0 | — | 0 | — | — | placeholder — instances locked, no run yet |
| 2026-06-16 | 3d2d942 | 1 | 0/1 | — | 0/1 | — | 130 | first-real: pallets/flask-4045 only; solo $0.11 29s, fleet $0.42 130s; reviewer never ran; solo-deepseek not yet implemented |

## How to add a row

1. Run the benchmark across all available modes:

   ```sh
   python benchmark/swebench/bench.py run \
       --instances 10 \
       --modes solo,solo-deepseek,fleet \
       --out runs/$(date +%F)
   ```
   (solo uses `claude -p`; solo-deepseek uses fleet-deepseek.toml and requires `DEEPSEEK_API_KEY`.)

2. Score the results:

   ```sh
   python benchmark/swebench/bench.py score \
       --run-dir runs/$(date +%F)
   ```

3. Generate a markdown report:

   ```sh
   python benchmark/swebench/bench.py report \
       --run-dir runs/$(date +%F)
   ```

4. Copy the totals from the report into a new row above. Fill in the SHA with `git rev-parse HEAD`. Commit and push.
