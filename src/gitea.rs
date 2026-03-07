use std::collections::HashSet;

use anyhow::{Context, Result};
use reqwest::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use serde::Deserialize;
use tracing::{debug, info};

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

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct Runner {
    #[serde(default)]
    pub busy: bool,
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct RunnersResponse {
    #[serde(default)]
    pub runners: Vec<Runner>,
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

    /// Fetch queued jobs and live in-progress jobs from the admin API.
    ///
    /// Returns `(queued_job_count, set_of_busy_live_runner_pod_names)`.
    ///
    /// In-progress jobs whose `runner_name` is not present in Kubernetes are
    /// treated as stale and ignored for scaling decisions. This makes the
    /// autoscaler resilient to jobs Gitea still shows as running after a runner
    /// has already disappeared.
    pub async fn get_jobs(
        &self,
        max_replicas: u32,
        live_runner_names: &HashSet<String>,
    ) -> Result<(u32, HashSet<String>)> {
        let mut busy_runners = HashSet::new();
        let mut queued = 0u32;
        let mut stale_in_progress = 0u32;
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

            let page_total = data.total_count as u32;

            for job in &data.jobs {
                match job.status.as_str() {
                    "queued" => queued += 1,
                    "in_progress" if !job.runner_name.is_empty() => {
                        if live_runner_names.contains(&job.runner_name) {
                            busy_runners.insert(job.runner_name.clone());
                            debug!(
                                job_id = job.id,
                                name = %job.name,
                                runner = %job.runner_name,
                                "in-progress job on live runner"
                            );
                        } else {
                            stale_in_progress += 1;
                            debug!(
                                job_id = job.id,
                                name = %job.name,
                                runner = %job.runner_name,
                                "ignoring in-progress job on non-live runner"
                            );
                        }
                    }
                    _ => {}
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
            if fetched >= page_total || data.jobs.is_empty() {
                break;
            }
            page += 1;
        }

        debug!(
            queued,
            busy = busy_runners.len(),
            stale_in_progress,
            "job summary"
        );

        if stale_in_progress > 0 {
            info!(
                stale_in_progress,
                "ignored stale in-progress jobs on non-live runners"
            );
        }

        Ok((queued, busy_runners))
    }

    pub async fn cleanup_offline_runners(
        &self,
        live_runner_names: &HashSet<String>,
        busy_runners: &HashSet<String>,
    ) -> Result<u32> {
        let runners = self.list_runners().await?;
        let stale_runner_ids =
            select_stale_offline_runners(&runners, live_runner_names, busy_runners);
        let stale_count = stale_runner_ids.len() as u32;

        for runner in stale_runner_ids {
            info!(runner_id = runner.id, runner = %runner.name, "deleting stale offline runner");
            self.delete_runner(runner.id).await?;
        }

        Ok(stale_count)
    }

    async fn list_runners(&self) -> Result<Vec<Runner>> {
        let url = format!("{}/api/v1/admin/actions/runners", self.base_url);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("failed to reach Gitea runners API")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gitea runners API returned {status}: {body}");
        }

        let data: RunnersResponse = resp
            .json()
            .await
            .context("failed to deserialize Gitea runners response")?;

        Ok(data.runners)
    }

    async fn delete_runner(&self, runner_id: u64) -> Result<()> {
        let url = format!("{}/api/v1/admin/actions/runners/{runner_id}", self.base_url);

        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .context("failed to reach Gitea delete runner API")?;

        if resp.status() != reqwest::StatusCode::NO_CONTENT {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gitea delete runner API returned {status}: {body}");
        }

        Ok(())
    }
}

fn select_stale_offline_runners<'a>(
    runners: &'a [Runner],
    live_runner_names: &HashSet<String>,
    busy_runners: &HashSet<String>,
) -> Vec<&'a Runner> {
    runners
        .iter()
        .filter(|runner| runner.status.eq_ignore_ascii_case("offline"))
        .filter(|runner| !runner.busy)
        .filter(|runner| !live_runner_names.contains(&runner.name))
        .filter(|runner| !busy_runners.contains(&runner.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Runner, select_stale_offline_runners};
    use std::collections::HashSet;

    fn runner(id: u64, name: &str, status: &str, busy: bool) -> Runner {
        Runner {
            busy,
            id,
            name: name.to_string(),
            status: status.to_string(),
        }
    }

    #[test]
    fn selects_offline_runners_missing_from_cluster() {
        let live_runner_names = HashSet::from(["runner-live".to_string()]);
        let busy_runners = HashSet::new();
        let runners = vec![
            runner(1, "runner-live", "offline", false),
            runner(2, "runner-stale", "offline", false),
        ];

        let selected = select_stale_offline_runners(&runners, &live_runner_names, &busy_runners);

        assert_eq!(selected, vec![&runners[1]]);
    }

    #[test]
    fn skips_online_and_busy_runners() {
        let live_runner_names = HashSet::new();
        let busy_runners = HashSet::from(["runner-busy".to_string(), "runner-job".to_string()]);
        let runners = vec![
            runner(1, "runner-online", "online", false),
            runner(2, "runner-busy", "offline", true),
            runner(3, "runner-job", "offline", false),
        ];

        let selected = select_stale_offline_runners(&runners, &live_runner_names, &busy_runners);

        assert_eq!(selected, Vec::<&Runner>::new());
    }

    #[test]
    fn matches_offline_status_case_insensitively() {
        let live_runner_names = HashSet::new();
        let busy_runners = HashSet::new();
        let runners = vec![runner(9, "runner-stale", "OffLine", false)];

        let selected = select_stale_offline_runners(&runners, &live_runner_names, &busy_runners);

        assert_eq!(selected, vec![&runners[0]]);
    }
}
