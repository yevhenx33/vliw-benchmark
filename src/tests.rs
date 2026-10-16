use super::*;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    thread,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("harness-bench-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn file(path: &str, text: &str) -> FrozenFile {
    FrozenFile { path: path.into(), text: text.into(), sha256: hash(text.as_bytes()) }
}
fn fixture() -> (Config, Challenge) {
    let config:Config=serde_json::from_value(json!({"schema_version":1,"seconds":1,"repeats":3,"seed":41,"tiers":["light","high"],"presets":{"solo":[0],"teams":[0,1,2,3,4,5]},
        "models":[{"id":"fixture-model","adapter":"command_v1","program":"/bin/sh","args":["-c","cat >/dev/null; printf '%s\\n' '{\"type\":\"benchmark.context\",\"id\":\"root\",\"parent\":null,\"model\":\"fixture-model\",\"effort\":\"{effort}\",\"complete\":true,\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":50,\"output_tokens\":10}}'"],"tiers":{"light":"low","high":"high"},"max_workers":5,"pricing":{"label":"fixture","input_per_million":2,"cached_per_million":0.1,"output_per_million":10}}]})).unwrap();
    let ch = Challenge {
        schema_version: 1,
        id: "number".into(),
        version: "1".into(),
        prompt: "Return the number 7 using supplied checker.".into(),
        public_files: vec![file("candidate.txt", "7\n"), file("instructions.txt", "Do not edit")],
        judge_files: vec![file(
            "judge.sh",
            "test \"$(cat candidate.txt)\" = 7 || exit 1\nprintf '%s' '{\"correct\":true,\"value\":7}' > score.json\n",
        )],
        protected: vec!["instructions.txt".into()],
        candidate: "candidate.txt".into(),
        submission: "selected.txt".into(),
        receipt: "selected.json".into(),
        submission_target: "candidate.txt".into(),
        added_lines_per_candidate: 5,
        judge_commands: vec![vec!["/bin/sh".into(), "{judge}/judge.sh".into()]],
        judge_seconds: 1,
        score_file: "score.json".into(),
        score_pointer: "/value".into(),
        direction: "min".into(),
        initial_receipt: json!({}),
        provenance: Value::Null,
    };
    (config, ch)
}
#[test]
fn matrices_repeats_and_unsupported_are_explicit() {
    let (mut c, ch) = fixture();
    c.tiers = vec!["light".into(), "medium".into(), "high".into(), "extra-high".into()];
    validate(&c, &ch, "teams").unwrap();
    let grid = cells(&c, "teams");
    assert_eq!(grid.len(), 72);
    assert_eq!(cells(&c, "solo").len(), 12);
    assert_eq!(grid.iter().filter(|x| x.unavailable.is_some()).count(), 36);
    assert_eq!(grid.iter().map(|x| &x.id).collect::<BTreeSet<_>>().len(), 72);
    assert_eq!(serde_json::to_value(&grid).unwrap(), serde_json::to_value(cells(&c, "teams")).unwrap());
    c.repeats = 1;
    assert_eq!(cells(&c, "solo").len(), 4);
    c.models[0].max_workers = 0;
    assert!(cells(&c, "teams").iter().filter(|x| x.workers > 0).all(|x| x.unavailable.is_some()));
}
#[test]
fn plan_does_not_execute_adapter() {
    let t = Temp::new();
    let (mut c, ch) = fixture();
    c.models[0].program = "/nonexistent-model-command".into();
    let config = t.0.join("config.json");
    let challenge = t.0.join("challenge.json");
    save(&config, &c).unwrap();
    save(&challenge, &ch).unwrap();
    cli(&["bench".into(), "plan".into(), config.display().to_string(), "solo".into(), challenge.display().to_string()]).unwrap();
    assert_eq!(fs::read_dir(&t.0).unwrap().count(), 2);
}
#[test]
fn invalid_contracts_and_changed_seals_fail_before_execution() {
    let t = Temp::new();
    let (c, mut ch) = fixture();
    ch.public_files[0].text = "bad".into();
    assert!(validate(&c, &ch, "solo").is_err());
    ch.public_files[0] = file("../escape", "7");
    assert!(validate(&c, &ch, "solo").is_err());
    let (c, ch) = fixture();
    let root = t.0.join("campaign");
    prepare(c, ch, "solo", &root).unwrap();
    frozen_plan(&root).unwrap();
    fs::write(root.join("plan.sha256"), "wrong").unwrap();
    assert!(frozen_plan(&root).is_err());
}
#[test]
fn repeated_solo_campaign_and_second_challenge_use_same_runner() {
    let t = Temp::new();
    let (c, ch) = fixture();
    let root = t.0.join("campaign");
    prepare(c, ch, "solo", &root).unwrap();
    run(&root).unwrap();
    let plan: Plan = load(&root.join("plan.json")).unwrap();
    assert_eq!(plan.cells.len(), 6);
    for cell in &plan.cells {
        let r: Value = load(&root.join(format!("{}.json", cell.id))).unwrap();
        assert_eq!(r["valid"], true, "{r}");
        assert_eq!(r["grading"]["score"], 7.0);
        assert!(r["accounting"]["cost_equivalent"].as_f64().unwrap() > 0.0);
    }
    assert!(run(&root).is_err());
    let (_, mut ch) = fixture();
    ch.id = "other-problem".into();
    ch.direction = "max".into();
    let stage = t.0.join("second-stage");
    let judge = t.0.join("second-judge");
    materialize(&judge, &ch.judge_files).unwrap();
    assert_eq!(grade(&ch, &judge, &stage, b"7\n").unwrap()["score"], 7.0);
}
#[test]
fn deadline_and_normal_exit_kill_late_writers() {
    let t = Temp::new();
    fs::write(t.0.join("selected"), "early").unwrap();
    for (n, script) in ["(sleep 0.2; printf late > selected) & sleep 5", "(sleep 0.2; printf late > selected) &"].iter().enumerate() {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", script]).current_dir(&t.0);
        let r = adapters::bounded(
            c,
            Duration::from_millis(60),
            &t.0,
            &format!("child{n}"),
            Some((&t.0.join("selected"), &t.0.join("snapshot"), &t.0.join("receipt"))),
        )
        .unwrap();
        assert_eq!(r["stop"], if n == 0 { "deadline" } else { "completed" });
        thread::sleep(Duration::from_millis(250));
        assert_eq!(fs::read(t.0.join("selected")).unwrap(), b"early");
        assert_eq!(fs::read(t.0.join("snapshot")).unwrap(), b"early");
    }
}
#[test]
fn missing_artifact_still_retains_model_cost() {
    let t = Temp::new();
    let (mut c, ch) = fixture();
    c.models[0].args[1].push_str("; rm selected.txt");
    c.tiers = vec!["light".into()];
    let root = t.0.join("campaign");
    prepare(c, ch, "solo", &root).unwrap();
    run(&root).unwrap();
    let p: Plan = load(&root.join("plan.json")).unwrap();
    let r: Value = load(&root.join(format!("{}.json", p.cells[0].id))).unwrap();
    assert_eq!(r["valid"], false);
    assert!(r["accounting"]["cost_equivalent"].as_f64().unwrap() > 0.0);
    assert_eq!(report_value(&root).unwrap()["groups"][0]["unfinished"], 2);
}
#[test]
fn incomplete_duplicate_and_wrong_settings_never_get_a_price() {
    let t = Temp::new();
    let (c, _) = fixture();
    let cell = cells(&c, "solo").remove(0);
    let model = &c.models[0];
    let mut event = json!({"type":"benchmark.context","id":"root","parent":null,"model":model.id,"effort":cell.effort,"complete":true,"usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":10}});
    let write =
        |events: &[Value]| fs::write(t.0.join("adapter.jsonl"), events.iter().map(|e| format!("{e}\n")).collect::<String>()).unwrap();
    write(&[event.clone()]);
    assert!(adapters::account(model, &cell, &t.0)["cost_equivalent"].as_f64().is_some());
    event["complete"] = json!(false);
    write(&[event.clone()]);
    assert!(adapters::account(model, &cell, &t.0)["cost_equivalent"].is_null());
    event["complete"] = json!(true);
    event["effort"] = json!("wrong");
    write(&[event.clone()]);
    assert_eq!(adapters::account(model, &cell, &t.0)["settings_valid"], false);
    event["effort"] = json!(cell.effort);
    write(&[event.clone(), event]);
    assert_eq!(adapters::account(model, &cell, &t.0)["settings_valid"], false);
}
#[test]
fn protected_edits_wrong_results_and_symlinks_are_rejected() {
    let t = Temp::new();
    let (_, ch) = fixture();
    let judge = t.0.join("judge");
    materialize(&judge, &ch.judge_files).unwrap();
    assert_eq!(grade(&ch, &judge, &t.0.join("bad-stage"), b"8").unwrap()["correct"], false);
    fs::write(judge.join("judge.sh"), "exit 0").unwrap();
    assert!(grade(&ch, &judge, &t.0.join("changed-stage"), b"7").is_err());
    std::os::unix::fs::symlink("/etc/passwd", t.0.join("link")).unwrap();
    assert!(plain_bytes(&t.0.join("link")).is_err());
    let (c, mut ch) = fixture();
    ch.public_files.push(file("selected.txt", "x"));
    ch.protected.push("selected.txt".into());
    assert!(validate(&c, &ch, "solo").is_err());
}
#[test]
fn frozen_vliw_manifest_and_judge_preserve_legacy_rules() {
    let ch: Challenge = serde_json::from_str(include_str!("../challenges/vliw.json")).unwrap();
    let (c, _) = fixture();
    validate(&c, &ch, "solo").unwrap();
    let seeds = &ch.judge_files.iter().find(|f| f.path == "host/heldout-seeds.json").unwrap().text;
    assert_eq!(serde_json::from_str::<Vec<u64>>(seeds).unwrap().len(), 9);
    let old = ch.judge_files[0]
        .text
        .replace("BASE = pathlib.Path(__file__).resolve().parent", "BASE = pathlib.Path('/tmp/harness-vliw-factorial-_qsidhtf')");
    assert_eq!(hash(old.as_bytes()), ch.provenance["original_judge_sha256"]);
}
#[test]
fn repeated_lines_count_as_additions_and_reports_preserve_failures() {
    assert!(!within_allowance("a\nb\n", "a\na\na\nb\n", 1));
    let t = Temp::new();
    let (c, ch) = fixture();
    let root = t.0.join("report");
    prepare(c, ch, "solo", &root).unwrap();
    let plan: Plan = load(&root.join("plan.json")).unwrap();
    for cell in &plan.cells {
        let low = cell.tier == "light";
        let score = if low { 8 + cell.repeat * 2 } else { 9 };
        save(
            &root.join(format!("{}.json", cell.id)),
            &json!({"cell":cell,"valid":true,"grading":{"score":score},"accounting":{"cost_equivalent":if low {0.5} else {1.0}}}),
        )
        .unwrap();
    }
    let r = report_value(&root).unwrap();
    let gs = r["groups"].as_array().unwrap();
    assert!(gs.iter().all(|g| g["observed_frontier"] == true));
    assert_eq!(gs.iter().find(|g| g["tier"] == "light").unwrap()["median_score"], 12.0);
    let cell = &plan.cells[0];
    let p = root.join(format!("{}.json", cell.id));
    let mut row: Value = load(&p).unwrap();
    row["valid"] = json!(false);
    save(&p, &row).unwrap();
    let r = report_value(&root).unwrap();
    assert_eq!(r["solver_cost_equivalent_known_sum"], 4.5);
    assert!(r["groups"].as_array().unwrap().iter().any(|g| g["frontier_eligible"] == false));
    fs::write(p, "corrupt").unwrap();
    assert!(report_value(&root).is_err());
}
#[test]
fn native_adapter_audits_worker_identity_and_rejects_compaction_cost() {
    let t = Temp::new();
    let logs = t.0.join("home/sessions/day");
    fs::create_dir_all(&logs).unwrap();
    let (mut c, _) = fixture();
    let m = &mut c.models[0];
    m.adapter = "codex_exec".into();
    m.codex_home = Some(t.0.join("home").display().to_string());
    m.expected_version = Some("codex-cli 0.163.0".into());
    let cell = Cell {
        id: "native".into(),
        model_index: 0,
        tier: "light".into(),
        effort: Some("low".into()),
        workers: 1,
        repeat: 1,
        unavailable: None,
    };
    fs::write(t.0.join("adapter.jsonl"), "{\"type\":\"thread.started\",\"thread_id\":\"root\"}\n").unwrap();
    for (id, parent, spawn) in [("root", Value::Null, 1), ("worker", json!("root"), 0)] {
        let mut events = vec![
            json!({"type":"session_meta","payload":{"id":id,"parent_thread_id":parent,"cli_version":"0.163.0"}}),
            json!({"type":"turn_context","payload":{"model":"fixture-model","effort":"low"}}),
            json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":10,"total_tokens":110}}}}),
            json!({"type":"event_msg","payload":{"type":"task_complete"}}),
        ];
        for _ in 0..spawn {
            events.push(json!({"type":"response_item","payload":{"type":"function_call","name":"spawn_agent"}}));
        }
        fs::write(logs.join(format!("{id}.jsonl")), events.iter().map(|e| format!("{e}\n")).collect::<String>()).unwrap();
    }
    let r = adapters::account(m, &cell, &t.0);
    assert_eq!(r["settings_valid"], true);
    assert_eq!(r["recorded_input"], 200);
    assert!(r["cost_equivalent"].as_f64().is_some());
    use std::io::Write;
    fs::OpenOptions::new().append(true).open(logs.join("worker.jsonl")).unwrap().write_all(b"{\"type\":\"compacted\"}\n").unwrap();
    assert!(adapters::account(m, &cell, &t.0)["cost_equivalent"].is_null());
}

#[test]
fn team_agent_names_match_count_and_tool_grammar() {
    let (config, challenge) = fixture();
    let mut cell = cells(&config, "solo").remove(0);
    for workers in 1..=5 {
        cell.workers = workers;
        let text = prompt(&challenge, &cell, 600);
        let names = text.split("Agent task_name values: ").nth(1).unwrap().split('.').next().unwrap();
        let names: Vec<_> = names.split(", ").collect();
        assert_eq!(names.len(), workers);
        for name in names {
            assert!(!name.is_empty());
            assert!(name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
        }
    }
}
