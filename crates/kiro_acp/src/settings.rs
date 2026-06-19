//! Permissions/configuration model loaded from `acp_settings.json`.
//!
//! Controls how KiroUI responds to `session/request_permission` and provides
//! working-directory and environment context for the agent.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Auto-approval policy for tool permission requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoApprove {
    /// Surface every permission request to the user (default; safest).
    #[default]
    Ask,
    /// Automatically approve non-destructive requests; still prompt for
    /// destructive/elevated operations.
    AllowAll,
}

/// KiroUI settings, typically deserialized from `acp_settings.json`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Permission auto-approval policy.
    pub auto_approve_permissions: AutoApprove,
    /// Optional working directory override for the agent/session.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables to inject into the agent subprocess.
    #[serde(default)]
    pub env: Vec<EnvPair>,
}

/// A single environment variable entry in settings.
#[derive(Debug, Clone, Deserialize)]
pub struct EnvPair {
    /// Variable name.
    pub name: String,
    /// Variable value.
    pub value: String,
}

impl Settings {
    /// Load settings from a JSON file. Returns defaults if the file is absent;
    /// returns an error if present but malformed.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents)
                .map_err(|e| format!("invalid {}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    /// Whether a permission request with the given title should be
    /// auto-approved without prompting the user.
    ///
    /// Only applies in [`AutoApprove::AllowAll`] mode, and never for operations
    /// that look destructive or privilege-elevating.
    pub fn should_auto_approve(&self, title: &str) -> bool {
        if self.auto_approve_permissions != AutoApprove::AllowAll {
            return false;
        }
        !is_destructive(title)
    }
}

/// Heuristic: does this tool-call title look destructive or elevated?
pub fn is_destructive(title: &str) -> bool {
    let t = title.to_lowercase();
    const MARKERS: &[&str] = &[
        "rm ",
        "rm -",
        "remove",
        "delete",
        "drop ",
        "sudo",
        "mkfs",
        "format",
        "shutdown",
        "reboot",
        "kill",
        "truncate",
        "overwrite",
        "force push",
        "git push --force",
        "reset --hard",
        "dd ",
        ">/",
        "chmod 777",
    ];
    MARKERS.iter().any(|m| t.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_ask() {
        let s = Settings::default();
        assert_eq!(s.auto_approve_permissions, AutoApprove::Ask);
        assert!(!s.should_auto_approve("Read file"));
    }

    #[test]
    fn allow_all_auto_approves_safe_ops() {
        let s = Settings {
            auto_approve_permissions: AutoApprove::AllowAll,
            ..Default::default()
        };
        assert!(s.should_auto_approve("Read file foo.txt"));
        assert!(s.should_auto_approve("List directory"));
    }

    #[test]
    fn allow_all_still_prompts_for_destructive() {
        let s = Settings {
            auto_approve_permissions: AutoApprove::AllowAll,
            ..Default::default()
        };
        assert!(!s.should_auto_approve("rm -rf /tmp/x"));
        assert!(!s.should_auto_approve("sudo apt install"));
        assert!(!s.should_auto_approve("delete the database"));
    }

    #[test]
    fn parses_json() {
        let json = r#"{"autoApprovePermissions":"allow_all","env":[{"name":"FOO","value":"bar"}]}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.auto_approve_permissions, AutoApprove::AllowAll);
        assert_eq!(s.env.len(), 1);
        assert_eq!(s.env[0].name, "FOO");
    }

    #[test]
    fn missing_file_is_default() {
        let s = Settings::load("/no/such/acp_settings.json").unwrap();
        assert_eq!(s.auto_approve_permissions, AutoApprove::Ask);
    }
}
