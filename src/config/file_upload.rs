use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_files: usize,
    pub allow_extensions: Vec<String>,
    pub deny_extensions: Vec<String>,
    pub allow_hidden: bool,
    pub allow_symlinks: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 100 * 1024 * 1024,
            max_total_bytes: 250 * 1024 * 1024,
            max_files: 10,
            allow_extensions: vec![],
            deny_extensions: vec![],
            allow_hidden: false,
            allow_symlinks: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ssh {
    pub executable: String,
    pub config_file: Option<PathBuf>,
    pub identity_file: Option<PathBuf>,
    pub port: Option<u16>,
    pub jump_host: Option<String>,
    pub batch_mode: bool,
    pub connect_timeout_seconds: u64,
    pub operation_timeout_seconds: u64,
    pub keepalive_seconds: u64,
    pub chunk_bytes: usize,
    pub options: BTreeMap<String, String>,
}

impl Default for Ssh {
    fn default() -> Self {
        Self {
            executable: "ssh".into(),
            config_file: None,
            identity_file: None,
            port: None,
            jump_host: None,
            batch_mode: true,
            connect_timeout_seconds: 15,
            operation_timeout_seconds: 600,
            keepalive_seconds: 15,
            chunk_bytes: 1024 * 1024,
            options: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_destination")]
    pub destination: String,
    #[serde(default = "default_socket")]
    pub socket: String,
    #[serde(default = "default_python")]
    pub python: String,
    #[serde(default = "default_retention")]
    pub retention_seconds: u64,
    #[serde(default)]
    pub ssh: Option<Ssh>,
    #[serde(default)]
    pub limits: Option<Limits>,
}

fn default_destination() -> String {
    "~/.cache/herdr/uploads".into()
}
fn default_socket() -> String {
    "~/.config/herdr/herdr.sock".into()
}
fn default_python() -> String {
    "python3".into()
}
fn default_retention() -> u64 {
    30 * 24 * 60 * 60
}

impl Limits {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.max_file_bytes > 0 && self.max_total_bytes > 0 && self.max_files > 0,
            "file limits must be positive"
        );
        for ext in self.allow_extensions.iter().chain(&self.deny_extensions) {
            ensure!(
                !ext.is_empty()
                    && !ext.contains(['/', '\\', '.'])
                    && !ext.chars().any(char::is_control),
                "extensions must be bare suffixes such as pdf or png"
            );
        }
        Ok(())
    }
}

impl Ssh {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.executable.is_empty(), "empty SSH executable");
        ensure!(
            (4096..=8 * 1024 * 1024).contains(&self.chunk_bytes),
            "chunk_bytes must be between 4096 and 8388608"
        );
        ensure!(
            self.connect_timeout_seconds > 0 && self.operation_timeout_seconds > 0,
            "SSH timeouts must be positive"
        );
        for key in self.options.keys() {
            if !key.chars().all(|c| c.is_ascii_alphanumeric()) || key.is_empty() {
                bail!("invalid SSH option name");
            }
        }
        Ok(())
    }
}

/// Local client policy for dropped files. SSH host/session come from the selected machine.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileUploadConfig {
    pub enabled: bool,
    pub limits: Limits,
    pub ssh: Ssh,
    pub destination: String,
    pub python: String,
    pub retention_seconds: u64,
    /// Overrides keyed by the saved machine's SSH target, not its display label.
    pub targets: BTreeMap<String, TargetOverride>,
}

impl Default for FileUploadConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            limits: Limits::default(),
            ssh: Ssh::default(),
            destination: default_destination(),
            python: default_python(),
            retention_seconds: default_retention(),
            targets: BTreeMap::new(),
        }
    }
}

impl FileUploadConfig {
    pub fn resolve(&self, host: &str, session: &str) -> Result<(Target, Ssh, Limits)> {
        let mut target = Target {
            host: host.to_owned(),
            destination: self.destination.clone(),
            socket: if session == "default" {
                default_socket()
            } else {
                format!("~/.config/herdr/sessions/{session}/herdr.sock")
            },
            python: self.python.clone(),
            retention_seconds: self.retention_seconds,
            ssh: None,
            limits: None,
        };
        if let Some(overrides) = self.targets.get(host) {
            if let Some(value) = &overrides.destination {
                target.destination.clone_from(value);
            }
            if let Some(value) = &overrides.socket {
                target.socket.clone_from(value);
            }
            if let Some(value) = &overrides.python {
                target.python.clone_from(value);
            }
            if let Some(value) = overrides.retention_seconds {
                target.retention_seconds = value;
            }
            target.ssh.clone_from(&overrides.ssh);
            target.limits.clone_from(&overrides.limits);
        }
        ensure!(
            !host.is_empty()
                && !host.starts_with('-')
                && !host.chars().any(|c| c.is_whitespace() || c.is_control()),
            "invalid SSH target"
        );
        for path in [&target.destination, &target.socket] {
            ensure!(
                (path.starts_with('/') || path.starts_with("~/"))
                    && !path.chars().any(char::is_control),
                "upload paths must be absolute or start with ~/"
            );
        }
        ensure!(
            !target.python.is_empty() && target.retention_seconds > 0,
            "invalid upload receiver configuration"
        );
        let ssh = target.ssh.clone().unwrap_or_else(|| self.ssh.clone());
        let limits = target.limits.clone().unwrap_or_else(|| self.limits.clone());
        ssh.validate()?;
        limits.validate()?;
        Ok((target, ssh, limits))
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetOverride {
    pub destination: Option<String>,
    pub socket: Option<String>,
    pub python: Option<String>,
    pub retention_seconds: Option<u64>,
    pub ssh: Option<Ssh>,
    pub limits: Option<Limits>,
}
