mod config;
mod gitea;
mod scaler;

use std::time::Instant;

use anyhow::Result;
use tokio::time::{Duration, sleep};
use tracing::{error, info, warn};

use config::Config;
use gitea::GiteaClient;
use scaler::Scaler;

#[derive(Debug, PartialEq, Eq)]
enum ScaleDecision {
    ScaleUp,
    ScaleDown,
    WaitCooldown { remaining: u64 },
    Noop,
}

fn decide_scale_action(
    current: i32,
    desired: i32,
    cooldown_secs: u64,
    scale_down_since: &mut Option<Instant>,
) -> ScaleDecision {
    if desired > current {
        *scale_down_since = None;
        return ScaleDecision::ScaleUp;
    }

    if desired == current {
        *scale_down_since = None;
        return ScaleDecision::Noop;
    }

    let started_at = scale_down_since.get_or_insert_with(Instant::now);
    let elapsed = started_at.elapsed().as_secs();
    if elapsed >= cooldown_secs {
        ScaleDecision::ScaleDown
    } else {
        ScaleDecision::WaitCooldown {
            remaining: cooldown_secs - elapsed,
        }
    }
}

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

    let mut scale_down_since = None;

    // ── main control loop ──────────────────────────────────────────────
    loop {
        if let Err(e) = tick(&gitea, &k8s, min, max, cooldown, &mut scale_down_since).await {
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
    scale_down_since: &mut Option<Instant>,
) -> Result<()> {
    let live_runner_names = k8s.list_live_runner_pod_names().await?;
    let (queued, busy_runners) = gitea.get_jobs(max as u32, &live_runner_names).await?;
    let running = busy_runners.len() as i32;
    let desired = (queued as i32 + running).clamp(min, max);
    let current = k8s.get_replicas().await?;

    match decide_scale_action(current, desired, cooldown_secs, scale_down_since) {
        ScaleDecision::ScaleUp => {
            info!(current, desired, running, queued, "SCALE UP");
            k8s.scale(desired).await?;
        }
        ScaleDecision::ScaleDown => {
            info!(current, desired, running, queued, "SCALE DOWN");
            k8s.safe_scale_down(desired, &busy_runners).await?;
        }
        ScaleDecision::WaitCooldown { remaining } => {
            info!(
                current,
                desired, remaining, running, queued, "cooldown (scale-down pending)"
            );
        }
        ScaleDecision::Noop => {
            info!(replicas = current, running, queued, "ok");
        }
    }

    match gitea
        .cleanup_offline_runners(&live_runner_names, &busy_runners)
        .await
    {
        Ok(deleted) if deleted > 0 => {
            info!(deleted, "removed stale offline runner registrations")
        }
        Ok(_) => {}
        Err(e) => warn!(error = %e, "offline runner cleanup failed"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ScaleDecision, decide_scale_action};
    use std::time::{Duration, Instant};

    #[test]
    fn scales_up_immediately_and_clears_cooldown() {
        let mut scale_down_since = Some(Instant::now() - Duration::from_secs(120));

        let decision = decide_scale_action(2, 4, 300, &mut scale_down_since);

        assert_eq!(decision, ScaleDecision::ScaleUp);
        assert!(scale_down_since.is_none());
    }

    #[test]
    fn waits_for_cooldown_when_surplus_first_appears() {
        let mut scale_down_since = None;

        let decision = decide_scale_action(6, 2, 300, &mut scale_down_since);

        assert_eq!(decision, ScaleDecision::WaitCooldown { remaining: 300 });
        assert!(scale_down_since.is_some());
    }

    #[test]
    fn scales_down_after_cooldown_expires() {
        let mut scale_down_since = Some(Instant::now() - Duration::from_secs(301));

        let decision = decide_scale_action(6, 2, 300, &mut scale_down_since);

        assert_eq!(decision, ScaleDecision::ScaleDown);
        assert!(scale_down_since.is_some());
    }

    #[test]
    fn equal_replicas_clear_cooldown_state() {
        let mut scale_down_since = Some(Instant::now() - Duration::from_secs(301));

        let decision = decide_scale_action(2, 2, 300, &mut scale_down_since);

        assert_eq!(decision, ScaleDecision::Noop);
        assert!(scale_down_since.is_none());
    }
}
