mod config;
mod gitea;
mod scaler;

use std::time::Instant;

use anyhow::Result;
use tokio::time::{Duration, sleep};
use tracing::{error, info};

use config::Config;
use gitea::GiteaClient;
use scaler::Scaler;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialise structured logging (respects RUST_LOG env var).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .compact()
        .init();

    // Load configuration from environment.
    let config = Config::from_env()?;
    info!(config = %config.summary(), "autoscaler starting");

    // Build clients.
    let gitea = GiteaClient::new(&config)?;
    let k8s = Scaler::new(&config.runner_namespace, &config.runner_deployment).await?;

    let poll = Duration::from_secs(config.poll_interval);
    let cooldown = config.scale_down_delay;
    let min = config.min_replicas;
    let max = config.max_replicas;

    let mut last_busy = Instant::now();

    // ── main control loop ──────────────────────────────────────────────
    loop {
        if let Err(e) = tick(&gitea, &k8s, min, max, cooldown, &mut last_busy).await {
            error!(error = %e, "poll cycle failed");
        }
        sleep(poll).await;
    }
}

/// Single iteration of the autoscaler control loop.
async fn tick(
    gitea: &GiteaClient,
    k8s: &Scaler,
    min: i32,
    max: i32,
    cooldown_secs: u64,
    last_busy: &mut Instant,
) -> Result<()> {
    let (queued, busy_runners) = gitea.get_jobs(max as u32).await?;
    let running = busy_runners.len() as i32;
    let desired = (queued as i32 + running).clamp(min, max);
    let current = k8s.get_replicas().await?;

    // Reset cooldown timer whenever there is any activity.
    if queued > 0 || running > 0 {
        *last_busy = Instant::now();
    }

    if desired > current {
        // ── scale up immediately ───────────────────────────────────────
        info!(current, desired, running, queued, "SCALE UP");
        k8s.scale(desired).await?;
    } else if desired < current {
        // ── scale down (with cooldown) ─────────────────────────────────
        let elapsed = last_busy.elapsed().as_secs();
        if elapsed >= cooldown_secs {
            info!(current, desired, running, queued, "SCALE DOWN");
            k8s.safe_scale_down(desired, &busy_runners).await?;
        } else {
            let remaining = cooldown_secs - elapsed;
            info!(
                current,
                desired, remaining, running, queued, "cooldown (scale-down pending)"
            );
        }
    } else {
        info!(replicas = current, running, queued, "ok");
    }

    Ok(())
}
