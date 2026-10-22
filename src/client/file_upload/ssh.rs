use std::{process::Stdio, time::Duration};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

use super::{
    config::{Limits, Ssh, Target},
    files::PreparedFile,
    progress::Progress,
};

const RECEIVER: &str = include_str!("receiver.py");
const MAX_REPLY: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Pane {
    pub pane_id: String,
    pub terminal_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
    #[serde(default)]
    pub agent_session: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Receipt {
    pub name: String,
    pub reference: String,
    pub size: u64,
    pub sha256: String,
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Only the fixed receiver and configured Python executable cross the SSH shell.
/// File paths, pane IDs and other request parameters always travel on stdin.
pub fn command_args(config: &Ssh, target: &Target) -> Vec<String> {
    let mut args = vec!["-T".into()];
    if let Some(file) = &config.config_file {
        args.extend(["-F".into(), file.to_string_lossy().into()]);
    }
    if let Some(file) = &config.identity_file {
        args.extend(["-i".into(), file.to_string_lossy().into()]);
    }
    if let Some(port) = config.port {
        args.extend(["-p".into(), port.to_string()]);
    }
    if let Some(jump) = &config.jump_host {
        args.extend(["-J".into(), jump.clone()]);
    }
    // Explicit options precede defaults because OpenSSH uses the first value.
    for (key, value) in &config.options {
        args.extend(["-o".into(), format!("{key}={value}")]);
    }
    for option in [
        format!("BatchMode={}", if config.batch_mode { "yes" } else { "no" }),
        format!("ConnectTimeout={}", config.connect_timeout_seconds),
        format!("ServerAliveInterval={}", config.keepalive_seconds),
        "ServerAliveCountMax=3".into(),
    ] {
        args.extend(["-o".into(), option]);
    }
    args.extend([
        "--".into(),
        target.host.clone(),
        format!(
            "{} -c {}",
            shell_quote(&target.python),
            shell_quote(RECEIVER)
        ),
    ]);
    args
}

struct Connection {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Connection {
    async fn open(config: &Ssh, target: &Target, request: &Value) -> Result<Self> {
        let mut child = Command::new(&config.executable)
            .args(command_args(config, target))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("start SSH")?;
        let input = child.stdin.take().context("SSH stdin unavailable")?;
        let output = child.stdout.take().context("SSH stdout unavailable")?;
        let mut connection = Self {
            child,
            input: Some(input),
            output: BufReader::new(output),
        };
        let mut header = serde_json::to_vec(request)?;
        header.push(b'\n');
        connection.write(&header).await?;
        Ok(connection)
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.input
            .as_mut()
            .context("SSH input is closed")?
            .write_all(bytes)
            .await
            .context("write upload stream")
    }

    async fn reply(&mut self) -> Result<Value> {
        let mut bytes = Vec::new();
        while bytes.len() < MAX_REPLY {
            let byte = self.output.read_u8().await.context("SSH receiver closed before replying; check SSH authentication and Python availability")?;
            if byte == b'\n' {
                let value: Value =
                    serde_json::from_slice(&bytes).context("invalid SSH receiver reply")?;
                if let Some(error) = value.get("error") {
                    bail!("remote: {error}");
                }
                return Ok(value);
            }
            bytes.push(byte);
        }
        bail!("SSH receiver reply exceeds 1 MiB")
    }

    async fn finish(mut self) -> Result<()> {
        self.input.take();
        let status = self.child.wait().await?;
        ensure!(status.success(), "SSH receiver failed ({status})");
        Ok(())
    }
}

fn request_header(target: &Target, op: &str) -> Value {
    json!({"version": 1, "op": op, "socket": target.socket, "destination": target.destination})
}

pub async fn request(config: &Ssh, target: &Target, value: Value) -> Result<Value> {
    tokio::time::timeout(
        Duration::from_secs(config.operation_timeout_seconds),
        async {
            let mut connection = Connection::open(config, target, &value).await?;
            let response = connection.reply().await?;
            connection.finish().await?;
            Ok(response)
        },
    )
    .await
    .context("SSH operation timed out")?
}

pub async fn pin(config: &Ssh, target: &Target, pane_id: &str) -> Result<Pane> {
    let mut header = request_header(target, "pane");
    header["pane"] = json!(pane_id);
    let pane: Pane = serde_json::from_value(request(config, target, header).await?)?;
    ensure!(
        pane.agent_status.as_deref() != Some("blocked"),
        "target is waiting for approval; choose a prompt-ready pane"
    );
    Ok(pane)
}

pub async fn upload(
    config: &Ssh,
    target: &Target,
    limits: &Limits,
    file: &PreparedFile,
    progress: &Progress,
) -> Result<Receipt> {
    progress.update(0, "connecting");
    let result = tokio::time::timeout(
        Duration::from_secs(config.operation_timeout_seconds),
        async {
            let mut header = request_header(target, "upload");
            header["name"] = json!(file.name);
            header["size"] = json!(file.size);
            header["sha256"] = json!(file.sha256);
            header["max_file_bytes"] = json!(limits.max_file_bytes);
            header["chunk_bytes"] = json!(config.chunk_bytes);
            header["retention_seconds"] = json!(target.retention_seconds);
            let mut connection = Connection::open(config, target, &header).await?;
            ensure!(
                connection.reply().await?.get("ready") == Some(&json!(true)),
                "receiver was not ready"
            );
            let mut input = tokio::fs::File::open(file.snapshot.path()).await?;
            let mut buffer = vec![0; config.chunk_bytes];
            let mut sent = 0;
            while sent < file.size {
                let wanted = (file.size - sent).min(config.chunk_bytes as u64) as usize;
                input.read_exact(&mut buffer[..wanted]).await?;
                connection.write(&buffer[..wanted]).await?;
                sent += wanted as u64;
                let ack = connection.reply().await?;
                ensure!(
                    ack.get("received").and_then(Value::as_u64) == Some(sent),
                    "invalid receiver acknowledgement"
                );
                progress.update(
                    sent,
                    if sent == file.size {
                        "verifying"
                    } else {
                        "uploading"
                    },
                );
            }
            let response = connection.reply().await?;
            ensure!(
                response.get("sha256").and_then(Value::as_str) == Some(&file.sha256),
                "receiver checksum mismatch"
            );
            ensure!(
                response.get("size").and_then(Value::as_u64) == Some(file.size),
                "receiver size mismatch"
            );
            let reference = response
                .get("path")
                .and_then(Value::as_str)
                .context("receiver returned no path")?
                .to_owned();
            ensure!(
                reference.starts_with('/') && !reference.chars().any(char::is_control),
                "invalid remote path"
            );
            connection.finish().await?;
            Ok(Receipt {
                name: file.name.clone(),
                reference,
                size: file.size,
                sha256: file.sha256.clone(),
            })
        },
    )
    .await
    .context("upload timed out")?;
    progress.finish(if result.is_ok() {
        "uploaded and checksum verified"
    } else {
        "upload failed"
    });
    result
}

pub async fn insert(
    config: &Ssh,
    target: &Target,
    pane: &Pane,
    receipts: &[Receipt],
) -> Result<()> {
    let mut header = request_header(target, "paste");
    header["pane"] = json!(pane.pane_id);
    header["terminal_id"] = json!(pane.terminal_id);
    header["agent_session"] = json!(pane.agent_session);
    header["text"] = json!(format!(
        "{} ",
        receipts
            .iter()
            .map(|r| reference_text(&r.reference))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    request(config, target, header).await?;
    Ok(())
}

fn reference_text(reference: &str) -> String {
    // Keep simple paths unquoted so agents can recognize image paths directly.
    if reference
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-:@".contains(c))
    {
        reference.into()
    } else {
        shell_quote(reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_boundary_and_reference_escaping() {
        assert_eq!(
            shell_quote("a'b $(touch /tmp/no)"),
            "'a'\\''b $(touch /tmp/no)'"
        );
        assert_eq!(reference_text("/tmp/a.png"), "/tmp/a.png");
        assert_eq!(reference_text("/tmp/a b.pdf"), "'/tmp/a b.pdf'");
        let target: Target = toml::from_str("host = 'dev'").unwrap();
        let args = command_args(&Ssh::default(), &target);
        assert_eq!(args[args.len() - 2], "dev");
        assert!(args.last().unwrap().starts_with("'python3' -c '"));
        assert!(!args.last().unwrap().contains(&target.destination));
    }
}
