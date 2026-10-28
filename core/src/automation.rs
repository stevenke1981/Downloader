//! Explicit one-shot actions. Manager persists consumption before invoking them.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Action {
    #[default]
    StartAll,
    PauseAll,
    Run {
        program: String,
        args: Vec<String>,
    },
    Shutdown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ScheduledAction {
    pub id: u64,
    /// UTC epoch seconds.
    pub at: i64,
    pub action: Action,
    pub consumed: bool,
}

impl ScheduledAction {
    pub fn validate(&self) -> Result<(), String> {
        if self.id == 0 || self.at < 0 {
            return Err("scheduled actions need a positive ID and a valid epoch time".into());
        }
        if let Action::Run { program, args } = &self.action
            && (program.trim().is_empty()
                || program.contains('\0')
                || args.iter().any(|a| a.contains('\0')))
        {
            return Err("scheduled program and arguments must be valid process arguments".into());
        }
        Ok(())
    }
}

/// Executes an explicitly configured action using argv, never a command shell.
pub(crate) fn run(action: Action) -> Result<(), String> {
    let mut command = match action {
        Action::Run { program, args } => {
            let mut command = std::process::Command::new(program);
            command.args(args);
            command
        }
        Action::Shutdown => {
            #[cfg(windows)]
            let mut command = {
                let mut command = std::process::Command::new("shutdown.exe");
                command.args(["/s", "/t", "0"]);
                command
            };
            #[cfg(not(windows))]
            let mut command = {
                let mut command = std::process::Command::new("shutdown");
                command.args(["-h", "now"]);
                command
            };
            command.stdin(std::process::Stdio::null());
            command
        }
        Action::StartAll | Action::PauseAll => return Ok(()),
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let status = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("could not start action: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("action exited with {status}"))
    }
}
