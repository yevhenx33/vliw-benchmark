# VLIW benchmark

Compare LLM coding agents on a frozen VLIW optimization challenge, using equal
wall-clock budgets and independently checked submissions. This standalone
repository is extracted from the harness benchmark package. No generated
solutions, private model traces, credentials or campaign directories are included.

The runner runs a model × reasoning-tier × worker-count matrix on a parameterized
challenge. The defaults are three independent attempts per cell and 600 seconds
per attempt. Zero workers is a solo agent; positive counts mean a coordinator
plus that many complete, independent solver candidates. This does not measure
combining independently implemented product subtasks.

The shipped configuration has `solo` (four tiers, 12 attempts) and `teams`
(four tiers × six setups, 72 attempts). Set `repeats` to 1 for one shot per
configuration, narrow `tiers`, or add model entries. No token ceiling applies.
An omitted tier mapping or excessive worker count produces an explicit
unavailable cell; it never substitutes another setting. Tier labels are local
configuration names, not equivalent computation across model providers.

## Commands

Linux, Rust 1.75+, Python 3, and the selected adapter executable are required.
Build and test locally; these tests only run fake adapters and local judges:

```sh
CARGO_TARGET_DIR=/tmp/vliw-benchmark-build cargo test --locked --manifest-path Cargo.toml
CARGO_TARGET_DIR=/tmp/vliw-benchmark-build cargo build --locked --manifest-path Cargo.toml
```

These commands plan or freeze inputs and **do not contact models**:

```sh
/tmp/vliw-benchmark-build/debug/harness-bench plan examples.json solo challenges/vliw.json
/tmp/vliw-benchmark-build/debug/harness-bench plan examples.json teams challenges/vliw.json
/tmp/vliw-benchmark-build/debug/harness-bench prepare examples.json solo challenges/vliw.json /tmp/my-benchmark
```

`prepare` requires a new output directory. It seals the config, order, challenge,
runner binary and adapter executable hashes. Edit the example model/version/price
settings before preparing a real campaign. The example CLI version and pricing
are explicit snapshots, not automatic claims about availability or billing.

Only `run` launches models and incurs model usage. Review the plan and authorize
your intended campaign before invoking it. The examples select 12 solo or 72
team-grid attempts, up to 2 or 12 solver hours at the default 600-second budget:

```sh
/tmp/vliw-benchmark-build/debug/harness-bench run /tmp/my-benchmark
/tmp/vliw-benchmark-build/debug/harness-bench report /tmp/my-benchmark
```

Cells run sequentially, with a deterministic shuffled block per repeat and no
grading overlap. There are no implicit retries or campaign resumes. A `started`
marker prevents accidental replay; an interrupted campaign retains unfinished
cells. Infrastructure errors stop the campaign. Model/judge failures are retained
as failed cells; elapsed deadlines can still yield a valid earlier checkpoint.
To retry, prepare a separately identified campaign and retain earlier costs.

## Model adapters

`codex_exec` uses the installed non-interactive CLI without a shared daemon, with an exact configured
version, model and mapped effort, workspace-write, network disabled for tools,
user config ignored, host skill discovery disabled, apps disabled, and bounded
native delegation. Native root/worker traces audit actual model, effort, CLI
version, hierarchy, spawn counts, completion and token counters. Changes in the
launcher hash or CLI version reject the run. Native binaries, injected context,
model access and provider behavior are not proven by a successful plan.
Interface: [official non-interactive documentation](https://learn.chatgpt.com/docs/non-interactive-mode).

`command_v1` permits another provider's **trusted agent-loop adapter**, not a raw
text-completion endpoint. Configure `program`, `args`, `tiers`, `max_workers` and
optional `pricing`. Arguments support `{workspace}`, `{request}`, `{model}`,
`{effort}`, `{workers}`; they are passed directly without shell interpolation.
The adapter runs in the workspace and receives a JSON request on stdin with
`schema_version`, `model`, `effort`, `workers`, `workspace`, `seconds`,
`started_unix_ns` and `prompt`. It must honor the same tools, deadline, checkpoint,
team strategy and restrictions. Another provider adapter must be qualified
against these contracts before claiming a comparable live result.

Emit one final JSONL record per root/worker to stdout (other progress is allowed):

```json
{"type":"benchmark.context","id":"root-id","parent":null,"model":"model-id","effort":"native-tier","complete":true,"usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":10,"cache_write_input_tokens":0}}
```

Workers use unique ids with the root id as `parent`. Exactly `workers + 1`
contexts are required. The trusted adapter must report all model calls, including
internal attempts, and attest actual settings; model-written self-reports are
not adequate telemetry. Input includes cached input; output includes reasoning.
Incomplete usage, duplicate contexts, settings mismatch, nonzero unpriced cache
writes, native compaction or counter resets yield no exact price. Known raw
counts remain visible. Price labels must identify the rate snapshot and its
adjustments; computed equivalents are not bills. Auth remains the adapter's
responsibility and must not be put in configuration or logs.

## Challenge contract

Pass a different JSON manifest as the `CHALLENGE` argument; the runner contains
no task-specific score or fixture logic. `challenges/vliw.json` is a complete
example. Unknown config fields, unsafe paths and wrong fixture hashes fail.

The manifest supplies:

- Identity/version, solver prompt, candidate path, selected submission and receipt
  paths, final staging target, uniform per-candidate added-line allowance.
- Public starting files and separate judge files, each as `path`, `text`, `sha256`;
  public protected paths must remain unchanged. UTF-8 text submissions are supported.
- Sequential judge argument arrays, a per-command timeout, score file, JSON score
  pointer and `min`/`max` direction. `{judge}` and `{stage}` expand to trusted paths.
- Optional initial receipt fields used by that challenge's supplied checker.

The supplied checker owns atomic selection and writes the selected file plus a
receipt containing `sha256` and `promoted_at_unix_ns`. The runner seeds a baseline
fallback, polls checkpoint receipts, freezes processes before copying the selected
artifact, kills the process group and observed descendants, then grades the exact
copy with pristine fixtures. It checks receipt identity/time, source allowances,
protected files and native adapter settings. Cutoff overshoot above 100ms invalidates
the attempt. SIGINT/SIGTERM cancel active children; SIGKILL/host failure cannot
guarantee cleanup. The process tree must remain cooperative: this is not a
container isolation or confidential-judge boundary. Exporting candidate code
executes Python; run untrusted/adversarial campaigns inside an external sandbox.

Bounds: 10,000 planned cells, 1–100 repeats, 1–3,600 solver seconds, 0–5 workers,
1–600 seconds per judge command, 1 MiB artifacts, 64 MiB process logs, 4,096
workspace files, 100,000 scanned native-session entries and 1,000 receipt samples.
No symlinks or extra solution files are admitted. Source additions use an ordered
line comparison with at most 5,000 baseline lines and 2,000 allowed additions.
Planning, preparation, grading and reporting sit outside solver time. Logs and
fixtures are retained locally; no repository-wide cleanup occurs.

## Frozen VLIW package and reporting

The package reuses the original commit, simulator, nine held-out inputs and eight
upstream correctness cases from the 24-cell experiment. The judge only gains a
portable BASE path; its scoring logic is unchanged. The development checker reads
the configured duration instead of hardcoding 600 seconds. Both adaptations and
original hashes are recorded in provenance. Uniform 500-line candidate allowances
make this challenge version 2; historical single/team allowances differed, so the
original grid is not a direct reference run for this version.

Results retain exact selected source, exported program, frozen plan, grading
logs, native trace hashes/paths, per-context accounting and checkpoint timeline.
Reports show every planned configuration, valid/unfinished/unavailable counts,
individual valid scores, median/range, observed wall time and price estimates.
Failed cells remain in reliability and known cost totals. Only configurations
with every repeat valid and exactly priced enter the observed median-score/cost
frontier. There are no significance or general optimality claims from three runs.
Malformed result files fail reporting instead of becoming missing values.
Researcher/setup cost is explicitly unknown, not zero; the report does not claim
full campaign cost. This extraction is verified with local fake adapters and judges; no new live
model campaign is included in the publication checks.

Recovery: remove only a specifically owned campaign directory after preserving
its results. No implicit cleanup or retry occurs.

## Practical configuration

Use `solo` for one agent across the four configured reasoning tiers, three
repeats each. To reproduce a high-effort coordinator-plus-one comparison, copy
`examples.json`, set `tiers` to `["high"]`, and set `presets.teams` to `[1]`;
prepare that configuration with the `teams` preset. `repeats: 1` gives one shot.
Pass another challenge manifest to evaluate a different problem; qualify its
fixtures, checker and independent judge first.

The Codex version and prices in the example are historical snapshots. Set
`expected_version` to your qualified installed CLI version and use pricing
appropriate to your adapter. Authentication remains in the model CLI, never
in this repository or its JSON configuration.

Team agent task names use underscores (`worker_1`); workspace directories use
hyphens (`worker-1`). This extraction adds that clarification after two historical
attempts first tried an invalid agent name. The strict audit still counts every
spawn call, including rejected calls. No historical result is reclassified and
the frozen VLIW challenge/judge is byte-identical to the source package.

## What the experiments established

On one frozen VLIW task with GPT-6.1 Sol and 10-minute maximum attempts:

| Setup | Three cycle scores | Median | Median solver API equivalent |
| --- | --- | ---: | ---: |
| Solo high | 1180, 1181, 1164 | 1180 | $0.472 |
| Solo extra-high | 1124, 1133, 1399 | 1133 | $0.487–$0.491 (bounded) |
| Coordinator + one, high | 1178, 1177, 1135 | 1177 (descriptive only) | $0.858 |

Only one team attempt passed the full protocol; all three kernels were correct.
The team median includes two invalid attempts and cannot enter the strict
frontier. The original 24-configuration grid had one observation per setting;
no 2–5-worker configuration reached its observed dollar-cost frontier.
These results support solo high as a practical measured default for this task,
not a general model ranking. Extra-high traded a better median for a worse
outlier. Prompt and tool autoresearch did not meet their predeclared 3% median
quality target. This repository does not contain the private experimental
traces or generated solutions; these aggregate observations are context, not
an independently reproducible results dataset.

## Attribution and license

The Rust runner originated in the Apache-2.0-licensed
[yevhenx33/harness](https://github.com/yevhenx33/harness) project; see `LICENSE`.
The embedded challenge fixtures are from
[Anthropic's original performance take-home](https://github.com/anthropics/original_performance_takehome),
pinned to commit `5452f74bd977807ac2e74f3d29432b9df6f25197`.
Their original README and attribution are retained in the manifest. That commit
contains no standalone license file; the runner's license does not relicense
those third-party fixtures. The local checker and judge adaptations and original
hashes are recorded in the manifest's provenance.
