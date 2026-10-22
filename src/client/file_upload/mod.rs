//! Native local-file drops. Storage uses SSH; progress belongs only to this client.
//! No private endpoint codec or remote Herdr installation change is needed.
mod files;
mod progress;
mod ssh;
use crate::config::file_upload as config;

use super::{
    endpoint::{ClientEndpointId, EndpointCatalog, EndpointRegistry},
    shell::ClientShellState,
};
use anyhow::{Context, Result};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::{sync::watch, task::JoinHandle};

struct Uploaded {
    target: config::Target,
    ssh: config::Ssh,
    pane: ssh::Pane,
    receipts: Vec<ssh::Receipt>,
}

enum Finished {
    Uploaded(Box<Uploaded>),
    Inserted,
}

struct Job {
    endpoint: ClientEndpointId,
    generation: u64,
    label: String,
    task: JoinHandle<Result<Finished>>,
    updates: watch::Receiver<String>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) struct Uploads {
    config: config::FileUploadConfig,
    job: Option<Job>,
    notice: Option<(String, Option<Instant>)>,
}

impl Uploads {
    pub(super) fn new(config: config::FileUploadConfig) -> Self {
        Self {
            config,
            job: None,
            notice: None,
        }
    }

    /// Terminal emulators deliver drops as pasted paths. Require an entire payload
    /// of existing absolute local paths, so ordinary prose/remote paths pass through.
    pub(super) fn try_drop(
        &mut self,
        text: &str,
        catalog: &EndpointCatalog,
        registry: &EndpointRegistry,
        shell: &ClientShellState,
    ) -> bool {
        if !self.config.enabled
            || registry.active_id().is_local()
            || !registry.active_surface_available()
        {
            return false;
        }
        let Some(pane_id) = shell.file_upload_pane() else {
            return false;
        };
        let Some(paths) = dropped_paths(text) else {
            return false;
        };
        if self.job.is_some() {
            self.notice = Some(("Upload already in progress; wait or press Esc to cancel before dropping more files".into(), None));
            return true;
        }
        let profile = catalog
            .ssh
            .iter()
            .find(|p| ClientEndpointId::Ssh(p.id.clone()) == *registry.active_id());
        let Some(profile) = profile else {
            self.fail("No saved SSH target for this drop".into());
            return true;
        };
        let resolved = self.config.resolve(&profile.target, &profile.session);
        let (target, ssh, limits) = match resolved {
            Ok(value) => value,
            Err(error) => {
                self.fail(format!("Invalid upload settings: {error:#}"));
                return true;
            }
        };
        let Some(connection) = registry.connection(registry.active_id()) else {
            return true;
        };
        let label = format!("{} → {pane_id}", profile.label);
        let (updates, receiver) = watch::channel("Preparing files…".to_owned());
        let task = tokio::spawn(async move {
            // Pin the terminal before potentially slow local file snapshots.
            let pane = ssh::pin(&ssh, &target, &pane_id).await?;
            let files =
                tokio::task::spawn_blocking(move || files::prepare(&paths, &limits)).await??;
            // Expiry is opportunistic: an upload reaps only our own marked directories.
            let cleanup = serde_json::json!({"version": 1, "op": "cleanup", "destination": &target.destination, "dry_run": false});
            if let Err(error) = ssh::request(&ssh, &target, cleanup).await {
                tracing::warn!(%error, "file upload cleanup failed");
            }
            let mut receipts = Vec::new();
            for file in files {
                let progress = progress::Progress::new(&file.name, file.size, updates.clone());
                // Limits have already been enforced for the entire batch before SSH.
                let limit = config::Limits {
                    max_file_bytes: file.size.max(1),
                    ..Default::default()
                };
                match ssh::upload(&ssh, &target, &limit, &file, &progress).await {
                    Ok(receipt) => receipts.push(receipt),
                    Err(error) => {
                        return Err(error.context(format!(
                            "Upload failed. Completed paths: {}",
                            references(&receipts)
                        )))
                    }
                }
            }
            Ok(Finished::Uploaded(Box::new(Uploaded {
                target,
                ssh,
                pane,
                receipts,
            })))
        });
        self.notice = Some((format!("{label} · Preparing files…"), None));
        self.job = Some(Job {
            endpoint: registry.active_id().clone(),
            generation: connection.generation,
            label,
            task,
            updates: receiver,
        });
        true
    }

    pub(super) fn cancel(&mut self) -> bool {
        if self.job.take().is_some() {
            self.fail("Upload cancelled. Completed files may remain; inspect the prompt if insertion had started.".into());
            true
        } else {
            self.notice.take().is_some()
        }
    }

    fn fail(&mut self, message: String) {
        tracing::warn!(%message, "native file upload");
        self.notice = Some((message, Some(Instant::now() + Duration::from_secs(30))));
    }

    pub(super) async fn poll(
        &mut self,
        registry: &EndpointRegistry,
        shell: &mut ClientShellState,
    ) -> bool {
        if self
            .job
            .as_ref()
            .is_some_and(|job| !registry.accepts(&job.endpoint, job.generation))
        {
            self.job.take();
            self.fail("Upload stopped: destination disconnected or was replaced".into());
        }
        if let Some(job) = self.job.as_mut() {
            if job.updates.has_changed().unwrap_or(false) {
                let text = job.updates.borrow_and_update().clone();
                self.notice = Some((format!("{} · {text}", job.label), None));
            }
        }
        if self.job.as_ref().is_some_and(|job| job.task.is_finished()) {
            if let Some(mut job) = self.job.take() {
                match (&mut job.task).await {
                    Ok(Ok(Finished::Uploaded(upload))) => {
                        let (updates, receiver) = watch::channel("Inserting remote paths…".into());
                        self.notice =
                            Some((format!("{} · Inserting remote paths…", job.label), None));
                        job.updates = receiver;
                        job.task = tokio::spawn(async move {
                            let _updates = updates;
                            ssh::insert(&upload.ssh, &upload.target, &upload.pane, &upload.receipts).await
                                .with_context(|| format!("Files uploaded, but insertion failed. Paths: {}. Inspect the prompt before retrying.", references(&upload.receipts)))?;
                            Ok(Finished::Inserted)
                        });
                        self.job = Some(job);
                    }
                    Ok(Ok(Finished::Inserted)) => {
                        self.notice = Some((
                            format!("{} · Uploaded and inserted", job.label),
                            Some(Instant::now() + Duration::from_secs(1)),
                        ));
                    }
                    Ok(Err(error)) => self.fail(format!("{error:#}")),
                    Err(error) => self.fail(format!("Upload task failed: {error}")),
                }
            }
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, expires)| expires.is_some_and(|at| Instant::now() >= at))
        {
            self.notice = None;
        }
        shell.set_file_upload_notice(
            self.notice.as_ref().map(|(text, _)| text.clone()),
            self.job.is_some(),
        )
    }
}

fn references(receipts: &[ssh::Receipt]) -> String {
    receipts
        .iter()
        .map(|r| r.reference.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn dropped_paths(text: &str) -> Option<Vec<PathBuf>> {
    if text.len() > 1024 * 1024 || text.trim().len() < 2 {
        return None;
    }
    let paths = files::parse_paths(text).ok()?;
    if paths.is_empty()
        || !paths.iter().all(|path| path.is_absolute())
        || !paths
            .iter()
            .take(11)
            .all(|path| std::fs::symlink_metadata(path).is_ok())
    {
        return None;
    }
    Some(paths)
}

/// Strip only complete bracketed-paste framing. Incomplete framing stays with the
/// host input parser and must never start a transfer of a truncated path.
#[cfg(any(unix, test))]
pub(super) fn drop_text(data: &[u8]) -> Option<&str> {
    let data = if let Some(data) = data.strip_prefix(b"\x1b[200~") {
        data.strip_suffix(b"\x1b[201~")?
    } else {
        data
    };
    std::str::from_utf8(data).ok()
}

#[cfg(test)]
mod tests;
