use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
mod adapters;
#[cfg(test)]
mod tests;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn require(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn load<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn save(p: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let temporary = p.with_extension("pending");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, p)?;
    Ok(())
}
fn epoch_ns() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
}
fn safe(s: &str) -> bool {
    !s.is_empty() && Path::new(s).components().all(|p| matches!(p, Component::Normal(_)))
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema_version: u32,
    seconds: u64,
    repeats: u32,
    seed: u64,
    tiers: Vec<String>,
    presets: BTreeMap<String, Vec<usize>>,
    models: Vec<Model>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Model {
    id: String,
    adapter: String,
    program: String,
    #[serde(default)]
    args: Vec<String>,
    tiers: BTreeMap<String, String>,
    max_workers: usize,
    #[serde(default)]
    expected_version: Option<String>,
    #[serde(default)]
    codex_home: Option<String>,
    #[serde(default)]
    pricing: Option<Pricing>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pricing {
    label: String,
    input_per_million: f64,
    cached_per_million: f64,
    output_per_million: f64,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FrozenFile {
    path: String,
    text: String,
    sha256: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Challenge {
    schema_version: u32,
    id: String,
    version: String,
    prompt: String,
    public_files: Vec<FrozenFile>,
    judge_files: Vec<FrozenFile>,
    protected: Vec<String>,
    candidate: String,
    submission: String,
    receipt: String,
    submission_target: String,
    added_lines_per_candidate: usize,
    judge_commands: Vec<Vec<String>>,
    judge_seconds: u64,
    score_file: String,
    score_pointer: String,
    direction: String,
    #[serde(default)]
    initial_receipt: Value,
    #[serde(default)]
    provenance: Value,
}
#[derive(Clone, Deserialize, Serialize)]
struct Cell {
    id: String,
    model_index: usize,
    tier: String,
    effort: Option<String>,
    workers: usize,
    repeat: u32,
    unavailable: Option<String>,
}
#[derive(Deserialize, Serialize)]
struct Plan {
    schema_version: u32,
    preset: String,
    config: Config,
    challenge: Challenge,
    cells: Vec<Cell>,
    runner_sha256: String,
    programs: BTreeMap<String, String>,
}
fn validate(config: &Config, challenge: &Challenge, preset: &str) -> Result<()> {
    require(config.schema_version == 1 && challenge.schema_version == 1, "unsupported schema version")?;
    require((1..=3600).contains(&config.seconds) && (1..=100).contains(&config.repeats), "invalid time/repeats")?;
    require(!config.models.is_empty() && !config.tiers.is_empty(), "empty models or tiers")?;
    let workers = config.presets.get(preset).ok_or("unknown preset")?;
    require(!workers.is_empty() && workers.iter().all(|&w| w <= 5), "workers must be 0..5")?;
    require(workers.iter().collect::<BTreeSet<_>>().len() == workers.len(), "duplicate worker count")?;
    require(config.models.iter().map(|m| &m.id).collect::<BTreeSet<_>>().len() == config.models.len(), "duplicate model id")?;
    require(config.tiers.iter().collect::<BTreeSet<_>>().len() == config.tiers.len(), "duplicate tier")?;
    require(config.tiers.iter().all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')), "invalid tier name")?;
    require(config.models.len() * config.tiers.len() * workers.len() * config.repeats as usize <= 10000, "plan exceeds 10000 cells")?;
    for m in &config.models {
        require(!m.id.is_empty() && ["codex_exec", "command_v1"].contains(&m.adapter.as_str()), "unknown adapter or empty model")?;
        require(m.tiers.values().all(|s| !s.is_empty()), "empty native effort mapping")?;
        require(m.max_workers <= 5 && (m.adapter != "codex_exec" || m.args.is_empty()), "invalid adapter arguments")?;
        if let Some(p) = &m.pricing {
            require(
                [p.input_per_million, p.cached_per_million, p.output_per_million].iter().all(|x| x.is_finite() && *x >= 0.0),
                "invalid prices",
            )?;
        }
    }
    require(["min", "max"].contains(&challenge.direction.as_str()), "direction must be min/max")?;
    require((1..=600).contains(&challenge.judge_seconds) && !challenge.judge_commands.is_empty(), "invalid judge bounds")?;
    require((1..=2000).contains(&challenge.added_lines_per_candidate), "source allowance must be 1..2000")?;
    for s in [&challenge.candidate, &challenge.submission, &challenge.receipt, &challenge.submission_target, &challenge.score_file] {
        require(safe(s), "unsafe challenge path")?;
    }
    for files in [&challenge.public_files, &challenge.judge_files] {
        let mut paths = BTreeSet::new();
        for f in files {
            require(safe(&f.path) && paths.insert(&f.path), "unsafe or duplicate frozen file path")?;
            require(hash(f.text.as_bytes()) == f.sha256, "frozen file hash mismatch")?;
        }
    }
    for p in &challenge.protected {
        require(challenge.public_files.iter().any(|f| &f.path == p), "protected file absent")?;
        require(p != &challenge.submission && p != &challenge.receipt, "checkpoint cannot be protected input")?;
    }
    require(challenge.public_files.iter().any(|f| f.path == challenge.candidate), "candidate absent")?;
    require(!challenge.protected.contains(&challenge.candidate), "candidate cannot be protected")?;
    require(
        !challenge.public_files.iter().any(|f| [&challenge.receipt, &challenge.submission].contains(&&f.path)),
        "checkpoint paths must not be public inputs",
    )?;
    require(challenge.judge_commands.iter().all(|a| !a.is_empty() && !a[0].is_empty()), "empty judge command")
}
fn cells(config: &Config, preset: &str) -> Vec<Cell> {
    let mut out = vec![];
    for repeat in 1..=config.repeats {
        let mut block = vec![];
        for (i, m) in config.models.iter().enumerate() {
            for tier in &config.tiers {
                for &workers in &config.presets[preset] {
                    let effort = m.tiers.get(tier).cloned();
                    let unavailable = if effort.is_none() {
                        Some("unsupported reasoning tier".into())
                    } else if workers > m.max_workers {
                        Some("unsupported worker count".into())
                    } else {
                        None
                    };
                    block.push(Cell {
                        id: format!("m{i}-{tier}-w{workers}-r{repeat}"),
                        model_index: i,
                        tier: tier.clone(),
                        effort,
                        workers,
                        repeat,
                        unavailable,
                    });
                }
            }
        }
        block.sort_by_key(|c| hash(format!("{}:{}", config.seed, c.id).as_bytes()));
        out.extend(block);
    }
    out
}
fn resolve(program: &str) -> Result<PathBuf> {
    if program.contains('/') {
        return Ok(fs::canonicalize(program)?);
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let p = dir.join(program);
        if p.is_file() {
            return Ok(fs::canonicalize(p)?);
        }
    }
    Err(format!("executable not found: {program}").into())
}
fn prepare(config: Config, challenge: Challenge, preset: &str, output: &Path) -> Result<()> {
    validate(&config, &challenge, preset)?;
    let mut programs = BTreeMap::new();
    for program in config.models.iter().map(|m| m.program.as_str()).chain(challenge.judge_commands.iter().map(|a| a[0].as_str())) {
        let p = resolve(program)?;
        programs.insert(p.to_string_lossy().into(), hash(&fs::read(&p)?));
    }
    let plan = Plan {
        schema_version: 1,
        preset: preset.into(),
        cells: cells(&config, preset),
        config,
        challenge,
        runner_sha256: hash(&fs::read(std::env::current_exe()?)?),
        programs,
    };
    fs::create_dir(output)?;
    save(&output.join("plan.json"), &plan)?;
    fs::write(output.join("plan.sha256"), hash(&fs::read(output.join("plan.json"))?))?;
    Ok(())
}
fn frozen_plan(root: &Path) -> Result<Plan> {
    require(fs::read_to_string(root.join("plan.sha256"))? == hash(&fs::read(root.join("plan.json"))?), "sealed plan changed")?;
    let plan: Plan = load(&root.join("plan.json"))?;
    require(plan.schema_version == 1, "invalid plan version")?;
    validate(&plan.config, &plan.challenge, &plan.preset)?;
    require(plan.runner_sha256 == hash(&fs::read(std::env::current_exe()?)?), "runner changed since prepare")?;
    for (p, h) in &plan.programs {
        require(hash(&fs::read(p)?) == *h, "adapter executable changed")?;
    }
    for program in plan.config.models.iter().map(|m| m.program.as_str()).chain(plan.challenge.judge_commands.iter().map(|a| a[0].as_str()))
    {
        require(plan.programs.contains_key(&resolve(program)?.display().to_string()), "executable resolution changed")?;
    }
    Ok(plan)
}
fn materialize(root: &Path, files: &[FrozenFile]) -> Result<()> {
    fs::create_dir_all(root)?;
    for f in files {
        let p = root.join(&f.path);
        fs::create_dir_all(p.parent().unwrap())?;
        fs::write(p, &f.text)?;
    }
    Ok(())
}
fn check_files(root: &Path, files: &[FrozenFile]) -> Result<()> {
    for f in files {
        let p = root.join(&f.path);
        require(!fs::symlink_metadata(&p)?.file_type().is_symlink() && hash(&fs::read(p)?) == f.sha256, "protected file changed")?;
    }
    Ok(())
}
fn plain_bytes(path: &Path) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    require(meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 1_048_576, "submission must be a regular file <=1 MiB")?;
    Ok(fs::read(path)?)
}
fn render(args: &[String], vars: &BTreeMap<&str, String>) -> Vec<String> {
    args.iter()
        .map(|s| {
            let mut a = s.clone();
            for (k, v) in vars {
                a = a.replace(&format!("{{{k}}}"), v);
            }
            a
        })
        .collect()
}
fn prompt(ch: &Challenge, cell: &Cell, seconds: u64) -> String {
    let roles = if cell.workers == 0 {
        "Work alone. Do not delegate or start other model sessions.".into()
    } else {
        let names = (1..=cell.workers).map(|n| format!("worker_{n}")).collect::<Vec<_>>().join(", ");
        format!("Spawn exactly {} workers, each with a fresh context, identical model/effort and a complete independent candidate in worker-1 through worker-{}. Agent task_name values: {}. Agent names use underscores; directory names use hyphens. No recursive/replacement agents. You coordinate, advise and select; do not implement another candidate. Copy a worker's candidate into {} and invoke the provided checker to select it. Never write the selected checkpoint or receipt yourself.", cell.workers, cell.workers, names, ch.candidate)
    };
    format!("{}\n\n{roles}\n\nYou have {seconds} seconds from the epoch in window.json, including reading, tools and coordination. There is no token ceiling. Finish before the deadline. Only change {} and the corresponding worker candidates. Each candidate may add at most {} lines over the pristine source; no auxiliary solution files or dependencies. The host grades only {} selected before the deadline. Use the supplied checker; do not write the checkpoint/receipt directly. Do not edit fixtures, simulator, checker or window.json. No network, commits, services, prior solutions, cross-attempt reads, judge access or new sessions. Worker directories already contain identical starting files.\n", ch.prompt, ch.candidate, ch.added_lines_per_candidate, ch.submission)
}
fn within_allowance(original: &str, text: &str, cap: usize) -> bool {
    let a: Vec<_> = original.lines().collect();
    let b: Vec<_> = text.lines().collect();
    if b.len() > a.len() + cap || a.len() > 5000 {
        return false;
    }
    let mut row = vec![0usize; a.len() + 1];
    for new in &b {
        let mut previous = 0;
        for (j, old) in a.iter().enumerate() {
            let saved = row[j + 1];
            row[j + 1] = if old == new { previous + 1 } else { row[j + 1].max(row[j]) };
            previous = saved;
        }
    }
    b.len() - row[a.len()] <= cap
}
fn source_limits(work: &Path, ch: &Challenge, workers: usize) -> Result<()> {
    let original = &ch.public_files.iter().find(|f| f.path == ch.candidate).unwrap().text;
    for dir in std::iter::once(work.to_owned()).chain((1..=workers).map(|n| work.join(format!("worker-{n}")))) {
        let text = String::from_utf8(plain_bytes(&dir.join(&ch.candidate))?)?;
        require(within_allowance(original, &text, ch.added_lines_per_candidate), "candidate source allowance exceeded")?;
        let allowed: BTreeSet<_> = ch
            .public_files
            .iter()
            .map(|f| f.path.as_str())
            .chain(["window.json", ch.submission.as_str(), ch.receipt.as_str(), "selected.tmp", "selected-json.tmp"])
            .collect();
        for p in files_below(&dir)? {
            let rel = p.strip_prefix(&dir)?.to_string_lossy();
            if (1..=workers).any(|n| rel.starts_with(&format!("worker-{n}/"))) || rel.contains("__pycache__/") {
                continue;
            }
            require(allowed.contains(rel.as_ref()), "unexpected auxiliary file")?;
        }
    }
    Ok(())
}
fn files_below(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = vec![];
    let mut dirs = vec![root.to_owned()];
    while let Some(d) = dirs.pop() {
        for e in fs::read_dir(d)? {
            let e = e?;
            let t = e.file_type()?;
            require(!t.is_symlink(), "symlinks are not allowed")?;
            if t.is_dir() {
                dirs.push(e.path());
            } else {
                found.push(e.path());
            }
            require(found.len() + dirs.len() <= 4096, "file count limit exceeded")?;
        }
    }
    Ok(found)
}
fn grade(ch: &Challenge, judge: &Path, stage: &Path, artifact: &[u8]) -> Result<Value> {
    materialize(stage, &ch.public_files)?;
    fs::write(stage.join(&ch.submission_target), artifact)?;
    let vars = BTreeMap::from([("judge", judge.display().to_string()), ("stage", stage.display().to_string())]);
    let mut checks = vec![];
    for (n, args) in ch.judge_commands.iter().enumerate() {
        check_files(judge, &ch.judge_files)?;
        let args = render(args, &vars);
        let mut command = Command::new(&args[0]);
        command.args(&args[1..]).current_dir(stage).env("PYTHONDONTWRITEBYTECODE", "1");
        let result = adapters::bounded(command, Duration::from_secs(ch.judge_seconds), stage, &format!("judge-{n}"), None)?;
        checks.push(json!({"command":args,"execution":result}));
        check_files(judge, &ch.judge_files)?;
        if result["exit_code"] != 0 || result["stop"] != "completed" {
            return Ok(json!({"correct":false,"checks":checks}));
        }
    }
    let score: Value = load(&stage.join(&ch.score_file))?;
    require(score["correct"] == true, "judge did not affirm correctness")?;
    let value = score.pointer(&ch.score_pointer).and_then(Value::as_f64).ok_or("missing numeric judge score")?;
    require(value.is_finite(), "non-finite score")?;
    Ok(json!({"correct":true,"score":value,"checks":checks,"detail":score}))
}
fn attempt(root: &Path, plan: &Plan, cell: &Cell) -> Result<Value> {
    let ch = &plan.challenge;
    let model = &plan.config.models[cell.model_index];
    let dir = root.join(&cell.id);
    fs::create_dir(&dir)?;
    let dir = fs::canonicalize(dir)?;
    let work = dir.join("work");
    let judge = dir.join("judge");
    materialize(&work, &ch.public_files)?;
    materialize(&judge, &ch.judge_files)?;
    let base = &ch.public_files.iter().find(|f| f.path == ch.candidate).unwrap().text;
    let start_ns = epoch_ns();
    let window = json!({"started_unix_ns":start_ns.to_string(),"seconds":plan.config.seconds});
    for d in std::iter::once(work.clone()).chain((1..=cell.workers).map(|n| work.join(format!("worker-{n}")))) {
        materialize(&d, &ch.public_files)?;
        save(&d.join("window.json"), &window)?;
        fs::write(d.join(&ch.submission), base)?;
        let mut receipt = ch.initial_receipt.clone();
        if !receipt.is_object() {
            receipt = json!({});
        }
        receipt["sha256"] = json!(hash(base.as_bytes()));
        receipt["promoted_at_unix_ns"] = json!(start_ns.to_string());
        save(&d.join(&ch.receipt), &receipt)?;
    }
    let request = json!({"schema_version":1,"model":model.id,"effort":cell.effort,"workers":cell.workers,"workspace":work,
        "seconds":plan.config.seconds,"started_unix_ns":start_ns.to_string(),"prompt":prompt(ch,cell,plan.config.seconds)});
    save(&dir.join("request.json"), &request)?;
    let mut command = adapters::command(model, cell, &work, &dir)?;
    if model.adapter == "codex_exec" {
        fs::write(dir.join("stdin"), request["prompt"].as_str().unwrap())?;
    } else {
        fs::write(dir.join("stdin"), serde_json::to_vec(&request)?)?;
    }
    command.stdin(fs::File::open(dir.join("stdin"))?);
    let remaining =
        Duration::from_secs(plan.config.seconds).saturating_sub(Duration::from_nanos((epoch_ns() - start_ns).min(u64::MAX as u128) as u64));
    require(!remaining.is_zero(), "setup exhausted the time allowance before model launch")?;
    let execution = adapters::bounded(
        command,
        remaining,
        &dir,
        "adapter",
        Some((&work.join(&ch.submission), &dir.join("selected"), &work.join(&ch.receipt))),
    )?;
    save(&dir.join("execution.json"), &execution)?;
    let accounting = adapters::account(model, cell, &dir);
    save(&dir.join("accounting.json"), &accounting)?;
    let mut failures = vec![];
    let infrastructure_failure = execution["exit_code"] != 0 && execution["stop"] != "deadline";
    if execution["cutoff_epoch_ns"]
        .as_str()
        .and_then(|s| s.parse::<u128>().ok())
        .is_some_and(|n| n > start_ns + plan.config.seconds as u128 * 1_000_000_000 + 100_000_000)
    {
        failures.push("cutoff exceeded 100ms tolerance".into());
    }
    if execution["stop"] == "cancelled" || execution["stop"] == "log_limit" {
        failures.push("execution interrupted".into());
    }
    if execution["exit_code"] != 0 && execution["stop"] != "deadline" {
        failures.push("adapter exited unsuccessfully".into());
    }
    let artifact = plain_bytes(&dir.join("selected"))?;
    if !within_allowance(base, &String::from_utf8_lossy(&artifact), ch.added_lines_per_candidate) {
        failures.push("selected source allowance exceeded".into());
    }
    let receipt: Value = load(&work.join(&ch.receipt))?;
    if receipt["sha256"] != hash(&artifact) {
        failures.push("checkpoint receipt hash mismatch".into());
    }
    let stamp = receipt["promoted_at_unix_ns"]
        .as_u64()
        .map(|v| v as u128)
        .or_else(|| receipt["promoted_at_unix_ns"].as_str().and_then(|s| s.parse().ok()));
    if !stamp.is_some_and(|s| s >= start_ns && s <= start_ns + plan.config.seconds as u128 * 1_000_000_000) {
        failures.push("checkpoint outside window".into());
    }
    for d in std::iter::once(work.clone()).chain((1..=cell.workers).map(|n| work.join(format!("worker-{n}")))) {
        let protected: Vec<_> = ch.public_files.iter().filter(|f| ch.protected.contains(&f.path)).cloned().collect();
        if let Err(e) = check_files(&d, &protected) {
            failures.push(e.to_string());
        }
        if load::<Value>(&d.join("window.json")).ok().as_ref() != Some(&window) {
            failures.push("window changed".into());
        }
    }
    if let Err(e) = source_limits(&work, ch, cell.workers) {
        failures.push(e.to_string());
    }
    if accounting["settings_valid"] != true {
        failures.push("adapter settings/delegation not verified".into());
    }
    let grading = match grade(ch, &judge, &dir.join("stage"), &artifact) {
        Ok(v) => v,
        Err(e) => json!({"correct":false,"error":e.to_string()}),
    };
    if grading["correct"] != true {
        failures.push("judge rejected submission".into());
    }
    Ok(
        json!({"cell":cell,"valid":failures.is_empty(),"failures":failures,"infrastructure_failure":infrastructure_failure,"execution":execution,"accounting":accounting,
        "artifact_sha256":hash(&artifact),"receipt":receipt,"grading":grading}),
    )
}
fn run(root: &Path) -> Result<()> {
    let plan = frozen_plan(root)?;
    fs::OpenOptions::new().write(true).create_new(true).open(root.join("started"))?;
    for cell in &plan.cells {
        frozen_plan(root)?;
        let row = if let Some(reason) = &cell.unavailable {
            json!({"cell":cell,"valid":false,"unavailable":reason})
        } else {
            match attempt(root, &plan, cell) {
                Ok(v) => v,
                Err(e) => json!({"cell":cell,"valid":false,"error":e.to_string(),
            "accounting":load::<Value>(&root.join(&cell.id).join("accounting.json")).ok()}),
            }
        };
        save(&root.join(format!("{}.json", cell.id)), &row)?;
        println!("{}", json!({"id":cell.id,"valid":row["valid"],"score":row["grading"]["score"]}));
        if row.get("error").is_some() || row["infrastructure_failure"] == true || adapters::cancelled() {
            break;
        }
    }
    report(root)
}
fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}
fn report_value(root: &Path) -> Result<Value> {
    require(fs::read_to_string(root.join("plan.sha256"))? == hash(&fs::read(root.join("plan.json"))?), "sealed plan changed")?;
    let plan: Plan = load(&root.join("plan.json"))?;
    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut total = 0.0;
    let mut known = 0;
    for cell in &plan.cells {
        let path = root.join(format!("{}.json", cell.id));
        let row = if path.exists() { load::<Value>(&path)? } else { json!({"cell":cell,"valid":false,"unfinished":true}) };
        if let Some(cost) = row["accounting"]["cost_equivalent"].as_f64() {
            total += cost;
            known += 1;
        }
        groups.entry(format!("m{}-{}-w{}", cell.model_index, cell.tier, cell.workers)).or_default().push(row);
    }
    let mut summary = vec![];
    for (id, rows) in groups {
        let valid: Vec<_> = rows.iter().filter(|r| r["valid"] == true).collect();
        let scores: Vec<_> = valid.iter().filter_map(|r| r["grading"]["score"].as_f64()).collect();
        let costs: Vec<_> = rows.iter().filter_map(|r| r["accounting"]["cost_equivalent"].as_f64()).collect();
        let full = valid.len() == rows.len() && costs.len() == rows.len();
        let c = &rows[0]["cell"];
        let mi = c["model_index"].as_u64().unwrap() as usize;
        summary.push(json!({"id":id,"model":plan.config.models[mi].id,"tier":c["tier"],"workers":c["workers"],"attempts":rows.len(),
            "valid":valid.len(),"unfinished":rows.iter().filter(|r|r["unfinished"]==true).count(),"unavailable":rows.iter().filter(|r|r.get("unavailable").is_some()).count(),
            "scores":scores,"median_score":median(scores.clone()),"score_min":scores.iter().copied().reduce(f64::min),"score_max":scores.iter().copied().reduce(f64::max),
            "median_wall_seconds":median(rows.iter().filter_map(|r|r["execution"]["elapsed_seconds"].as_f64()).collect()),
            "exact_usage_cells":rows.iter().filter(|r|r["accounting"]["usage_complete"]==true&&r["accounting"]["settings_valid"]==true).count(),
            "median_cost_equivalent":median(costs),"frontier_eligible":full}));
    }
    let copy = summary.clone();
    for p in &mut summary {
        let dominated = p["frontier_eligible"] == true
            && copy.iter().any(|q| {
                if q["frontier_eligible"] != true {
                    return false;
                }
                let a = p["median_score"].as_f64().unwrap();
                let b = q["median_score"].as_f64().unwrap();
                let x = p["median_cost_equivalent"].as_f64().unwrap();
                let y = q["median_cost_equivalent"].as_f64().unwrap();
                let better = if plan.challenge.direction == "min" { b <= a } else { b >= a };
                better && y <= x && (a != b || y < x)
            });
        p["observed_frontier"] = json!(p["frontier_eligible"] == true && !dominated);
    }
    Ok(json!({"schema_version":1,"challenge":plan.challenge.id,"direction":plan.challenge.direction,"groups":summary,
        "solver_cost_equivalent_known_sum":total,"cells_with_cost":known,"planned_cells":plan.cells.len(),"campaign_overhead_cost":null,"cost_note":"Not a bill; missing costs and outer researcher are not zero."}))
}
fn report(root: &Path) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&report_value(root)?)?);
    Ok(())
}
fn cli(args: &[String]) -> Result<()> {
    match args.get(1).map(String::as_str) {
        Some("plan" | "prepare") => {
            require(
                args.len() == if args[1] == "plan" { 5 } else { 6 },
                "usage: plan CONFIG PRESET CHALLENGE | prepare CONFIG PRESET CHALLENGE NEW_DIRECTORY",
            )?;
            let config: Config = load(Path::new(&args[2]))?;
            let ch: Challenge = load(Path::new(&args[4]))?;
            validate(&config, &ch, &args[3])?;
            if args[1] == "prepare" {
                prepare(config, ch, &args[3], Path::new(&args[5]))?;
            } else {
                let cells = cells(&config, &args[3]);
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({"challenge":ch.id,"cells":cells,"max_solver_seconds":cells.iter().filter(|c|c.unavailable.is_none()).count() as u64*config.seconds,"launches_models":false})
                    )?
                );
            }
        }
        Some("run") if args.len() == 3 => run(Path::new(&args[2]))?,
        Some("report") if args.len() == 3 => report(Path::new(&args[2]))?,
        _ => return Err("usage: harness-bench plan|prepare|run|report (see README.md)".into()),
    }
    Ok(())
}
fn main() {
    adapters::install_signals();
    if let Err(e) = cli(&std::env::args().collect::<Vec<_>>()) {
        eprintln!("benchmark: {e}");
        std::process::exit(1);
    }
}
