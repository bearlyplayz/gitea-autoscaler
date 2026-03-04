use std::collections::HashSet;

use anyhow::{Context, Result};
use reqwest::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use serde::Deserialize;
use tracing::debug;

use crate::config::{Config, GiteaAuth};

// ── Gitea API response types ───────────────────────────────────────────────

/// Matches `ActionWorkflowJobsResponse` from the Gitea OpenAPI spec.
#[derive(Debug, Deserialize)]
pub struct JobsResponse {
    #[serde(default)]
    pub jobs: Vec<Job>,
    pub total_count: u64,
}

/// Matches `ActionWorkflowJob` — only the fields we need.
#[derive(Debug, Deserialize)]
pub struct Job {
    pub id: u64,
    pub status: String,
    #[serde(default)]
    pub runner_name: String,
    #[serde(default)]
    pub name: String,
}

// ── Client ─────────────────────────────────────────────────────────────────

/// HTTP client for the Gitea admin Actions API.
pub struct GiteaClient {
    client: Client,
    base_url: String,
}

impl GiteaClient {
    /// Build a new client from application config.
    pub fn new(config: &Config) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        match &config.gitea_auth {
            GiteaAuth::Token(token) => {
                let val = HeaderValue::from_str(&format!("token {token}"))
                    .context("invalid GITEA_TOKEN value")?;
                headers.insert(AUTHORIZATION, val);
            }
            GiteaAuth::Basic { username, password } => {
                use base64::{Engine, engine::general_purpose::STANDARD};
                let encoded = STANDARD.encode(format!("{username}:{password}"));
                let val = HeaderValue::from_str(&format!("Basic {encoded}"))
                    .context("invalid basic-auth credentials")?;
                headers.insert(AUTHORIZATION, val);
            }
        }

        let client = Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            client,
            base_url: config.gitea_url.trim_end_matches('/').to_string(),
        })
    }

    /// Fetch all queued and in-progress jobs from the admin API.
    ///
    /// Returns `(total_job_count, set_of_busy_runner_pod_names)`.
    ///
    /// `total_count` from the API already represents all queued + in-progress
    /// jobs, so we only iterate pages to collect `runner_name` from in-progress
    /// jobs (needed for safe scale-down). Once `busy_runners >= max_replicas`
    /// we bail early — at max capacity there is nothing to scale.
    pub async fn get_jobs(&self, max_replicas: u32) -> Result<(u32, HashSet<String>)> {
        let mut busy_runners = HashSet::new();
        let mut total_count: u32;
        let mut page: u32 = 1;
        let limit: u32 = 50;

        loop {
            let url = format!(
                "{}/api/v1/admin/actions/jobs?status=queued&status=in_progress&limit={limit}&page={page}",
                self.base_url
            );

            let resp = self
                .client
                .get(&url)
                .send()
                .await
                .context("failed to reach Gitea API")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("Gitea API returned {status}: {body}");
            }

            let data: JobsResponse = resp
                .json()
                .await
                .context("failed to deserialize Gitea jobs response")?;

            // total_count covers all queued + in_progress jobs across all pages.
            total_count = data.total_count as u32;

            // We only need to iterate to collect busy runner names.
            for job in &data.jobs {
                if job.status == "in_progress" && !job.runner_name.is_empty() {
                    busy_runners.insert(job.runner_name.clone());
                    debug!(
                        job_id = job.id,
                        name = %job.name,
                        runner = %job.runner_name,
                        "in-progress job"
                    );
                }
            }

            // Fast exit: if busy runners already >= max, no scaling action is
            // possible (can't scale up past max, won't scale down while busy).
            if busy_runners.len() as u32 >= max_replicas {
                debug!(
                    busy = busy_runners.len(),
                    max_replicas, "busy runners at max, skipping remaining pages"
                );
                break;
            }

            // Check if we've fetched everything.
            let fetched = (page - 1) * limit + data.jobs.len() as u32;
            if fetched >= total_count || data.jobs.is_empty() {
                break;
            }
            page += 1;
        }

        // queued = total jobs minus the ones that are actively running.
        let queued = total_count.saturating_sub(busy_runners.len() as u32);
        debug!(
            total_count,
            queued,
            busy = busy_runners.len(),
            "job summary"
        );

        Ok((queued, busy_runners))
    }
}
