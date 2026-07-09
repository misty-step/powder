use std::{
    env,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use powder_core::{Authority, Card, CardId, CardStatus, DetailLevel, Priority, RunId, RunState};
use powder_store::{RepositoryTier, RepositoryUpsert, RepositoryVisibility, Store};
use serde_json::{json, Value};

const FIXTURE_ACTOR: &str = "eval-fixture";
const AGENT: &str = "codex-eval";
const TARGET_REPO: &str = "eval-target";
const OTHER_REPO: &str = "eval-other";
const SEED_NOW: i64 = 1_700_000_000;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub type EvalResult<T> = Result<T, String>;

#[derive(Debug, Clone)]
pub enum McpCommand {
    Binary(PathBuf),
    CargoRun,
}

impl McpCommand {
    pub fn binary(path: impl Into<PathBuf>) -> Self {
        Self::Binary(path.into())
    }

    pub fn from_env_or_default() -> Self {
        if let Some(path) = env::var_os("POWDER_MCP_BIN") {
            return Self::Binary(PathBuf::from(path));
        }
        if let Some(path) = option_env!("CARGO_BIN_EXE_powder-mcp") {
            return Self::Binary(PathBuf::from(path));
        }
        if let Some(path) = sibling_binary_from_current_exe() {
            return Self::Binary(path);
        }
        Self::CargoRun
    }

    fn command(&self) -> Command {
        match self {
            Self::Binary(path) => Command::new(path),
            Self::CargoRun => {
                let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
                let mut command = Command::new(cargo);
                command.current_dir(workspace_root()).args([
                    "run",
                    "-q",
                    "-p",
                    "powder-mcp",
                    "--bin",
                    "powder-mcp",
                    "--",
                ]);
                command
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioMetric {
    pub scenario: &'static str,
    pub tool_calls: usize,
    pub response_chars: usize,
    pub approx_tokens: usize,
    pub passed: bool,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalReport {
    pub scenarios: Vec<ScenarioMetric>,
}

impl EvalReport {
    pub fn all_passed(&self) -> bool {
        self.scenarios.iter().all(|scenario| scenario.passed)
    }

    pub fn table(&self) -> String {
        let mut table = String::from(
            "| scenario | calls | response chars | ~tokens | result |\n\
             | --- | ---: | ---: | ---: | --- |\n",
        );
        for scenario in &self.scenarios {
            let result = if scenario.passed { "pass" } else { "fail" };
            table.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                scenario.scenario,
                scenario.tool_calls,
                scenario.response_chars,
                scenario.approx_tokens,
                result
            ));
        }
        table
    }

    pub fn failures(&self) -> Vec<&str> {
        self.scenarios
            .iter()
            .filter_map(|scenario| scenario.failure.as_deref())
            .collect()
    }
}

#[derive(Debug, Default)]
struct ScenarioRecorder {
    tool_calls: usize,
    response_chars: usize,
}

impl ScenarioRecorder {
    fn record(&mut self, chars: usize) {
        self.tool_calls += 1;
        self.response_chars += chars;
    }

    fn metric(self, scenario: &'static str, failure: Option<String>) -> ScenarioMetric {
        let approx_tokens = self.response_chars.div_ceil(4);
        ScenarioMetric {
            scenario,
            tool_calls: self.tool_calls,
            response_chars: self.response_chars,
            approx_tokens,
            passed: failure.is_none(),
            failure,
        }
    }
}

pub fn run_eval(command: McpCommand) -> EvalReport {
    let mut temp = TempFixtureRoot::new();
    let scenarios = vec![
        run_grooming_scan(&command, &mut temp),
        run_work_loop(&command, &mut temp),
        run_input_loop(&command, &mut temp),
        run_error_recovery(&command, &mut temp),
    ];
    EvalReport { scenarios }
}

fn run_grooming_scan(command: &McpCommand, temp: &mut TempFixtureRoot) -> ScenarioMetric {
    run_scenario("grooming scan", |recorder| {
        let db_path = temp.db_path("grooming-scan")?;
        seed_grooming_scan(&db_path)?;
        let mut mcp = McpProcess::spawn(command, &db_path)?;

        let response = mcp.call_tool(
            recorder,
            "list_cards",
            json!({"status": "ready", "repo": TARGET_REPO, "limit": 10}),
        )?;
        let payload = response.payload()?;
        let cards = required_array(payload, "cards")?;
        let p0_ids = cards
            .iter()
            .filter(|card| card["priority"] == "p0")
            .map(|card| required_str(card, "id"))
            .collect::<EvalResult<Vec<_>>>()?;
        assert_eq_string_slices(&p0_ids, &["groom-p0-a", "groom-p0-b"])?;
        if cards.iter().any(|card| card["repo"] != TARGET_REPO) {
            return Err("grooming scan returned a card outside the target repo".to_string());
        }

        mcp.shutdown()?;
        Ok(())
    })
}

fn run_work_loop(command: &McpCommand, temp: &mut TempFixtureRoot) -> ScenarioMetric {
    run_scenario("work loop", |recorder| {
        let db_path = temp.db_path("work-loop")?;
        seed_work_loop(&db_path)?;
        let mut mcp = McpProcess::spawn(command, &db_path)?;

        let claimed = mcp.call_tool(
            recorder,
            "manage_claim",
            json!({
                "card_id": "work-loop",
                "action": "claim",
                "agent": AGENT,
                "ttl_seconds": 600
            }),
        )?;
        let run_id = required_str(claimed.payload()?, "run_id")?;

        mcp.call_tool(
            recorder,
            "append_work_log",
            json!({
                "card_id": "work-loop",
                "agent": AGENT,
                "model": "eval",
                "harness": "powder-mcp-eval",
                "run_id": run_id,
                "body": "Implemented deterministic proof path for eval fixture."
            }),
        )?;
        mcp.call_tool(
            recorder,
            "check_criterion",
            json!({"card_id": "work-loop", "criterion": 0, "actor": AGENT}),
        )?;
        let completed = mcp.call_tool(
            recorder,
            "complete_card",
            json!({
                "card_id": "work-loop",
                "proof": "https://example.test/powder-mcp-eval/work-loop"
            }),
        )?;
        expect_eq(
            completed.payload()?["status"].as_str(),
            Some("done"),
            "complete_card status",
        )?;

        mcp.shutdown()?;
        assert_work_loop_end_state(&db_path, &run_id)?;
        Ok(())
    })
}

fn run_input_loop(command: &McpCommand, temp: &mut TempFixtureRoot) -> ScenarioMetric {
    run_scenario("input loop", |recorder| {
        let db_path = temp.db_path("input-loop")?;
        let run_id = seed_input_loop(&db_path)?;
        let mut mcp = McpProcess::spawn(command, &db_path)?;

        mcp.call_tool(
            recorder,
            "request_input",
            json!({
                "run_id": run_id,
                "question": "Should the eval fixture continue?"
            }),
        )?;
        let awaiting = mcp.call_tool(recorder, "list_awaiting_input", json!({"limit": 10}))?;
        let awaiting_payload = awaiting.payload()?;
        let awaiting_items = awaiting_payload
            .as_array()
            .ok_or_else(|| "list_awaiting_input payload is not an array".to_string())?;
        if !awaiting_items.iter().any(|item| {
            item["run"]["id"] == run_id
                && item["question"]["payload"] == "Should the eval fixture continue?"
        }) {
            return Err("awaiting-input list did not include the requested question".to_string());
        }

        mcp.call_tool(
            recorder,
            "answer_input",
            json!({
                "run_id": run_id,
                "actor": "operator",
                "answer": "Approved for eval baseline."
            }),
        )?;
        let run = mcp.call_tool(
            recorder,
            "get_run",
            json!({"run_id": run_id, "detail": "detailed"}),
        )?;
        let run_payload = run.payload()?;
        expect_eq(
            run_payload["run"]["state"].as_str(),
            Some("active"),
            "get_run state",
        )?;
        expect_eq(
            run_payload["card"]["status"].as_str(),
            Some("running"),
            "get_run card status",
        )?;
        let activities = run_payload["activities"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if !activities.iter().any(|activity| {
            activity["payload"] == "answered by operator: Approved for eval baseline."
        }) {
            return Err("get_run readback did not include the operator answer".to_string());
        }

        mcp.shutdown()?;
        assert_input_loop_end_state(&db_path, &run_id)?;
        Ok(())
    })
}

fn run_error_recovery(command: &McpCommand, temp: &mut TempFixtureRoot) -> ScenarioMetric {
    run_scenario("error recovery", |recorder| {
        let db_path = temp.db_path("error-recovery")?;
        seed_error_recovery(&db_path)?;
        let mut mcp = McpProcess::spawn(command, &db_path)?;

        let invalid = mcp.call_tool(
            recorder,
            "update_status",
            json!({"card_id": "error-recovery", "status": "invalid-now"}),
        )?;
        let error = invalid
            .error_message()
            .ok_or_else(|| "invalid status call unexpectedly succeeded".to_string())?;
        if !error.contains("invalid status") || !error.contains("ready") {
            return Err(format!("invalid status error was not actionable: {error}"));
        }

        let corrected = mcp.call_tool(
            recorder,
            "update_status",
            json!({
                "card_id": "error-recovery",
                "status": "blocked",
                "actor": AGENT
            }),
        )?;
        expect_eq(
            corrected.payload()?["status"].as_str(),
            Some("blocked"),
            "corrected status",
        )?;

        mcp.shutdown()?;
        assert_card_status(&db_path, "error-recovery", CardStatus::Blocked)?;
        Ok(())
    })
}

fn run_scenario(
    scenario: &'static str,
    run: impl FnOnce(&mut ScenarioRecorder) -> EvalResult<()>,
) -> ScenarioMetric {
    let mut recorder = ScenarioRecorder::default();
    match run(&mut recorder) {
        Ok(()) => recorder.metric(scenario, None),
        Err(err) => recorder.metric(scenario, Some(err)),
    }
}

fn seed_grooming_scan(db_path: &Path) -> EvalResult<()> {
    with_seed_store(db_path, |store| {
        upsert_active_repository(store, TARGET_REPO)?;
        upsert_active_repository(store, OTHER_REPO)?;
        seed_card(
            store,
            CardSeed::new("groom-p0-a", "P0 target ready A", CardStatus::Ready)
                .priority(Priority::P0)
                .repo(TARGET_REPO)
                .acceptance(&["target proof A"])
                .created_at(SEED_NOW),
        )?;
        seed_card(
            store,
            CardSeed::new("groom-p0-b", "P0 target ready B", CardStatus::Ready)
                .priority(Priority::P0)
                .repo(TARGET_REPO)
                .acceptance(&["target proof B"])
                .created_at(SEED_NOW + 1),
        )?;
        seed_card(
            store,
            CardSeed::new("groom-p1", "P1 target ready", CardStatus::Ready)
                .priority(Priority::P1)
                .repo(TARGET_REPO)
                .acceptance(&["target proof C"])
                .created_at(SEED_NOW + 2),
        )?;
        seed_card(
            store,
            CardSeed::new("groom-other", "P0 other repo ready", CardStatus::Ready)
                .priority(Priority::P0)
                .repo(OTHER_REPO)
                .acceptance(&["other proof"])
                .created_at(SEED_NOW + 3),
        )?;
        seed_card(
            store,
            CardSeed::new("groom-blocked", "P0 target blocked", CardStatus::Blocked)
                .priority(Priority::P0)
                .repo(TARGET_REPO)
                .acceptance(&["blocked proof"])
                .created_at(SEED_NOW + 4),
        )?;
        Ok(())
    })
}

fn seed_work_loop(db_path: &Path) -> EvalResult<()> {
    with_seed_store(db_path, |store| {
        upsert_active_repository(store, TARGET_REPO)?;
        seed_card(
            store,
            CardSeed::new("work-loop", "Work loop card", CardStatus::Ready)
                .priority(Priority::P0)
                .repo(TARGET_REPO)
                .acceptance(&["criterion can be checked"])
                .created_at(SEED_NOW),
        )
    })
}

fn seed_input_loop(db_path: &Path) -> EvalResult<String> {
    let mut run_id = None;
    with_seed_store(db_path, |store| {
        upsert_active_repository(store, TARGET_REPO)?;
        seed_card(
            store,
            CardSeed::new("input-loop", "Input loop card", CardStatus::Ready)
                .priority(Priority::P0)
                .repo(TARGET_REPO)
                .acceptance(&["operator input is handled"])
                .created_at(SEED_NOW),
        )?;
        let receipt = store
            .claim_card(
                &card_id("input-loop")?,
                AGENT,
                SEED_NOW + 1,
                600,
                &Authority::unchecked(),
            )
            .map_err(to_string)?;
        run_id = Some(receipt.run_id.to_string());
        Ok(())
    })?;
    run_id.ok_or_else(|| "input-loop setup did not create a run".to_string())
}

fn seed_error_recovery(db_path: &Path) -> EvalResult<()> {
    with_seed_store(db_path, |store| {
        upsert_active_repository(store, TARGET_REPO)?;
        seed_card(
            store,
            CardSeed::new("error-recovery", "Error recovery card", CardStatus::Ready)
                .priority(Priority::P0)
                .repo(TARGET_REPO)
                .acceptance(&["status can be corrected"])
                .created_at(SEED_NOW),
        )
    })
}

fn with_seed_store(
    db_path: &Path,
    seed: impl FnOnce(&mut Store) -> EvalResult<()>,
) -> EvalResult<()> {
    let mut store = Store::open(db_path).map_err(to_string)?;
    store.migrate().map_err(to_string)?;
    seed(&mut store)
}

struct CardSeed<'a> {
    id: &'a str,
    title: &'a str,
    status: CardStatus,
    priority: Priority,
    repo: Option<&'a str>,
    acceptance: &'a [&'a str],
    created_at: i64,
}

impl<'a> CardSeed<'a> {
    fn new(id: &'a str, title: &'a str, status: CardStatus) -> Self {
        Self {
            id,
            title,
            status,
            priority: Priority::P2,
            repo: None,
            acceptance: &[],
            created_at: SEED_NOW,
        }
    }

    fn priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    fn repo(mut self, repo: &'a str) -> Self {
        self.repo = Some(repo);
        self
    }

    fn acceptance(mut self, acceptance: &'a [&'a str]) -> Self {
        self.acceptance = acceptance;
        self
    }

    fn created_at(mut self, created_at: i64) -> Self {
        self.created_at = created_at;
        self
    }
}

fn seed_card(store: &mut Store, seed: CardSeed<'_>) -> EvalResult<()> {
    let mut card = Card::new(
        card_id(seed.id)?,
        seed.title,
        format!("Fixture body for {}", seed.id),
    )
    .map_err(to_string)?
    .with_status(seed.status)
    .with_priority(seed.priority)
    .with_acceptance(seed.acceptance.iter().map(|item| (*item).to_string()))
    .with_created_at(seed.created_at);
    card.repo = seed.repo.map(ToOwned::to_owned);
    store
        .create_card_with_events(card, FIXTURE_ACTOR, seed.created_at)
        .map_err(to_string)?;
    Ok(())
}

fn upsert_active_repository(store: &mut Store, name: &str) -> EvalResult<()> {
    store
        .upsert_repository(
            RepositoryUpsert {
                name: name.to_string(),
                aliases: None,
                visibility: Some(RepositoryVisibility::Visible),
                tier: Some(RepositoryTier::Active),
                import_provenance: Some("powder-mcp eval fixture".to_string()),
            },
            SEED_NOW,
        )
        .map_err(to_string)?;
    Ok(())
}

fn assert_work_loop_end_state(db_path: &Path, run_id: &str) -> EvalResult<()> {
    let store = Store::open(db_path).map_err(to_string)?;
    let detail = store
        .get_card_detail(&card_id("work-loop")?, DetailLevel::Detailed)
        .map_err(to_string)?
        .ok_or_else(|| "work-loop card missing after scenario".to_string())?;
    if detail.card.status != CardStatus::Done {
        return Err(format!(
            "work-loop card status was {}, expected done",
            detail.card.status.as_str()
        ));
    }
    if detail.card.claim.is_some() {
        return Err("work-loop card still had an active claim after completion".to_string());
    }
    let checked = detail
        .card
        .criteria
        .first()
        .and_then(|criterion| criterion.checked_by.as_deref());
    expect_eq(checked, Some(AGENT), "checked criterion actor")?;
    if !detail.work_log.iter().any(|entry| {
        entry.agent == AGENT
            && entry
                .run_id
                .as_ref()
                .is_some_and(|id| id.as_str() == run_id)
    }) {
        return Err("work-loop work_log entry was not persisted with the run id".to_string());
    }
    let run_id = run_id_value(run_id)?;
    let run = store
        .get_run_detail(&run_id, DetailLevel::Detailed)
        .map_err(to_string)?
        .ok_or_else(|| "work-loop run missing after completion".to_string())?;
    if run.run.state != RunState::Complete {
        return Err(format!(
            "work-loop run state was {}, expected complete",
            run.run.state.as_str()
        ));
    }
    expect_eq(
        run.run.proof.as_deref(),
        Some("https://example.test/powder-mcp-eval/work-loop"),
        "work-loop proof",
    )
}

fn assert_input_loop_end_state(db_path: &Path, run_id: &str) -> EvalResult<()> {
    let store = Store::open(db_path).map_err(to_string)?;
    let run_id = run_id_value(run_id)?;
    let run = store
        .get_run_detail(&run_id, DetailLevel::Detailed)
        .map_err(to_string)?
        .ok_or_else(|| "input-loop run missing after scenario".to_string())?;
    if run.run.state != RunState::Active {
        return Err(format!(
            "input-loop run state was {}, expected active",
            run.run.state.as_str()
        ));
    }
    if run.card.status != CardStatus::Running {
        return Err(format!(
            "input-loop card status was {}, expected running",
            run.card.status.as_str()
        ));
    }
    Ok(())
}

fn assert_card_status(db_path: &Path, id: &str, expected: CardStatus) -> EvalResult<()> {
    let store = Store::open(db_path).map_err(to_string)?;
    let card = store
        .get_card(&card_id(id)?)
        .map_err(to_string)?
        .ok_or_else(|| format!("card missing after scenario: {id}"))?;
    if card.status == expected {
        Ok(())
    } else {
        Err(format!(
            "{id} status was {}, expected {}",
            card.status.as_str(),
            expected.as_str()
        ))
    }
}

struct ToolResponse {
    payload: Option<Value>,
    error: Option<String>,
}

impl ToolResponse {
    fn payload(&self) -> EvalResult<&Value> {
        self.payload
            .as_ref()
            .ok_or_else(|| format!("tool call failed: {}", self.error_message().unwrap_or("")))
    }

    fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: Option<ChildStderr>,
    next_id: u64,
    finished: bool,
}

impl McpProcess {
    fn spawn(command: &McpCommand, db_path: &Path) -> EvalResult<Self> {
        let mut command = command.command();
        let mut child = command
            .env("POWDER_DB_PATH", db_path)
            .env_remove("POWDER_API_BASE_URL")
            .env_remove("POWDER_API_KEY")
            .env_remove("POWDER_BACKLOG_DIR")
            .env_remove("POWDER_MCP_TOOLSETS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("spawn powder-mcp: {err}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "powder-mcp stdin was not piped".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "powder-mcp stdout was not piped".to_string())?;
        let stderr = child.stderr.take();
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            stderr,
            next_id: 1,
            finished: false,
        })
    }

    fn call_tool(
        &mut self,
        recorder: &mut ScenarioRecorder,
        name: &str,
        args: Value,
    ) -> EvalResult<ToolResponse> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": args,
            }
        });
        let line = serde_json::to_string(&request).map_err(to_string)?;
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "powder-mcp stdin is closed".to_string())?;
        writeln!(stdin, "{line}").map_err(to_string)?;
        stdin.flush().map_err(to_string)?;

        let mut response_line = String::new();
        let read = self
            .stdout
            .read_line(&mut response_line)
            .map_err(to_string)?;
        if read == 0 {
            return Err(format!("powder-mcp closed stdout while handling {name}"));
        }
        let response = serde_json::from_str::<Value>(&response_line).map_err(|err| {
            format!(
                "powder-mcp returned invalid JSON for {name}: {err}; line={}",
                response_line.trim()
            )
        })?;
        if response["id"].as_u64() != Some(id) {
            return Err(format!(
                "powder-mcp response id mismatch for {name}: expected {id}, got {}",
                response["id"]
            ));
        }

        let tool_response = parse_tool_response(&response)?;
        recorder.record(visible_response_chars(&tool_response));
        Ok(tool_response)
    }

    fn shutdown(&mut self) -> EvalResult<()> {
        drop(self.stdin.take());
        let status = self.child.wait().map_err(to_string)?;
        self.finished = true;
        let mut stderr = String::new();
        if let Some(mut pipe) = self.stderr.take() {
            pipe.read_to_string(&mut stderr).map_err(to_string)?;
        }
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "powder-mcp exited with {status}; stderr={}",
                stderr.trim()
            ))
        }
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        drop(self.stdin.take());
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
            Err(_) => {}
        }
    }
}

fn parse_tool_response(response: &Value) -> EvalResult<ToolResponse> {
    if let Some(error) = response.get("error") {
        let message = error["message"].as_str().unwrap_or("unknown error");
        return Ok(ToolResponse {
            payload: None,
            error: Some(message.to_string()),
        });
    }

    let text = response["result"]["content"]
        .as_array()
        .and_then(|content| content.first())
        .and_then(|content| content["text"].as_str())
        .ok_or_else(|| format!("tool response missing content text: {response}"))?;
    let payload = serde_json::from_str(text)
        .map_err(|err| format!("tool response content was not JSON: {err}; text={text}"))?;
    Ok(ToolResponse {
        payload: Some(payload),
        error: None,
    })
}

fn visible_response_chars(response: &ToolResponse) -> usize {
    match (&response.payload, &response.error) {
        (Some(payload), None) => payload.to_string().chars().count(),
        (None, Some(error)) => error.chars().count(),
        _ => 0,
    }
}

struct TempFixtureRoot {
    path: PathBuf,
}

impl TempFixtureRoot {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "powder-mcp-eval-{}-{nonce}-{counter}",
            std::process::id()
        ));
        Self { path }
    }

    fn db_path(&mut self, scenario: &str) -> EvalResult<PathBuf> {
        fs::create_dir_all(&self.path).map_err(to_string)?;
        Ok(self.path.join(format!("{scenario}.db")))
    }
}

impl Drop for TempFixtureRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn sibling_binary_from_current_exe() -> Option<PathBuf> {
    let exe_name = format!("powder-mcp{}", env::consts::EXE_SUFFIX);
    let current = env::current_exe().ok()?;
    current.ancestors().find_map(|ancestor| {
        let dirname = ancestor.file_name()?.to_str()?;
        if dirname == "debug" || dirname == "release" {
            let candidate = ancestor.join(&exe_name);
            candidate.exists().then_some(candidate)
        } else {
            None
        }
    })
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")))
        .to_path_buf()
}

fn required_array<'a>(value: &'a Value, key: &str) -> EvalResult<&'a Vec<Value>> {
    value[key]
        .as_array()
        .ok_or_else(|| format!("{key} is missing or not an array"))
}

fn required_str(value: &Value, key: &str) -> EvalResult<String> {
    value[key]
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("{key} is missing or not a string"))
}

fn expect_eq(actual: Option<&str>, expected: Option<&str>, label: &str) -> EvalResult<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label} was {:?}, expected {:?}", actual, expected))
    }
}

fn assert_eq_string_slices(actual: &[String], expected: &[&str]) -> EvalResult<()> {
    let expected = expected.iter().map(ToString::to_string).collect::<Vec<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err(format!("expected ids {expected:?}, got {actual:?}"))
    }
}

fn card_id(raw: &str) -> EvalResult<CardId> {
    CardId::new(raw).map_err(to_string)
}

fn run_id_value(raw: &str) -> EvalResult<RunId> {
    RunId::new(raw).map_err(to_string)
}

fn to_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}
