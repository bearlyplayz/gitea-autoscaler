use std::env;

use anyhow::{Context, Result, bail};

/// Authentication method for the Gitea API.
#[derive(Debug, Clone)]
pub enum GiteaAuth {
    Basic { username: String, password: String },
    Token(String),
}

/// Application configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    /// Base URL of the Gitea instance (e.g. `http://gitea-http.gitea.svc.cluster.local:3000`).
    pub gitea_url: String,
    /// Authentication credentials for the Gitea admin API.
    pub gitea_auth: GiteaAuth,
    /// Name of the runner Deployment to scale.
    pub runner_deployment: String,
    /// Kubernetes namespace of the runner Deployment.
    pub runner_namespace: String,
    /// Minimum replica count (floor).
    pub min_replicas: i32,
    /// Maximum replica count (ceiling).
    pub max_replicas: i32,
    /// Seconds between polling cycles.
    pub poll_interval: u64,
    /// Seconds to wait after last activity before scaling down.
    pub scale_down_delay: u64,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Required:
    ///   - `GITEA_URL`
    ///   - One of: `GITEA_TOKEN` **or** (`GITEA_USERNAME` + `GITEA_PASSWORD`)
    ///
    /// Optional (with defaults):
    ///   - `RUNNER_DEPLOYMENT`  (default: `gitea-actions-runner`)
    ///   - `RUNNER_NAMESPACE`   (default: `gitea`)
    ///   - `MIN_REPLICAS`       (default: `1`)
    ///   - `MAX_REPLICAS`       (default: `3`)
    ///   - `POLL_INTERVAL`      (default: `10`)
    ///   - `SCALE_DOWN_DELAY`   (default: `1000`)
    pub fn from_env() -> Result<Self> {
        let gitea_url =
            env::var("GITEA_URL").context("GITEA_URL environment variable is required")?;

        // Determine auth method: token takes precedence over basic auth.
        let gitea_auth = if let Ok(token) = env::var("GITEA_TOKEN") {
            GiteaAuth::Token(token)
        } else {
            let username = env::var("GITEA_USERNAME")
                .context("Either GITEA_TOKEN or GITEA_USERNAME+GITEA_PASSWORD must be set")?;
            let password = env::var("GITEA_PASSWORD")
                .context("GITEA_PASSWORD is required when using GITEA_USERNAME")?;
            GiteaAuth::Basic { username, password }
        };

        let runner_deployment =
            env::var("RUNNER_DEPLOYMENT").unwrap_or_else(|_| "gitea-actions-runner".to_string());
        let runner_namespace = env::var("RUNNER_NAMESPACE").unwrap_or_else(|_| "gitea".to_string());

        let min_replicas = env::var("MIN_REPLICAS")
            .unwrap_or_else(|_| "1".to_string())
            .parse::<i32>()
            .context("MIN_REPLICAS must be a valid integer")?;
        let max_replicas = env::var("MAX_REPLICAS")
            .unwrap_or_else(|_| "3".to_string())
            .parse::<i32>()
            .context("MAX_REPLICAS must be a valid integer")?;

        if min_replicas < 0 {
            bail!("MIN_REPLICAS must be >= 0, got {min_replicas}");
        }
        if max_replicas < min_replicas {
            bail!("MAX_REPLICAS ({max_replicas}) must be >= MIN_REPLICAS ({min_replicas})");
        }

        let poll_interval = env::var("POLL_INTERVAL")
            .unwrap_or_else(|_| "10".to_string())
            .parse::<u64>()
            .context("POLL_INTERVAL must be a valid integer")?;
        let scale_down_delay = env::var("SCALE_DOWN_DELAY")
            .unwrap_or_else(|_| "1000".to_string())
            .parse::<u64>()
            .context("SCALE_DOWN_DELAY must be a valid integer")?;

        Ok(Self {
            gitea_url,
            gitea_auth,
            runner_deployment,
            runner_namespace,
            min_replicas,
            max_replicas,
            poll_interval,
            scale_down_delay,
        })
    }

    /// Redacted summary suitable for logging at startup.
    pub fn summary(&self) -> String {
        let auth_kind = match &self.gitea_auth {
            GiteaAuth::Basic { username, .. } => format!("basic (user={username})"),
            GiteaAuth::Token(_) => "token".to_string(),
        };
        format!(
            "gitea={} auth={} deployment={}/{} replicas=[{},{}] poll={}s cooldown={}s",
            self.gitea_url,
            auth_kind,
            self.runner_namespace,
            self.runner_deployment,
            self.min_replicas,
            self.max_replicas,
            self.poll_interval,
            self.scale_down_delay,
        )
    }
}
