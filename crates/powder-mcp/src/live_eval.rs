//! Live-model A/B harness (powder-mcp-live-model-ab).
//!
//! The scripted harness in `eval_harness.rs` drives fixed tool calls and
//! proves the wire mechanics work. It cannot tell us whether a real model
//! *chooses* the right tools and arguments -- the efficiency epic cut
//! response tokens on scans by consolidating tools and truncating history,
//! and both of those are exactly the kind of change that can quietly make an
//! agent worse at picking cards or recovering older facts even while every
//! scripted assertion stays green. This module gives an actual model the MCP
//! tool surface over stdio and grades the resulting card/run state -- no LLM
//! judge, just SQLite-backed end states and exact string matches on the
//! model's final reply.
//!
//! # Never touches `cargo test`
//!
//! This module is wired into exactly one entry point: `examples/live_ab_eval.rs`.
//! There is no `#[test]` anywhere in this crate that calls into it, so a
//! plain `cargo test --workspace` never imports `ureq`, never reads
//! `OPENROUTER_API_KEY`, and never opens a socket. `LiveEvalConfig::from_env`
//! returns `None` when neither `POWDER_EVAL_MODEL_API_KEY` nor
//! `OPENROUTER_API_KEY` is set; the example prints a skip message and exits
//! 0 rather than failing or fabricating results.
//!
//! # Producing the "old" (pre-epic) binary
//!
//! The old MCP surface (commit b5e5ecc, before the tool-consolidation epic)
//! is a separately compiled binary, not a Cargo feature -- the epic renamed
//! five claim tools into one `manage_claim`, added `board_stats`, and added
//! `detail` truncation, so this crate's tool table only ever describes one
//! surface at a time. To produce the old binary:
//!
//! ```text
//! git worktree add /tmp/powder-old-mcp b5e5ecc --detach
//! (cd /tmp/powder-old-mcp && cargo build --release -p powder-mcp)
//! export POWDER_EVAL_OLD_BINARY=/tmp/powder-old-mcp/target/release/powder-mcp
//! ```
//!
//! Then `cargo run --example live_ab_eval` (with `OPENROUTER_API_KEY` set)
//! picks it up automatically. Without `POWDER_EVAL_OLD_BINARY` the harness
//! only exercises the new surface and says so in the table footer -- it
//! never silently substitutes the new binary for the old one.

use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};

use crate::eval_harness::McpCommand;

const AGENT_NAME: &str = "eval-agent";
const ROLLBACK_FACT: &str = "2027-03-14T02:00Z";
const DEFAULT_MAX_TOOL_CALLS: usize = 15;
const DEFAULT_TRIALS: usize = 3;
const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

// ---------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Old,
    New,
}

impl Surface {
    pub fn label(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelCandidate {
    pub label: &'static str,
    pub slug: String,
}

pub struct LiveEvalConfig {
    pub api_key: String,
    pub models: Vec<ModelCandidate>,
    pub old_binary: Option<PathBuf>,
    pub trials: usize,
    pub max_tool_calls: usize,
}

impl LiveEvalConfig {
    /// `None` when neither `POWDER_EVAL_MODEL_API_KEY` nor
    /// `OPENROUTER_API_KEY` is set. Never panics, never reads the value into
    /// a log line.
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("POWDER_EVAL_MODEL_API_KEY")
            .or_else(|_| std::env::var("OPENROUTER_API_KEY"))
            .ok()
            .filter(|key| !key.trim().is_empty())?;
        let trials = std::env::var("POWDER_EVAL_TRIALS")
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(DEFAULT_TRIALS);
        let max_tool_calls = std::env::var("POWDER_EVAL_MAX_TOOL_CALLS")
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(DEFAULT_MAX_TOOL_CALLS);
        Some(Self {
            api_key,
            models: models_from_env(),
            old_binary: std::env::var_os("POWDER_EVAL_OLD_BINARY").map(PathBuf::from),
            trials,
            max_tool_calls,
        })
    }
}

fn models_from_env() -> Vec<ModelCandidate> {
    let claude = std::env::var("POWDER_EVAL_MODEL_CLAUDE")
        .unwrap_or_else(|_| "anthropic/claude-haiku-4.5".to_string());
    let open = std::env::var("POWDER_EVAL_MODEL_OPEN")
        .unwrap_or_else(|_| "qwen/qwen3-30b-a3b-instruct-2507".to_string());
    vec![
        ModelCandidate {
            label: "claude",
            slug: claude,
        },
        ModelCandidate {
            label: "open",
            slug: open,
        },
    ]
}

// ---------------------------------------------------------------------
// Minimal MCP stdio transport
//
// Deliberately separate from eval_harness::McpProcess: that type is coupled
// to ScenarioRecorder and a fixed set of typed calls the scripted harness
// issues itself. This one hands raw tool names and arguments chosen by a
// live model to the child process and passes back whatever it says,
// including errors, verbatim -- the model needs to see and recover from
// them, which is a different contract than the scripted harness's
// assert-and-fail-fast calls.
// ---------------------------------------------------------------------

struct LiveMcpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: Option<ChildStderr>,
    next_id: u64,
    finished: bool,
}

struct ToolCallOutcome {
    ok: bool,
    content: String,
}

impl LiveMcpProcess {
    fn spawn(command: &McpCommand, db_path: &Path) -> Result<Self, String> {
        Self::spawn_with_toolset(command, db_path, None)
    }

    /// A repo that has never been registered via `upsert_repository`
    /// defaults to `RepositoryTier::Backburner`, which `claim_card` rejects
    /// outright -- so fixture repos must be registered before a model can
    /// claim anything in them. `upsert_repository` is admin-only, hidden
    /// from the default persona a model sees, so registration happens in a
    /// short-lived admin-toolset session that exits before the model's own
    /// (default-toolset) session opens the same db file.
    fn spawn_admin(command: &McpCommand, db_path: &Path) -> Result<Self, String> {
        Self::spawn_with_toolset(command, db_path, Some("admin"))
    }

    fn spawn_with_toolset(
        command: &McpCommand,
        db_path: &Path,
        toolset_env: Option<&str>,
    ) -> Result<Self, String> {
        let mut command = mcp_command(command);
        command
            .env("POWDER_DB_PATH", db_path)
            .env_remove("POWDER_API_BASE_URL")
            .env_remove("POWDER_API_KEY");
        match toolset_env {
            Some(value) => {
                command.env("POWDER_MCP_TOOLSETS", value);
            }
            None => {
                command.env_remove("POWDER_MCP_TOOLSETS");
            }
        }
        let mut child = command
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

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let line = serde_json::to_string(&request).map_err(|err| err.to_string())?;
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "powder-mcp stdin is closed".to_string())?;
        writeln!(stdin, "{line}").map_err(|err| err.to_string())?;
        stdin.flush().map_err(|err| err.to_string())?;

        let mut response_line = String::new();
        let read = self
            .stdout
            .read_line(&mut response_line)
            .map_err(|err| err.to_string())?;
        if read == 0 {
            return Err(format!("powder-mcp closed stdout while handling {method}"));
        }
        let response: Value = serde_json::from_str(&response_line).map_err(|err| {
            format!(
                "powder-mcp returned invalid JSON for {method}: {err}; line={}",
                response_line.trim()
            )
        })?;
        if let Some(error) = response.get("error") {
            return Err(error["message"]
                .as_str()
                .unwrap_or("unknown error")
                .to_string());
        }
        Ok(response["result"].clone())
    }

    fn list_tools(&mut self) -> Result<Vec<Value>, String> {
        let result = self.request("tools/list", json!({}))?;
        result["tools"]
            .as_array()
            .cloned()
            .ok_or_else(|| "tools/list response missing tools array".to_string())
    }

    fn server_instructions(&mut self) -> Result<Option<String>, String> {
        let result = self.request(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {}}),
        )?;
        Ok(result["instructions"].as_str().map(str::to_string))
    }

    fn call_tool(&mut self, name: &str, args: Value) -> ToolCallOutcome {
        match self.request("tools/call", json!({"name": name, "arguments": args})) {
            Ok(result) => {
                let text = result["content"][0]["text"].as_str().unwrap_or("");
                ToolCallOutcome {
                    ok: true,
                    content: text.to_string(),
                }
            }
            Err(message) => ToolCallOutcome {
                ok: false,
                content: message,
            },
        }
    }

    /// Convenience over `call_tool("get_card", ...)`: always asks for
    /// `detail: "detailed"`. Harmless against the old surface, which has no
    /// `detail` parameter and ignores unrecognized JSON keys.
    fn fetch_card(&mut self, card_id: &str) -> Result<Value, String> {
        let outcome = self.call_tool(
            "get_card",
            json!({"card_id": card_id, "detail": "detailed"}),
        );
        if !outcome.ok {
            return Err(format!("get_card {card_id} failed: {}", outcome.content));
        }
        serde_json::from_str(&outcome.content)
            .map_err(|err| format!("get_card {card_id} payload was not JSON: {err}"))
    }

    fn shutdown(mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
        self.finished = true;
        if let Some(mut pipe) = self.stderr.take() {
            let mut stderr = String::new();
            let _ = pipe.read_to_string(&mut stderr);
        }
    }
}

impl Drop for LiveMcpProcess {
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

fn mcp_command(command: &McpCommand) -> Command {
    match command {
        McpCommand::Binary(path) => Command::new(path),
        McpCommand::CargoRun => {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let mut cmd = Command::new(cargo);
            cmd.current_dir(workspace_root()).args([
                "run",
                "-q",
                "-p",
                "powder-mcp",
                "--bin",
                "powder-mcp",
                "--",
            ]);
            cmd
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")))
        .to_path_buf()
}

// ---------------------------------------------------------------------
// OpenRouter chat-completions client (OpenAI-compatible tool calling)
// ---------------------------------------------------------------------

struct OpenRouterClient {
    api_key: String,
    agent: ureq::Agent,
}

impl OpenRouterClient {
    fn new(api_key: String) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(90))
            .build();
        Self { api_key, agent }
    }

    fn chat(&self, model: &str, messages: &Value, tools: &Value) -> Result<Value, String> {
        let body = json!({
            "model": model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
        });
        let attempt = |body: &Value| -> Result<Value, String> {
            match self
                .agent
                .post(OPENROUTER_ENDPOINT)
                .set("Authorization", &format!("Bearer {}", self.api_key))
                .set("HTTP-Referer", "https://github.com/misty-step/powder")
                .set("X-Title", "powder-mcp-live-model-ab")
                .send_json(body)
            {
                Ok(response) => response
                    .into_json::<Value>()
                    .map_err(|err| format!("openrouter response was not JSON: {err}")),
                Err(ureq::Error::Status(code, response)) => {
                    let text = response.into_string().unwrap_or_default();
                    Err(format!("openrouter returned HTTP {code}: {text}"))
                }
                Err(err) => Err(format!("openrouter request failed: {err}")),
            }
        };
        match attempt(&body) {
            Ok(value) => Ok(value),
            Err(first_err) => {
                std::thread::sleep(Duration::from_secs(2));
                attempt(&body).map_err(|second_err| {
                    format!("openrouter request failed twice: {first_err}; then: {second_err}")
                })
            }
        }
    }
}

fn tools_to_openai_json(tools: &[Value]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool["name"],
                        "description": tool["description"],
                        "parameters": tool["inputSchema"],
                    }
                })
            })
            .collect(),
    )
}

const BASE_SYSTEM_PROMPT: &str = "You are an autonomous agent connected to a Powder work-board MCP server over stdio JSON-RPC. You have the tools listed in this request. Use them to complete the user's task -- prefer the smallest number of calls that gets a correct, verifiable result. Once the task is complete or you have your answer, reply with a final plain-text message and do not call any more tools.";

fn system_prompt(server_instructions: Option<&str>) -> String {
    match server_instructions {
        Some(instructions) if !instructions.trim().is_empty() => {
            format!("{BASE_SYSTEM_PROMPT}\n\nServer operating contract: {instructions}")
        }
        _ => BASE_SYSTEM_PROMPT.to_string(),
    }
}

// ---------------------------------------------------------------------
// Agent loop
// ---------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct AgentRun {
    pub tool_calls: usize,
    pub invalid_calls: usize,
    pub response_chars: usize,
    pub final_text: String,
    pub used_detailed: bool,
    pub tool_call_log: Vec<String>,
}

fn run_agent(
    client: &OpenRouterClient,
    model: &str,
    mcp: &mut LiveMcpProcess,
    tools_json: &Value,
    system_prompt: &str,
    user_task: &str,
    max_tool_calls: usize,
) -> Result<AgentRun, String> {
    let mut messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": user_task}),
    ];
    let mut run = AgentRun::default();

    loop {
        let response = client.chat(model, &Value::Array(messages.clone()), tools_json)?;
        let message = response["choices"][0]["message"].clone();
        if message.is_null() {
            return Err(format!(
                "openrouter response missing choices[0].message: {response}"
            ));
        }
        if let Some(content) = message["content"].as_str() {
            run.response_chars += content.chars().count();
        }

        let tool_calls = message["tool_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if tool_calls.is_empty() {
            run.final_text = message["content"].as_str().unwrap_or_default().to_string();
            break;
        }

        messages.push(message.clone());
        let mut budget_exhausted = false;
        for call in &tool_calls {
            if run.tool_calls >= max_tool_calls {
                budget_exhausted = true;
                break;
            }
            run.tool_calls += 1;
            let call_id = call["id"].as_str().unwrap_or_default().to_string();
            let name = call["function"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let args_raw = call["function"]["arguments"].as_str().unwrap_or("{}");
            let parsed_args = serde_json::from_str::<Value>(args_raw);
            let is_parse_err = parsed_args.is_err();
            let args = parsed_args.unwrap_or_else(|_| json!({}));
            if is_parse_err {
                run.invalid_calls += 1;
            }
            if args["detail"].as_str() == Some("detailed") {
                run.used_detailed = true;
            }
            let outcome = mcp.call_tool(&name, args);
            if !outcome.ok {
                run.invalid_calls += 1;
            }
            run.response_chars += outcome.content.chars().count();
            run.tool_call_log
                .push(format!("{name}:{}", if outcome.ok { "ok" } else { "err" }));
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": outcome.content,
            }));
        }
        if budget_exhausted || run.tool_calls >= max_tool_calls {
            run.final_text = message["content"].as_str().unwrap_or_default().to_string();
            break;
        }
    }

    Ok(run)
}

// ---------------------------------------------------------------------
// Scenarios: seed (direct tool calls, not through the model), task brief,
// and grade (direct end-state tool calls / transcript inspection, not an
// LLM judge).
// ---------------------------------------------------------------------

type SeedFn = fn(&mut LiveMcpProcess) -> Result<(), String>;
type GradeFn = fn(&mut LiveMcpProcess, &AgentRun) -> (bool, String);

struct Scenario {
    name: &'static str,
    task: &'static str,
    seed: SeedFn,
    grade: GradeFn,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "card-selection",
        task: "There are many cards on this board. Find and claim exactly one: the highest-priority ready (unblocked) card in repo 'eval-target' whose title mentions 'auth'. Use agent name 'eval-agent' for the claim. Reply with only the id of the card you claimed.",
        seed: seed_card_selection,
        grade: grade_card_selection,
    },
    Scenario {
        name: "truncation-recovery",
        task: "Card 'trunc-target' has a rollback window timestamp recorded somewhere in its work log history. Find the exact timestamp and reply with only that timestamp string.",
        seed: seed_truncation_recovery,
        grade: grade_truncation_recovery,
    },
    Scenario {
        name: "claim-ergonomics",
        task: "Card 'lifecycle-target' is ready to work. Using agent name 'eval-agent': claim it, append a work log entry noting 'implemented the fix', mark acceptance criterion 0 checked, then complete the card with proof url 'https://example.test/eval-proof'. Reply 'done' once the full lifecycle is complete.",
        seed: seed_claim_ergonomics,
        grade: grade_claim_ergonomics,
    },
];

fn create_seed_card(
    mcp: &mut LiveMcpProcess,
    id: &str,
    title: &str,
    status: &str,
    priority: &str,
    repo: &str,
) -> Result<(), String> {
    let args = json!({
        "id": id,
        "title": title,
        "status": status,
        "priority": priority,
        "repo": repo,
        "acceptance": ["work is verifiably complete"],
    });
    let outcome = mcp.call_tool("create_card", args);
    if !outcome.ok {
        return Err(format!("seed create_card {id} failed: {}", outcome.content));
    }
    Ok(())
}

fn seed_card_selection(mcp: &mut LiveMcpProcess) -> Result<(), String> {
    create_seed_card(
        mcp,
        "sel-target",
        "Rotate auth tokens for staging",
        "ready",
        "P0",
        "eval-target",
    )?;
    // powder-status-vocabulary: blocked-ness is an unresolved `blocked_by`
    // relation, not a status -- the decoy carries Ready status plus a live
    // blocker, so it looks claimable on a naive status scan but is excluded
    // by any eligibility-aware path (list_ready / claim).
    create_seed_card(
        mcp,
        "sel-decoy-blocker",
        "Ship the new auth design first",
        "backlog",
        "P0",
        "eval-target",
    )?;
    create_seed_card(
        mcp,
        "sel-decoy-blocked",
        "Redesign auth login flow",
        "ready",
        "P0",
        "eval-target",
    )?;
    let relations = mcp.call_tool(
        "update_relations",
        json!({
            "card_id": "sel-decoy-blocked",
            "blocked_by": ["sel-decoy-blocker"],
        }),
    );
    if !relations.ok {
        return Err(format!(
            "seed update_relations sel-decoy-blocked failed: {}",
            relations.content
        ));
    }
    create_seed_card(
        mcp,
        "sel-decoy-repo",
        "Patch auth bypass vulnerability",
        "ready",
        "P0",
        "eval-other",
    )?;
    create_seed_card(
        mcp,
        "sel-decoy-lowpri",
        "Improve auth error messages",
        "ready",
        "P2",
        "eval-target",
    )?;
    let priorities = ["P0", "P1", "P2", "P3"];
    for i in 0..16 {
        let repo = if i % 2 == 0 {
            "eval-target"
        } else {
            "eval-other"
        };
        let priority = priorities[i % priorities.len()];
        // Not "sel-filler-{i}": a trailing "-<digits>" suffix makes the
        // store derive a repo from the prefix before the last dash (the
        // legacy numeric-id-prefix convention), which collides with the
        // explicit repo set below and rejects the card.
        create_seed_card(
            mcp,
            &format!("sel-filler{i}"),
            &format!("Improve dashboard widget {i}"),
            "ready",
            priority,
            repo,
        )?;
    }
    Ok(())
}

fn grade_card_selection(mcp: &mut LiveMcpProcess, _run: &AgentRun) -> (bool, String) {
    let target = match mcp.fetch_card("sel-target") {
        Ok(card) => card,
        Err(err) => return (false, err),
    };
    let claim_agent = target["card"]["claim"]["agent"].as_str();
    if claim_agent != Some(AGENT_NAME) {
        return (
            false,
            format!("sel-target claim.agent was {claim_agent:?}, expected Some({AGENT_NAME:?})"),
        );
    }
    for decoy in ["sel-decoy-blocked", "sel-decoy-repo", "sel-decoy-lowpri"] {
        match mcp.fetch_card(decoy) {
            Ok(card) => {
                if !card["card"]["claim"].is_null() {
                    return (
                        false,
                        format!("{decoy} was also claimed: {}", card["card"]["claim"]),
                    );
                }
            }
            Err(err) => return (false, format!("grading fetch of {decoy} failed: {err}")),
        }
    }
    (true, "claimed sel-target and only sel-target".to_string())
}

fn seed_truncation_recovery(mcp: &mut LiveMcpProcess) -> Result<(), String> {
    let outcome = mcp.call_tool(
        "create_card",
        json!({
            "id": "trunc-target",
            "title": "Investigate staging rollback window",
            "status": "ready",
            "priority": "P1",
            "repo": "eval-target",
            "acceptance": ["rollback window is documented"],
        }),
    );
    if !outcome.ok {
        return Err(format!(
            "seed create_card trunc-target failed: {}",
            outcome.content
        ));
    }

    let fact_entry = mcp.call_tool(
        "append_work_log",
        json!({
            "card_id": "trunc-target",
            "agent": "eval-seed",
            "body": format!(
                "Provisioning note: the staging rollback window is {ROLLBACK_FACT} UTC -- do not deploy after this until confirmed."
            ),
        }),
    );
    if !fact_entry.ok {
        return Err(format!(
            "seed append_work_log (fact entry) failed: {}",
            fact_entry.content
        ));
    }

    for i in 0..24 {
        let outcome = mcp.call_tool(
            "append_work_log",
            json!({
                "card_id": "trunc-target",
                "agent": "eval-seed",
                "body": format!("Routine status update #{i}: still investigating, no blockers."),
            }),
        );
        if !outcome.ok {
            return Err(format!(
                "seed append_work_log (filler entry {i}) failed: {}",
                outcome.content
            ));
        }
    }
    Ok(())
}

fn grade_truncation_recovery(_mcp: &mut LiveMcpProcess, run: &AgentRun) -> (bool, String) {
    let has_fact = run.final_text.contains(ROLLBACK_FACT);
    if !has_fact {
        return (
            false,
            format!(
                "final answer did not contain {ROLLBACK_FACT:?}: {:?}",
                run.final_text
            ),
        );
    }
    let note = if run.used_detailed {
        "answered correctly after fetching detail=\"detailed\"".to_string()
    } else {
        "answered correctly (surface returned full history without a detail flag)".to_string()
    };
    (true, note)
}

fn seed_claim_ergonomics(mcp: &mut LiveMcpProcess) -> Result<(), String> {
    let outcome = mcp.call_tool(
        "create_card",
        json!({
            "id": "lifecycle-target",
            "title": "Fix flaky retry timer",
            "status": "ready",
            "priority": "P1",
            "repo": "eval-target",
            "acceptance": ["retry timer no longer flakes"],
        }),
    );
    if !outcome.ok {
        return Err(format!(
            "seed create_card lifecycle-target failed: {}",
            outcome.content
        ));
    }
    Ok(())
}

fn grade_claim_ergonomics(mcp: &mut LiveMcpProcess, _run: &AgentRun) -> (bool, String) {
    let card = match mcp.fetch_card("lifecycle-target") {
        Ok(card) => card,
        Err(err) => return (false, err),
    };
    let status = card["card"]["status"].as_str();
    let status_ok = status == Some("done");
    let criterion_checked = card["card"]["criteria"][0]["checked_by"].is_string();
    let has_proof = card["runs"]
        .as_array()
        .map(|runs| {
            runs.iter()
                .any(|run| run["proof"].as_str() == Some("https://example.test/eval-proof"))
        })
        .unwrap_or(false);
    if status_ok && criterion_checked && has_proof {
        (
            true,
            "lifecycle complete: status done, criterion checked, proof recorded".to_string(),
        )
    } else {
        (
            false,
            format!(
                "status={status:?} criterion_checked={criterion_checked} has_proof={has_proof}"
            ),
        )
    }
}

// ---------------------------------------------------------------------
// Pilot runner and report
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub scenario: &'static str,
    pub surface: Surface,
    pub model_label: &'static str,
    pub trial: usize,
    pub passed: bool,
    pub tool_calls: usize,
    pub invalid_calls: usize,
    pub response_chars: usize,
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct RunError {
    pub scenario: &'static str,
    pub surface: Surface,
    pub model_label: &'static str,
    pub trial: usize,
    pub error: String,
}

pub struct PilotReport {
    pub outcomes: Vec<RunOutcome>,
    pub errors: Vec<RunError>,
    pub old_surface_skipped: bool,
}

impl PilotReport {
    pub fn table(&self) -> String {
        let mut groups: Vec<(&'static str, Surface, &'static str)> = Vec::new();
        for outcome in &self.outcomes {
            let key = (outcome.scenario, outcome.surface, outcome.model_label);
            if !groups.contains(&key) {
                groups.push(key);
            }
        }

        let mut table = String::from(
            "| scenario | surface | model | pass rate | avg tool calls | avg invalid calls | avg response chars |\n\
             | --- | --- | --- | --- | ---: | ---: | ---: |\n",
        );
        for (scenario, surface, model_label) in groups {
            let runs: Vec<&RunOutcome> = self
                .outcomes
                .iter()
                .filter(|o| {
                    o.scenario == scenario && o.surface == surface && o.model_label == model_label
                })
                .collect();
            if runs.is_empty() {
                continue;
            }
            let total = runs.len();
            let passed = runs.iter().filter(|o| o.passed).count();
            let avg_tool_calls =
                runs.iter().map(|o| o.tool_calls).sum::<usize>() as f64 / total as f64;
            let avg_invalid =
                runs.iter().map(|o| o.invalid_calls).sum::<usize>() as f64 / total as f64;
            let avg_chars =
                runs.iter().map(|o| o.response_chars).sum::<usize>() as f64 / total as f64;
            table.push_str(&format!(
                "| {} | {} | {} | {}/{} | {:.1} | {:.1} | {:.0} |\n",
                scenario,
                surface.label(),
                model_label,
                passed,
                total,
                avg_tool_calls,
                avg_invalid,
                avg_chars
            ));
        }

        let failed: Vec<&RunOutcome> = self.outcomes.iter().filter(|o| !o.passed).collect();
        if !failed.is_empty() {
            table.push_str(&format!("\n{} failed run(s):\n", failed.len()));
            for outcome in failed {
                table.push_str(&format!(
                    "- {} / {} / {} / trial {}: {}\n",
                    outcome.scenario,
                    outcome.surface.label(),
                    outcome.model_label,
                    outcome.trial,
                    outcome.note
                ));
            }
        }

        if !self.errors.is_empty() {
            table.push_str(&format!(
                "\n{} run(s) errored before grading (network/API failures, not scored as fail):\n",
                self.errors.len()
            ));
            for error in &self.errors {
                table.push_str(&format!(
                    "- {} / {} / {} / trial {}: {}\n",
                    error.scenario,
                    error.surface.label(),
                    error.model_label,
                    error.trial,
                    error.error
                ));
            }
        }

        if self.old_surface_skipped {
            table.push_str(
                "\nold surface skipped: POWDER_EVAL_OLD_BINARY was not set. See \
                 src/live_eval.rs module docs for how to build the pre-epic binary.\n",
            );
        }

        table
    }
}

pub fn run_pilot(config: &LiveEvalConfig) -> PilotReport {
    let client = OpenRouterClient::new(config.api_key.clone());
    let mut outcomes = Vec::new();
    let mut errors = Vec::new();
    let old_surface_skipped = config.old_binary.is_none();
    let temp_root = temp_root_dir();
    let _ = std::fs::create_dir_all(&temp_root);

    let surfaces: Vec<Surface> = if old_surface_skipped {
        vec![Surface::New]
    } else {
        vec![Surface::Old, Surface::New]
    };

    for scenario in SCENARIOS {
        for surface in &surfaces {
            let command = match surface {
                Surface::New => McpCommand::from_env_or_default(),
                Surface::Old => McpCommand::binary(
                    config
                        .old_binary
                        .clone()
                        .expect("old surface only selected when old_binary is Some"),
                ),
            };
            for model in &config.models {
                for trial in 1..=config.trials {
                    let db_path = temp_root.join(format!(
                        "{}-{}-{}-trial{trial}.db",
                        scenario.name,
                        surface.label(),
                        model.label
                    ));
                    let _ = std::fs::remove_file(&db_path);
                    match run_one(
                        &client, model, *surface, &command, &db_path, scenario, trial,
                    ) {
                        Ok(outcome) => outcomes.push(outcome),
                        Err(error) => errors.push(RunError {
                            scenario: scenario.name,
                            surface: *surface,
                            model_label: model.label,
                            trial,
                            error,
                        }),
                    }
                }
            }
        }
    }

    let _ = std::fs::remove_dir_all(&temp_root);
    PilotReport {
        outcomes,
        errors,
        old_surface_skipped,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_one(
    client: &OpenRouterClient,
    model: &ModelCandidate,
    surface: Surface,
    command: &McpCommand,
    db_path: &Path,
    scenario: &Scenario,
    trial: usize,
) -> Result<RunOutcome, String> {
    register_fixture_repositories(command, db_path)?;

    let mut mcp = LiveMcpProcess::spawn(command, db_path)?;
    (scenario.seed)(&mut mcp)?;

    let tools = mcp.list_tools()?;
    let tools_json = tools_to_openai_json(&tools);
    let instructions = mcp.server_instructions()?;
    let prompt = system_prompt(instructions.as_deref());

    let run = run_agent(
        client,
        &model.slug,
        &mut mcp,
        &tools_json,
        &prompt,
        scenario.task,
        DEFAULT_MAX_TOOL_CALLS,
    )?;

    let (passed, note) = (scenario.grade)(&mut mcp, &run);
    mcp.shutdown();

    Ok(RunOutcome {
        scenario: scenario.name,
        surface,
        model_label: model.label,
        trial,
        passed,
        tool_calls: run.tool_calls,
        invalid_calls: run.invalid_calls,
        response_chars: run.response_chars,
        note,
    })
}

fn register_fixture_repositories(command: &McpCommand, db_path: &Path) -> Result<(), String> {
    let mut admin = LiveMcpProcess::spawn_admin(command, db_path)?;
    for repo in ["eval-target", "eval-other"] {
        let outcome = admin.call_tool(
            "upsert_repository",
            json!({"name": repo, "visibility": "visible", "tier": "active"}),
        );
        if !outcome.ok {
            return Err(format!(
                "seed upsert_repository {repo} failed: {}",
                outcome.content
            ));
        }
    }
    admin.shutdown();
    Ok(())
}

fn temp_root_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("powder-mcp-live-ab-{}-{nonce}", std::process::id()))
}
