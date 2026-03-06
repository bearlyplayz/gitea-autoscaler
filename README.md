# Gitea-Autoscaler

A lightweight Rust controller that automatically scales Gitea Actions runner pods based on the job queue. It polls the Gitea admin API directly — no Prometheus, no KEDA, no CRDs.

## How It Works

```
┌──────────────────┐         poll every N seconds         ┌──────────────┐
│  Gitea Admin API │ ◄─────────────────────────────────── │  Autoscaler  │
│  /admin/actions/  │  queued + in-progress job counts    │  (this app)  │
│  jobs             │ ──────────────────────────────────► │              │
└──────────────────┘                                      │  desired =   │
                                                          │  clamp(      │
┌──────────────────┐    scale / safe_scale_down           │   queued +   │
│  Runner          │ ◄─────────────────────────────────── │   running,   │
│  Deployment      │    (Kubernetes API)                  │   min, max)  │
└──────────────────┘                                      └──────────────┘
```

1. **Poll** — Queries `GET /api/v1/admin/actions/jobs?status=queued&status=in_progress` to get the total job count and which runners are busy.
2. **Decide** — Computes `desired = clamp(queued + running, min, max)`.
3. **Scale up** — Immediately patches the Deployment replica count.
4. **Scale down** — Waits for a configurable cooldown period, then deletes only *idle* pods (never kills a pod running a job).
5. **Cleanup** — Deletes stale Gitea runner registrations when a runner is offline in Gitea and no matching runner pod still exists in Kubernetes.

## Configuration

All configuration is via environment variables:

| Variable | Required | Default | Description |
|---|---|---|---|
| `GITEA_URL` | Yes | — | Base URL of the Gitea instance |
| `GITEA_TOKEN` | One of | — | API token (takes precedence over basic auth) |
| `GITEA_USERNAME` | One of | — | Basic auth username |
| `GITEA_PASSWORD` | One of | — | Basic auth password (required with username) |
| `RUNNER_DEPLOYMENT` | No | `gitea-actions-runner` | Name of the Deployment to scale |
| `RUNNER_NAMESPACE` | No | `gitea` | Namespace of the runner Deployment |
| `MIN_REPLICAS` | No | `1` | Minimum replica count (floor) |
| `MAX_REPLICAS` | No | `3` | Maximum replica count (ceiling) |
| `POLL_INTERVAL` | No | `10` | Seconds between polling cycles |
| `SCALE_DOWN_DELAY` | No | `1000` | Seconds of inactivity before scaling down |

Set `RUST_LOG=debug` for verbose logging.

## Kubernetes RBAC

The autoscaler needs a `ServiceAccount` with these permissions in the runner namespace:

```yaml
rules:
  - apiGroups: ["apps"]
    resources: ["deployments", "deployments/scale"]
    verbs: ["get", "patch"]
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["get", "list", "delete"]
```

## Docker Image

Multi-stage build — final image is `debian:bookworm-slim` (~30 MB) running as non-root.

```bash
docker build -t gitea.bearly.local/bearlyprojects/gitea-autoscaler:latest .
docker push gitea.bearly.local/bearlyprojects/gitea-autoscaler:latest
```

## Project Structure

```
src/
├── main.rs     # Entry point, control loop (poll → decide → scale)
├── config.rs   # Environment variable parsing + validation
├── gitea.rs    # Gitea admin API client (job queue queries + stale runner cleanup)
└── scaler.rs   # Kubernetes Deployment scaling + safe pod deletion
``` 
