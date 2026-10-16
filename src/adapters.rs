use super::*;
use std::{
    io::BufRead,
    os::unix::process::CommandExt,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};
static CANCEL: AtomicBool = AtomicBool::new(false);
extern "C" fn interrupt(_: i32) {
    CANCEL.store(true, Ordering::SeqCst);
}
pub fn install_signals() {
    unsafe {
        libc::signal(libc::SIGINT, interrupt as usize);
        libc::signal(libc::SIGTERM, interrupt as usize);
    }
}
pub fn cancelled() -> bool {
    CANCEL.load(Ordering::SeqCst)
}
fn signal(pid: i32, sig: i32) {
    unsafe {
        libc::kill(pid, sig);
    }
}
fn descendants(root: u32) -> BTreeSet<u32> {
    let mut parents = vec![];
    for e in fs::read_dir("/proc").into_iter().flatten().flatten() {
        if let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() {
            if let Ok(text) = fs::read_to_string(e.path().join("stat")) {
                if let Some(parent) =
                    text.rsplit_once(") ").and_then(|(_, s)| s.split_whitespace().nth(1)).and_then(|s| s.parse::<u32>().ok())
                {
                    parents.push((pid, parent));
                }
            }
        }
    }
    let mut found = BTreeSet::from([root]);
    loop {
        let n = found.len();
        for &(pid, parent) in &parents {
            if found.contains(&parent) {
                found.insert(pid);
            }
        }
        if found.len() == n {
            return found;
        }
    }
}
struct Process {
    child: std::process::Child,
    seen: BTreeSet<u32>,
}
impl Drop for Process {
    fn drop(&mut self) {
        self.seen.extend(descendants(self.child.id()));
        signal(-(self.child.id() as i32), libc::SIGKILL);
        for pid in &self.seen {
            signal(*pid as i32, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}
pub fn bounded(mut command: Command, limit: Duration, dir: &Path, name: &str, snapshot: Option<(&Path, &Path, &Path)>) -> Result<Value> {
    let out = dir.join(format!("{name}.jsonl"));
    let err = dir.join(format!("{name}.stderr"));
    command.stdout(fs::File::create(&out)?).stderr(fs::File::create(&err)?).process_group(0);
    let start = Instant::now();
    let mut process = Process { child: command.spawn()?, seen: BTreeSet::new() };
    let stop;
    let exit_code;
    let mut checkpoints = vec![];
    let mut last_receipt = Value::Null;
    loop {
        process.seen.extend(descendants(process.child.id()));
        if let Some((_, _, receipt)) = snapshot {
            if let Ok(value) = load::<Value>(receipt) {
                if value != last_receipt && checkpoints.len() < 1000 {
                    checkpoints.push(json!({"elapsed_seconds":start.elapsed().as_secs_f64(),"receipt":value}));
                    last_receipt = value;
                }
            }
        }
        if cancelled() {
            stop = "cancelled";
            exit_code = None;
            break;
        }
        if start.elapsed() >= limit {
            stop = "deadline";
            exit_code = None;
            break;
        }
        if fs::metadata(&out)?.len() + fs::metadata(&err)?.len() > 67_108_864 {
            stop = "log_limit";
            exit_code = None;
            break;
        }
        if let Some(status) = process.child.try_wait()? {
            stop = "completed";
            exit_code = status.code();
            break;
        }
        thread::sleep(Duration::from_millis(20).min(limit.saturating_sub(start.elapsed())));
    }
    signal(-(process.child.id() as i32), libc::SIGSTOP);
    process.seen.extend(descendants(process.child.id()));
    for pid in &process.seen {
        signal(*pid as i32, libc::SIGSTOP);
    }
    let snapshot_error =
        snapshot.and_then(|(from, to, _)| plain_bytes(from).and_then(|bytes| Ok(fs::write(to, bytes)?)).err().map(|e| e.to_string()));
    let elapsed = start.elapsed().as_secs_f64();
    let cutoff_epoch_ns = epoch_ns().to_string();
    drop(process);
    Ok(
        json!({"stop":stop,"exit_code":exit_code,"elapsed_seconds":elapsed,"cutoff_epoch_ns":cutoff_epoch_ns,"checkpoints":checkpoints,"snapshot_error":snapshot_error,"stdout_sha256":hash(&fs::read(out)?),"stderr_sha256":hash(&fs::read(err)?)}),
    )
}
pub fn command(model: &Model, cell: &Cell, work: &Path, dir: &Path) -> Result<Command> {
    let program = resolve(&model.program)?;
    let mut cmd = Command::new(program);
    cmd.current_dir(work).env("PYTHONDONTWRITEBYTECODE", "1");
    if model.adapter == "command_v1" {
        let vars = BTreeMap::from([
            ("workspace", work.display().to_string()),
            ("request", dir.join("request.json").display().to_string()),
            ("model", model.id.clone()),
            ("effort", cell.effort.clone().unwrap()),
            ("workers", cell.workers.to_string()),
        ]);
        cmd.args(render(&model.args, &vars));
    } else {
        let mut version_command = Command::new(resolve(&model.program)?);
        version_command.arg("--version");
        let actual = bounded(version_command, Duration::from_secs(10), dir, "version", None)?;
        let version = fs::read_to_string(dir.join("version.jsonl"))?.trim().to_owned();
        require(
            actual["exit_code"] == 0 && model.expected_version.as_ref() == Some(&version),
            "Codex version must match expected_version",
        )?;
        cmd.args([
            "--no-daemon",
            "exec",
            "--ignore-user-config",
            "--ignore-rules",
            "--skip-git-repo-check",
            "-s",
            "workspace-write",
            "--json",
            "-m",
            &model.id,
        ]);
        for value in [
            format!("model_reasoning_effort={:?}", cell.effort.as_ref().unwrap()),
            "approval_policy=\"never\"".into(),
            "sandbox_workspace_write.network_access=false".into(),
            "features.apps=false".into(),
            "features.skip_host_skill_discovery=true".into(),
            format!("features.multi_agent={}", cell.workers > 0),
            format!("agents.max_concurrent_threads_per_session={}", cell.workers.max(1)),
            "agents.max_depth=1".into(),
        ] {
            cmd.args(["-c", &value]);
        }
        cmd.arg("-");
        if let Some(home) = &model.codex_home {
            cmd.env("CODEX_HOME", home);
        }
    }
    Ok(cmd)
}
fn jsonl(path: &Path) -> Vec<Value> {
    fs::read_to_string(path).unwrap_or_default().split('\n').filter_map(|s| serde_json::from_str(s).ok()).collect()
}
fn codex_contexts(model: &Model, dir: &Path) -> Result<Vec<Value>> {
    let events = jsonl(&dir.join("adapter.jsonl"));
    let root = events.iter().find(|e| e["type"] == "thread.started").and_then(|e| e["thread_id"].as_str()).ok_or("no root thread")?;
    let home = model
        .codex_home
        .clone()
        .or_else(|| std::env::var("CODEX_HOME").ok())
        .unwrap_or_else(|| format!("{}/.codex", std::env::var("HOME").unwrap_or_default()));
    let mut logs = vec![];
    let mut dirs = vec![PathBuf::from(home).join("sessions")];
    let mut visited = 0;
    while let Some(d) = dirs.pop() {
        for e in fs::read_dir(d)?.flatten() {
            visited += 1;
            require(visited <= 100000, "session scan limit exceeded")?;
            if e.file_type()?.is_dir() {
                dirs.push(e.path());
                continue;
            }
            if e.path().extension().map_or(true, |s| s != "jsonl") {
                continue;
            }
            let mut first = String::new();
            std::io::BufReader::new(fs::File::open(e.path())?).read_line(&mut first)?;
            if let Ok(v) = serde_json::from_str::<Value>(&first) {
                logs.push((e.path(), v["payload"].clone()));
            }
        }
    }
    let mut ids = BTreeSet::from([root.to_owned()]);
    loop {
        let n = ids.len();
        for (_, meta) in &logs {
            if meta["parent_thread_id"].as_str().is_some_and(|p| ids.contains(p)) {
                if let Some(id) = meta["id"].as_str() {
                    ids.insert(id.to_owned());
                }
            }
        }
        if ids.len() == n {
            break;
        }
    }
    let mut out = vec![];
    for (path, meta) in logs.into_iter().filter(|(_, m)| m["id"].as_str().is_some_and(|s| ids.contains(s))) {
        let events = jsonl(&path);
        let mut usage = Value::Null;
        let mut complete = false;
        let mut stable = true;
        let mut settings = vec![];
        let mut last = 0;
        let mut spawns = 0;
        for e in &events {
            let p = &e["payload"];
            if e["type"] == "compacted" {
                stable = false;
            }
            if e["type"] == "turn_context" {
                settings.push(json!({"model":p["model"],"effort":p["effort"]}));
            }
            if e["type"] == "response_item"
                && p["type"] == "function_call"
                && p["name"].as_str().is_some_and(|s| s.ends_with("spawn_agent"))
            {
                spawns += 1;
            }
            if e["type"] == "event_msg" && p["type"] == "task_complete" {
                complete = true;
            }
            if e["type"] == "event_msg" && ["task_started", "turn_aborted"].contains(&p["type"].as_str().unwrap_or("")) {
                complete = false;
            }
            if e["type"] == "event_msg" && p["type"] == "token_count" && p["info"]["total_token_usage"].is_object() {
                let current = p["info"]["total_token_usage"]["total_tokens"].as_u64().unwrap_or(0);
                if current < last {
                    stable = false;
                }
                last = current;
                usage = p["info"]["total_token_usage"].clone();
            }
        }
        let valid = !settings.is_empty() && settings.iter().all(|s| s["model"] == model.id && s["effort"] == settings[0]["effort"]);
        out.push(json!({"id":meta["id"],"parent":meta["parent_thread_id"],"model":settings.first().map(|s|s["model"].clone()),
            "effort":settings.first().map(|s|s["effort"].clone()),"complete":complete&&stable,"settings_consistent":valid,"cli_version":meta["cli_version"],
            "usage":usage,"spawn_calls":spawns,"native_path":path,"native_sha256":hash(&fs::read(path)?)}));
    }
    Ok(out)
}
pub fn account(model: &Model, cell: &Cell, dir: &Path) -> Value {
    let contexts = if model.adapter == "codex_exec" {
        match codex_contexts(model, dir) {
            Ok(c) => c,
            Err(e) => return json!({"settings_valid":false,"error":e.to_string()}),
        }
    } else {
        jsonl(&dir.join("adapter.jsonl")).into_iter().filter(|e| e["type"] == "benchmark.context").collect()
    };
    let mut ids = BTreeSet::new();
    let mut valid = contexts.len() == cell.workers + 1;
    let mut exact = valid;
    let mut totals = [0u64; 3];
    let roots: Vec<_> = contexts.iter().filter(|c| c["parent"].is_null()).collect();
    valid &= roots.len() == 1;
    for c in &contexts {
        valid &= c["id"].as_str().is_some_and(|id| !id.is_empty() && ids.insert(id.to_owned()))
            && c["model"] == model.id
            && c["effort"] == cell.effort.as_deref().unwrap_or("")
            && c["settings_consistent"] != false;
        if !c["parent"].is_null() {
            valid &= roots.first().is_some_and(|r| c["parent"] == r["id"]);
        }
        exact &= c["complete"] == true;
        for (i, k) in ["input_tokens", "cached_input_tokens", "output_tokens"].iter().enumerate() {
            if let Some(n) = c["usage"][k].as_u64() {
                if let Some(sum) = totals[i].checked_add(n) {
                    totals[i] = sum;
                } else {
                    exact = false;
                }
            } else {
                exact = false;
            }
        }
        exact &= c["usage"]["cached_input_tokens"].as_u64().zip(c["usage"]["input_tokens"].as_u64()).is_some_and(|(a, b)| a <= b);
        if model.adapter == "codex_exec" {
            valid &= model.expected_version.as_deref().and_then(|v| v.strip_prefix("codex-cli ")).is_some_and(|v| c["cli_version"] == v);
            if !c["parent"].is_null() {
                valid &= c["spawn_calls"] == 0;
            }
        }
        if c["usage"]["cache_write_input_tokens"].as_u64().unwrap_or(0) > 0 {
            exact = false;
        }
    }
    if model.adapter == "codex_exec" {
        valid &= roots.first().is_some_and(|r| r["spawn_calls"].as_u64() == Some(cell.workers as u64));
    }
    let cost = if exact && valid {
        model.pricing.as_ref().map(|p| {
            ((totals[0] - totals[1]) as f64 * p.input_per_million
                + totals[1] as f64 * p.cached_per_million
                + totals[2] as f64 * p.output_per_million)
                / 1e6
        })
    } else {
        None
    };
    json!({"contexts":contexts,"settings_valid":valid,"usage_complete":exact,"recorded_input":totals[0],"recorded_cached_input":totals[1],
        "recorded_output":totals[2],"cost_equivalent":cost,"pricing":model.pricing,"billing_adjustments_known":false})
}
