mod claude;
mod codex;
mod pi;

pub use claude::ClaudeCode;
pub use codex::CodexCode;
pub use pi::PiCode;

use super::AgentKind;

impl AgentKind {
    pub fn profile(self) -> &'static str {
        match self {
            Self::Claude => "agents/claude",
            Self::Codex => "agents/codex",
            Self::Pi => "agents/pi",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Pi => "Pi",
        }
    }

    pub fn purpose(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code uses your account to run tasks.",
            Self::Codex => "Codex uses your OpenAI account to run tasks.",
            Self::Pi => "Pi uses your provider account to run tasks.",
        }
    }

    pub fn login_files(self) -> &'static [&'static str] {
        match self {
            // Claude Code's own login file; its settings and MCP files stay.
            Self::Claude => &[".credentials.json"],
            Self::Codex => &["auth.json"],
            // Pi keeps one credential per provider here, and nothing else.
            Self::Pi => &["auth.json"],
        }
    }
}
