use std::collections::HashSet;

use anyhow::{Context, Result};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Pod;
use kube::Client;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams};
use tracing::{debug, info, warn};

/// Kubernetes scaling operations for the runner Deployment.
pub struct Scaler {
    deployments: Api<Deployment>,
    pods: Api<Pod>,
    deployment_name: String,
}

impl Scaler {
    /// Create a new Scaler using in-cluster or kubeconfig credentials.
    pub async fn new(namespace: &str, deployment_name: &str) -> Result<Self> {
        let client = Client::try_default()
            .await
            .context("failed to create Kubernetes client (are you running in-cluster?)")?;

        let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
        let pods: Api<Pod> = Api::namespaced(client, namespace);

        Ok(Self {
            deployments,
            pods,
            deployment_name: deployment_name.to_string(),
        })
    }

    /// Get the current replica count of the runner Deployment.
    pub async fn get_replicas(&self) -> Result<i32> {
        let deploy = self
            .deployments
            .get(&self.deployment_name)
            .await
            .context("failed to get runner Deployment")?;

        Ok(deploy.spec.and_then(|s| s.replicas).unwrap_or(1))
    }

    /// Set the replica count of the runner Deployment via the Scale subresource.
    pub async fn scale(&self, replicas: i32) -> Result<()> {
        let patch = serde_json::json!({
            "spec": { "replicas": replicas }
        });

        self.deployments
            .patch_scale(
                &self.deployment_name,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
            .context("failed to update Deployment scale")?;

        Ok(())
    }

    /// List running pod names for the runner Deployment.
    pub async fn list_runner_pods(&self) -> Result<Vec<String>> {
        let lp = ListParams::default().labels(&format!("app={}", self.deployment_name));

        let pod_list = self
            .pods
            .list(&lp)
            .await
            .context("failed to list runner pods")?;

        let names: Vec<String> = pod_list
            .items
            .into_iter()
            .filter(|pod| pod.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running"))
            .filter_map(|pod| pod.metadata.name)
            .collect();

        Ok(names)
    }

    /// List live pod names for the runner Deployment.
    pub async fn list_live_runner_pod_names(&self) -> Result<HashSet<String>> {
        let lp = ListParams::default().labels(&format!("app={}", self.deployment_name));

        let pod_list = self
            .pods
            .list(&lp)
            .await
            .context("failed to list live runner pods")?;

        let names = pod_list
            .items
            .into_iter()
            .filter(|pod| {
                !matches!(
                    pod.status
                        .as_ref()
                        .and_then(|status| status.phase.as_deref()),
                    Some("Succeeded") | Some("Failed")
                )
            })
            .filter_map(|pod| pod.metadata.name)
            .collect();

        Ok(names)
    }

    /// Delete a specific pod by name (30s graceful termination).
    pub async fn delete_pod(&self, name: &str) -> Result<()> {
        let dp = DeleteParams {
            grace_period_seconds: Some(30),
            ..Default::default()
        };
        self.pods
            .delete(name, &dp)
            .await
            .context(format!("failed to delete pod {name}"))?;
        Ok(())
    }

    /// Scale down safely by deleting only idle pods (those not in `busy_runners`).
    ///
    /// 1. List all running runner pods.
    /// 2. Separate into busy (name appears in Gitea in-progress jobs) and idle.
    /// 3. Delete idle pods until we reach `target` replicas.
    /// 4. Adjust the Deployment replica count to match.
    pub async fn safe_scale_down(&self, target: i32, busy_runners: &HashSet<String>) -> Result<()> {
        let pods = self.list_runner_pods().await?;
        let current = pods.len() as i32;
        let to_remove = current - target;

        if to_remove <= 0 {
            return Ok(());
        }

        // Partition into idle and busy.
        let idle: Vec<&String> = pods.iter().filter(|p| !busy_runners.contains(*p)).collect();

        if idle.is_empty() {
            warn!(
                current,
                busy = busy_runners.len(),
                "skip scale-down: all pods are busy"
            );
            return Ok(());
        }

        // Only delete from the idle pool, up to `to_remove` pods.
        let removable = &idle[..std::cmp::min(idle.len(), to_remove as usize)];
        let mut deleted = 0u32;

        for pod_name in removable {
            info!(pod = %pod_name, "deleting idle runner pod");
            match self.delete_pod(pod_name).await {
                Ok(()) => deleted += 1,
                Err(e) => warn!(pod = %pod_name, error = %e, "failed to delete pod"),
            }
        }

        // Set the Deployment replica count to the new level.
        let new_count = std::cmp::max(target, current - deleted as i32);
        self.scale(new_count).await?;
        info!(new_count, deleted, "scale-down complete");

        debug!(
            idle_count = idle.len(),
            busy_count = busy_runners.len(),
            "pod partition details"
        );

        Ok(())
    }
}
