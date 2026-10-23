use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// Presentation messages are coalesced: a slow renderer never queues byte events.
pub(super) struct Progress {
    name: String,
    size: u64,
    started: Instant,
    last: Mutex<Option<Instant>>,
    updates: watch::Sender<String>,
}

impl Progress {
    pub(super) fn new(name: &str, size: u64, updates: watch::Sender<String>) -> Self {
        Self {
            name: name.into(),
            size,
            started: Instant::now(),
            last: Mutex::new(None),
            updates,
        }
    }

    // Transfer rates and MiB are approximate presentation values, not byte counts.
    #[allow(clippy::cast_precision_loss)]
    pub(super) fn update(&self, received: u64, phase: &str) {
        let now = Instant::now();
        if let Ok(mut last) = self.last.lock() {
            if received != self.size
                && last.is_some_and(|at| now.duration_since(at) < Duration::from_millis(100))
            {
                return;
            }
            *last = Some(now);
        }
        let percent = received
            .saturating_mul(100)
            .checked_div(self.size)
            .unwrap_or(100);
        let filled = (percent / 10).min(10) as usize;
        self.updates.send_replace(format!(
            "{} [{}{}] {}% · {:.1}/{:.1} MiB · {:.1} MiB/s · {phase}",
            self.name,
            "━".repeat(filled),
            "─".repeat(10 - filled),
            percent,
            received as f64 / 1_048_576.0,
            self.size as f64 / 1_048_576.0,
            received as f64 / 1_048_576.0 / self.started.elapsed().as_secs_f64().max(0.001)
        ));
    }

    pub(super) fn finish(&self, phase: &str) {
        self.updates
            .send_replace(format!("{} · {phase}", self.name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_reports_bytes_and_finishes() {
        let (updates, receiver) = watch::channel(String::new());
        let progress = Progress::new("report.pdf", 2 * 1_048_576, updates);
        progress.update(1_048_576, "uploading");
        let message = receiver.borrow().clone();
        assert!(message.contains("50%"));
        assert!(message.contains("1.0/2.0 MiB"));
        progress.finish("uploaded and checksum verified");
        assert!(receiver.borrow().contains("uploaded and checksum verified"));
    }
}
