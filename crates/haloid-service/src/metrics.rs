//! Prometheus metrics, kept as plain atomic counters and rendered as the
//! text exposition format. No metrics library: the set is small, and every
//! number here is one an operator would alert on.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Why an upload ended, for `haloid_uploads_total{outcome}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadOutcome {
    Stored,
    Duplicate,
    /// Refused for something about the request: type, size, limit, rights.
    Refused,
    /// Failed for something of ours: storage, database, media tools.
    Failed,
}

#[derive(Debug, Default)]
pub struct Metrics {
    uploads: [AtomicU64; 4],
    upload_bytes: AtomicU64,
    http: [AtomicU64; 5],
    jobs_ok: AtomicU64,
    jobs_failed: AtomicU64,
    token: OnceLock<String>,
}

fn add(counter: &AtomicU64, n: u64) {
    counter.fetch_add(n, Ordering::Relaxed);
}

impl Metrics {
    pub fn upload(&self, outcome: UploadOutcome, bytes: u64) {
        add(&self.uploads[outcome as usize], 1);
        if outcome == UploadOutcome::Stored {
            add(&self.upload_bytes, bytes);
        }
    }

    /// One HTTP response, by status class.
    pub fn response(&self, status: u16) {
        let class = (status / 100).clamp(1, 5) as usize - 1;
        add(&self.http[class], 1);
    }

    pub fn job(&self, ok: bool) {
        add(if ok { &self.jobs_ok } else { &self.jobs_failed }, 1);
    }

    /// A bearer token a scraper may use instead of an admin's key, from
    /// `HALOID_METRICS_TOKEN`. Set once, at startup.
    pub fn set_token(&self, token: String) {
        let _ = self.token.set(token);
    }

    pub fn token(&self) -> Option<&str> {
        self.token.get().map(String::as_str)
    }

    /// The text format, with the gauges the caller read just now.
    pub fn render(&self, gauges: &[(&str, &str, u64)]) -> String {
        let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
        let mut out = String::new();
        let mut family = |name: &str, kind: &str, help: &str, samples: &[(String, u64)]| {
            out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
            for (labels, value) in samples {
                out.push_str(&format!("{name}{labels} {value}\n"));
            }
        };
        family(
            "haloid_uploads_total",
            "counter",
            "Uploads by how they ended.",
            &[
                ("stored", UploadOutcome::Stored),
                ("duplicate", UploadOutcome::Duplicate),
                ("refused", UploadOutcome::Refused),
                ("failed", UploadOutcome::Failed),
            ]
            .map(|(label, o)| {
                (
                    format!("{{outcome=\"{label}\"}}"),
                    load(&self.uploads[o as usize]),
                )
            }),
        );
        family(
            "haloid_upload_bytes_total",
            "counter",
            "Bytes received in stored uploads.",
            &[(String::new(), load(&self.upload_bytes))],
        );
        family(
            "haloid_http_responses_total",
            "counter",
            "HTTP responses by status class.",
            &(0..5)
                .map(|i| (format!("{{class=\"{}xx\"}}", i + 1), load(&self.http[i])))
                .collect::<Vec<_>>(),
        );
        family(
            "haloid_jobs_total",
            "counter",
            "Background job runs by result.",
            &[
                ("{outcome=\"ok\"}".to_string(), load(&self.jobs_ok)),
                ("{outcome=\"failed\"}".to_string(), load(&self.jobs_failed)),
            ],
        );
        for (name, help, value) in gauges {
            family(name, "gauge", help, &[(String::new(), *value)]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_prometheus_text_format() {
        let m = Metrics::default();
        m.upload(UploadOutcome::Stored, 100);
        m.upload(UploadOutcome::Stored, 50);
        m.upload(UploadOutcome::Refused, 9);
        m.response(200);
        m.response(404);
        m.response(503);
        m.job(false);
        let text = m.render(&[("haloid_moderation_pending", "Files awaiting review.", 3)]);
        for line in [
            "# TYPE haloid_uploads_total counter",
            "haloid_uploads_total{outcome=\"stored\"} 2",
            "haloid_uploads_total{outcome=\"refused\"} 1",
            "haloid_upload_bytes_total 150",
            "haloid_http_responses_total{class=\"4xx\"} 1",
            "haloid_http_responses_total{class=\"5xx\"} 1",
            "haloid_jobs_total{outcome=\"failed\"} 1",
            "# TYPE haloid_moderation_pending gauge",
            "haloid_moderation_pending 3",
        ] {
            assert!(
                text.lines().any(|l| l == line),
                "missing {line:?} in\n{text}"
            );
        }
    }
}
