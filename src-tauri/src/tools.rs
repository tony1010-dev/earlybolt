//! The AI tools Earlybolt pings: how to call them, their official installers, and how to sign in.

pub const PROMPT: &str = "Reply with exactly: ok";

pub struct Tool {
    pub id: &'static str,
    pub name: &'static str,
    /// Call with the lightest model (all commands run in order)
    pub light: Vec<Vec<String>>,
    /// Retried with the default model when `light` fails (e.g. a model was renamed)
    pub plain: Vec<Vec<String>>,
    /// Official install scripts (macOS/Linux shell, Windows PowerShell)
    pub install_sh: &'static str,
    pub install_ps1: &'static str,
    /// Starts the browser sign-in; runs in a visible terminal window
    pub login: &'static str,
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.replace("$P", PROMPT)).collect()
}

pub fn all() -> Vec<Tool> {
    vec![
        Tool {
            id: "claude",
            name: "Claude",
            light: vec![argv(&["claude", "-p", "$P", "--model", "haiku"])],
            plain: vec![argv(&["claude", "-p", "$P"])],
            install_sh: "curl -fsSL https://claude.ai/install.sh | bash",
            install_ps1: "irm https://claude.ai/install.ps1 | iex",
            login: "claude auth login --claudeai",
        },
        Tool {
            id: "codex",
            name: "Codex",
            light: vec![argv(&["codex", "exec", "--skip-git-repo-check", "-c", "model_reasoning_effort=\"low\"", "$P"])],
            plain: vec![argv(&["codex", "exec", "--skip-git-repo-check", "$P"])],
            install_sh: "curl -fsSL https://chatgpt.com/codex/install.sh | sh",
            install_ps1: "irm https://chatgpt.com/codex/install.ps1 | iex",
            login: "codex login",
        },
        Tool {
            id: "grok",
            name: "Grok",
            light: vec![argv(&["grok", "-p", "$P", "-m", "grok-4.7-build-fast"])],
            plain: vec![argv(&["grok", "-p", "$P"])],
            install_sh: "curl -fsSL https://x.ai/cli/install.sh | bash",
            install_ps1: "irm https://x.ai/cli/install.ps1 | iex",
            login: "grok login --oauth",
        },
        // Antigravity may meter model families separately, so ping both a Gemini and a Claude model
        Tool {
            id: "agy",
            name: "Antigravity",
            light: vec![
                argv(&["agy", "-p", "$P", "--model", "gemini-3.8-flash-low"]),
                argv(&["agy", "-p", "$P", "--model", "claude-sonnet-5-5-low"]),
            ],
            plain: vec![argv(&["agy", "-p", "$P"])],
            install_sh: "curl -fsSL https://antigravity.google/cli/install.sh | bash",
            install_ps1: "irm https://antigravity.google/cli/install.ps1 | iex",
            // No login subcommand: the browser sign-in opens on the first model call
            login: "agy -p ok",
        },
    ]
}
