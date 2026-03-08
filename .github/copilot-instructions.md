# Gitea-Autoscaler | Copilot Instructions

This repository contains a small Rust controller that scales Gitea Actions runner pods up and down by polling the Gitea admin API and patching Kubernetes resources directly.

## Stack

- Rust 2024
- `kube` and `k8s-openapi` for Kubernetes access
- `reqwest` with rustls for Gitea API calls
- `tokio` for the control loop
- `tracing` for logs

## Core Behavior

The intended control loop is:

1. Poll queued and in-progress Gitea action jobs.
2. Compute desired replicas with min/max clamping.
3. Scale up immediately when demand increases.
4. Scale down only after the cooldown and only by deleting idle runner pods.
5. Clean up stale runner registrations that no longer map to live pods.

Any change that weakens those guarantees is a regression.

## Non-Negotiable Rules

1. Never kill a busy runner.
Safe scale-down matters more than fast scale-down.

2. Keep the controller simple.
This project intentionally avoids Prometheus, KEDA, CRDs, and unnecessary framework layers.

3. Keep decision logic explicit.
Replica calculations, cooldown logic, and stale-runner cleanup should stay easy to reason about and test.

4. Do not downgrade dependencies.
If a crate update is needed, move forward and fix compatibility issues.

5. Zero-warning policy.
Changes should pass `cargo fmt`, `cargo clippy --all-targets --all-features`, and `cargo test`.

## Code Organization

- `config.rs` owns environment parsing and validation.
- `gitea.rs` owns Gitea API interaction.
- `scaler.rs` owns Kubernetes scaling and pod deletion behavior.
- `main.rs` should remain a thin orchestration loop.

Keep responsibilities separated. Do not bury Gitea HTTP code inside scaling code or vice versa.

## Testing Expectations

- Prefer extracting pure decision logic so replica math and cooldown behavior can be tested without Kubernetes.
- Add coverage for:
	- happy path scaling decisions
	- empty or zero-work queue behavior
	- failure paths such as Gitea API errors or Kubernetes patch/delete failures
- When changing cleanup logic, verify offline-runner deletion still avoids removing valid active runners.

## Repository Boundaries

- This repo owns the autoscaler binary.
- Kubernetes deployment manifests for the autoscaler live in `MyServer` under the cluster infrastructure repo.
- Do not move cluster bootstrap or runner deployment policy into this repository.

## Definition of Done

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features`
- `cargo test`
- Safe scale-down behavior preserved
- Config changes documented through env-var handling and defaults
