use crate::agent::{
    AUTOPILOT_SYSTEM_PROMPT, Activity, AgentEngine, AgentKind, ConversationState,
    TaskProgressDecoder, TaskStreamDecoder, TaskUsage, activity, system_prompt,
};
use crate::isolation::{
    DefaultFile, GuestPath, PersistentDirectory, ProfileRecipe, SignInCommand, shell_quote,
};
use crate::task::TaskAgentTurnContext;
use serde_json::Value;
use std::mem;

const INSTALL_COMMAND: &str = "set -eu\n\
    case $(uname -m) in\n\
      aarch64) target=aarch64-unknown-linux-musl ;;\n\
      x86_64) target=x86_64-unknown-linux-musl ;;\n\
      *) echo 'Codex requires an aarch64 or x86_64 guest' >&2; exit 1 ;;\n\
    esac\n\
    curl -fsSL \"https://github.com/openai/codex/releases/download/rust-v0.167.0/codex-package-${target}.tar.gz\" \
      -o /tmp/altitude-codex.tar.gz\n\
    mkdir -p /opt/codex\n\
    tar -xzf /tmp/altitude-codex.tar.gz -C /opt/codex\n\
    ln -sf /opt/codex/bin/codex /usr/local/bin/codex\n\
    rm /tmp/altitude-codex.tar.gz\n\
    codex --version";

const DEFAULT_CONFIG: &str = r#"cli_auth_credentials_store = "file"
check_for_update_on_startup = false
approval_policy = "never"
sandbox_mode = "danger-full-access"

[features]
apps = false

[mcp_servers.altitude]
command = "python3"
args = ["/altitude/codex/altitude-bridge.py"]
env_vars = ["ALTITUDE_MCP_PORT"]
required = true
"#;

// These locations belong to the capsule, even when the operator changes their
// profile's model or reasoning settings.
const STATE_OPTIONS: &str = "-c 'sqlite_home=\"/altitude/codex/runtime\"' \
    -c 'log_dir=\"/altitude/codex/runtime/logs\"'";

const OPERATOR_INSTRUCTIONS: &str = "The operator has taken this altitude task over and is at the terminal. Answer them directly here. They control when work starts and stops; do not continue autonomously or require a tasks_signal call to end a turn. Preserve this task's conversation and workspace. altitude still owns the task record, and the operator alone accepts or closes the task.";

#[derive(Clone)]
pub struct CodexCode;

#[derive(Default)]
struct JsonLines {
    pending: Vec<u8>,
}

impl JsonLines {
    fn process(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            lines.push(self.pending.drain(..=newline).collect());
        }
        lines
    }

    fn take_pending(&mut self) -> Option<Value> {
        let message = serde_json::from_slice(&self.pending).ok()?;
        self.pending.clear();
        Some(message)
    }
}

#[derive(Default)]
struct CodexStream {
    lines: JsonLines,
    conversation_id: Option<String>,
    completed: bool,
    error: Option<String>,
    stderr: Vec<u8>,
}

impl CodexStream {
    fn message(&mut self, message: Value) {
        match message.get("type").and_then(Value::as_str) {
            Some("thread.started") => {
                self.conversation_id = message
                    .get("thread_id")
                    .and_then(Value::as_str)
                    .filter(|id| uuid::Uuid::parse_str(id).is_ok())
                    .map(str::to_owned);
            }
            Some("turn.completed") => {
                self.completed = true;
                self.error = None;
            }
            Some("turn.failed") => {
                self.error = Some(
                    message
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("Codex failed the turn")
                        .to_owned(),
                );
            }
            Some("error") => {
                self.error = message
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            _ => {}
        }
    }

    fn finish_pending(&mut self) {
        if let Some(message) = self.lines.take_pending() {
            self.message(message);
        }
    }
}

impl TaskStreamDecoder for CodexStream {
    fn process(&mut self, bytes: &[u8]) {
        for line in self.lines.process(bytes) {
            match serde_json::from_slice(&line) {
                Ok(message) => self.message(message),
                // A guest PTY can merge stderr into stdout before startup has
                // produced any JSON, including configuration and login errors.
                Err(_) => self.process_stderr(&line),
            }
        }
    }

    fn process_stderr(&mut self, bytes: &[u8]) {
        self.stderr.extend_from_slice(bytes);
        if self.stderr.len() > 8192 {
            self.stderr.drain(..self.stderr.len() - 8192);
        }
    }

    fn take_conversation_id(&mut self) -> Option<String> {
        self.finish_pending();
        self.conversation_id.take()
    }

    fn failure_message(&mut self) -> Option<&str> {
        self.finish_pending();
        if !self.completed && self.error.is_none() {
            let pending = mem::take(&mut self.lines.pending);
            self.process_stderr(&pending);
            let said = String::from_utf8_lossy(&self.stderr);
            self.error = Some(if said.trim().is_empty() {
                "Codex ended without completing its turn".to_owned()
            } else {
                format!("Codex ended without completing its turn: {}", said.trim())
            });
        }
        self.error.as_deref()
    }
}

#[derive(Default)]
struct CodexProgress {
    lines: JsonLines,
    activity: Option<Activity>,
}

impl CodexProgress {
    fn message(&mut self, message: Value) {
        if !matches!(
            message.get("type").and_then(Value::as_str),
            Some("item.updated" | "item.completed")
        ) {
            return;
        }
        let Some(item) = message.get("item") else {
            return;
        };
        let Some(text) = item.get("text").and_then(Value::as_str) else {
            return;
        };
        let reading = match item.get("type").and_then(Value::as_str) {
            Some("agent_message") => activity::narration(text),
            Some("reasoning") => activity::thinking(text),
            _ => None,
        };
        if reading.is_some() {
            self.activity = reading;
        }
    }
}

impl TaskProgressDecoder for CodexProgress {
    fn process(&mut self, bytes: &[u8]) {
        for line in self.lines.process(bytes) {
            if let Ok(message) = serde_json::from_slice(&line) {
                self.message(message);
            }
        }
    }

    fn take_activity(&mut self) -> Option<Activity> {
        if let Some(message) = self.lines.take_pending() {
            self.message(message);
        }
        self.activity.take()
    }
}

impl AgentEngine for CodexCode {
    fn profile_recipe(&self) -> ProfileRecipe {
        ProfileRecipe {
            profile: AgentKind::Codex.profile().to_owned(),
            display: "Codex",
            sign_in: "Open the link from Codex and enter its device code to sign in.",
            sign_in_command: SignInCommand::Direct(
                "exec codex -c 'cli_auth_credentials_store=\"file\"' login --device-auth",
            ),
            status_command: "codex -c 'cli_auth_credentials_store=\"file\"' login status 2>&1",
            label: "installing Codex",
            setup_command: INSTALL_COMMAND,
            directories: vec![
                PersistentDirectory::root(
                    GuestPath::Absolute("/altitude/codex"),
                    include_str!("assets/codex.md"),
                    vec![DefaultFile {
                        path: "config.toml",
                        contents: DEFAULT_CONFIG,
                    }],
                )
                .with_managed_file("altitude-bridge.py", include_str!("assets/bridge.py"))
                .with_task_local_subdirectories(&[
                    "sessions",
                    "archived_sessions",
                    "runtime",
                    "shell_snapshots",
                    "thread-writer-locks",
                ]),
            ],
            environment: vec![("CODEX_HOME", "/altitude/codex")],
        }
    }

    fn new_conversation_id(&self) -> Option<String> {
        None
    }

    fn conversation_state(&self, _conversation_id: &str) -> ConversationState {
        // Paginated Codex sessions keep their messages in SQLite. Their rollout
        // alone cannot restore the conversation.
        ConversationState::Directories {
            required_file: "/altitude/codex/runtime/thread_history_1.sqlite",
            paths: &[
                "/altitude/codex/sessions",
                "/altitude/codex/archived_sessions",
                "/altitude/codex/runtime",
            ],
        }
    }

    fn autopilot_command(
        &self,
        conversation_id: Option<&str>,
        prompt: &str,
        context: &TaskAgentTurnContext,
        _resume: bool,
    ) -> String {
        let session = conversation_id
            .map(|id| format!("resume {}", shell_quote(id)))
            .unwrap_or_default();
        let instructions =
            toml::Value::String(system_prompt(AUTOPILOT_SYSTEM_PROMPT, context)).to_string();
        format!(
            "exec codex exec --json --skip-git-repo-check \
             -c 'approval_policy=\"never\"' -c 'sandbox_mode=\"danger-full-access\"' \
             {STATE_OPTIONS} -c {} {session} {} </dev/null",
            shell_quote(&format!("developer_instructions={instructions}")),
            shell_quote(&format!(
                "{prompt}\n\naltitude task context at the start of this turn: {context}"
            )),
        )
    }

    fn operator_command(
        &self,
        conversation_id: &str,
        opening: &str,
        context: &TaskAgentTurnContext,
    ) -> String {
        let instructions =
            toml::Value::String(system_prompt(OPERATOR_INSTRUCTIONS, context)).to_string();
        format!(
            "exec codex {STATE_OPTIONS} -c {} resume {} {}",
            shell_quote(&format!("developer_instructions={instructions}")),
            shell_quote(conversation_id),
            shell_quote(opening)
        )
    }

    fn operator_requires_conversation(&self) -> bool {
        true
    }

    fn stream_decoder(&self, _recorded: &TaskUsage) -> Box<dyn TaskStreamDecoder> {
        Box::<CodexStream>::default()
    }

    fn progress_decoder(&self) -> Option<Box<dyn TaskProgressDecoder>> {
        Some(Box::<CodexProgress>::default())
    }
}
