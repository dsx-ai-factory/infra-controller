<!--
SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
SPDX-License-Identifier: Apache-2.0
-->

# NVIDIA Infrastructure Controller (NICo) REST API

A collection of microservices that comprise the management backend for NVIDIA Infrastructure Controller (NICo), exposed as a REST API.

In deployments, NVIDIA Infrastructure Controller REST requires Core services to be available.

The REST layer can be deployed in the datacenter with NVIDIA Infrastructure Controller Core, or deployed anywhere in Cloud and allow Site Agent to connect from the datacenter. Multiple NVIDIA Infrastructure Controller Cores running in different datacenters can also connect to NVIDIA Infrastructure Controller REST through respective Site Agents.

View the latest OpenAPI schema in the [REST API Reference](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference).

## Prerequisites

- Go 1.26.4 or later
- Docker 20.10+ with BuildKit enabled
- Make
- [Kind](https://kind.sigs.k8s.io/docs/user/quick-start/#installation) (for local deployment)
- [kubectl](https://kubernetes.io/docs/tasks/tools/) (for local deployment)
- [jq](https://stedolan.github.io/jq/) (optional, for parsing JSON responses)

## Core Compatibility Matrix

| NICo REST Version | NICo Core Version |
|------------------|------------------|
| v1.6.x           | v0.10.x           |
| v1.5.x           | v0.9.x           |
| v1.4.x           | v0.8.x           |
| v1.3.x           | v0.7.x           |

Versions older than v1.3.0 are no longer supported.

## Quick Start

### Run Unit Tests

```bash
make test
```

Tests require PostgreSQL. The Makefile automatically manages a test container.

Test database configuration:

- Host: `localhost`
- Port: `30432`
- User/Password: `postgres` / `postgres`

### Option A: Local Development with Kind

The fastest path to a running stack on your laptop. Builds images locally, spins up a Kind cluster, and deploys a mock NVIDIA Infrastructure Controller Core automatically — no external registry or bare-metal cluster required.

```bash
make kind-reset
```

This deploys the full stack via **Helm charts** (default). It:

1. Creates a Kind Kubernetes cluster
2. Builds all Docker images
3. Sets up infrastructure (PostgreSQL, Temporal, Keycloak, cert-manager, etc.)
4. Deploys app services via Helm umbrella chart
5. Bootstraps and deploys site-agent
6. Deploys a mock NVIDIA Infrastructure Controller Core

To deploy via **Kustomize overlays** instead:

```bash
make kind-reset-kustomize
```

Once complete, services are available at:

| Service | URL |
|---------|-----|
| API | [http://localhost:8388](http://localhost:8388) |
| Keycloak | [http://localhost:8082](http://localhost:8082) |
| Temporal UI | [http://localhost:8233](http://localhost:8233) |
| Adminer (DB UI) | [http://localhost:8081](http://localhost:8081) |

Other useful commands:

```bash
make kind-status         # Check pod status
make kind-logs           # Tail API logs
make kind-redeploy       # Rebuild and restart after code changes (Kustomize)
make helm-redeploy       # Rebuild and restart after code changes (Helm)
make kind-verify         # Run health checks
make helm-verify         # Check Helm deployment rollout status
make helm-uninstall      # Uninstall Helm releases
make kind-down           # Tear down cluster
```

### Option B: Bare-Metal Cluster with helm-prereqs

For deploying onto a real Kubernetes cluster alongside Core services. Uses [helm-prereqs/setup.sh](../helm-prereqs/setup.sh), which installs the full prerequisite stack (cert-manager, Vault, external-secrets, PostgreSQL, Temporal, Keycloak) and deploys both NICo Core and NICo REST in the correct order.

```bash
# 1. Build and push images to your registry
make docker-build IMAGE_REGISTRY=my-registry.example.com/nico IMAGE_TAG=v1.0.4

for image in nico-rest-api nico-rest-workflow nico-rest-site-manager \
             nico-rest-site-agent nico-rest-db nico-rest-cert-manager; do
    docker push my-registry.example.com/nico/$image:v1.0.4
done

# 2. Set environment variables
export KUBECONFIG=/path/to/kubeconfig
export REGISTRY_PULL_SECRET=<pull-secret-or-api-key>
export NICO_IMAGE_REGISTRY=my-registry.example.com/nico
export NICO_CORE_IMAGE_TAG=<nico-core-tag>    # NVIDIA Infrastructure Controller Core image tag
export NICO_REST_IMAGE_TAG=v1.0.4               # NICo REST image tag

# 4. Run setup
cd ../helm-prereqs
./setup.sh -y     # or ./setup.sh for interactive prompts
```

To tear everything down:

```bash
./clean.sh
```

See [helm-prereqs/README.md](../helm-prereqs/README.md) for the full reference: PKI architecture, phase-by-phase description, site customization, secrets reference, and troubleshooting (including site-agent gRPC connectivity).

### Option C: Manual / Kustomize Production Deployment

See **[Deployment QuickStart Guide](deploy/README.md)** for a concise bring-up guide, and **[Detailed Installation Guide](deploy/INSTALLATION.md)** for the full step-by-step reference with per-component explanations.

## CLI

`nicocli` is a command-line client that wraps the full REST API. Install it and set up configs for each environment you work with:

```bash
make nico-cli             # build and install to $GOPATH/bin
nicocli init              # generate ~/.nico/config.yaml
```

Create a config per environment (`~/.nico/config.yaml`, `~/.nico/config.staging.yaml`, `~/.nico/config.prod.yaml`), then launch the interactive TUI which handles environment selection, login, and token refresh automatically:

```bash
nicocli tui
```

All commands are also available directly for scripting and one-off use:

```bash
nicocli --config ~/.nico/config.staging.yaml site list
```

See [cli/README.md](cli/README.md) for configuration, authentication, shell completion, and the full command reference.

## Using the API

### Get an Access Token

```bash
TOKEN=$(curl -s -X POST "http://localhost:8082/realms/nico-dev/protocol/openid-connect/token" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d "client_id=nico-api" \
  -d "client_secret=nico-local-secret" \
  -d "grant_type=password" \
  -d "username=admin@example.com" \
  -d "password=adminpassword" | jq -r .access_token)
```

### Example API Requests

```bash
# Health check
curl -s http://localhost:8388/healthz -H "Authorization: Bearer $TOKEN" | jq .

# Get current tenant (auto-creates on first access)
curl -s "http://localhost:8388/v2/org/test-org/nico/tenant/current" \
  -H "Authorization: Bearer $TOKEN" | jq .

# List sites
curl -s "http://localhost:8388/v2/org/test-org/nico/site" \
  -H "Authorization: Bearer $TOKEN" | jq .
```

### Test Users

| Email | Password | Roles |
|-------|----------|-------|
| `admin@example.com` | `adminpassword` | PROVIDER_ADMIN, TENANT_ADMIN |
| `testuser@example.com` | `testpassword` | TENANT_ADMIN |
| `provider@example.com` | `providerpassword` | PROVIDER_ADMIN |

All users have the `test-org` organization assigned.

## Building Docker Images

### Build All Images

```bash
make docker-build
```

Images are tagged with `localhost:5000` registry and `latest` tag by default.

### Build with Custom Registry and Tag

```bash
make docker-build IMAGE_REGISTRY=my-registry.example.com/nico IMAGE_TAG=v1.0.0
```

### Push to Your Registry

1. Authenticate with your registry:

```bash
# Docker Hub
docker login

# AWS ECR
aws ecr get-login-password --region us-east-1 | docker login --username AWS --password-stdin 123456789.dkr.ecr.us-east-1.amazonaws.com

# Google Container Registry
gcloud auth configure-docker

# Azure Container Registry
az acr login --name myregistry
```

2. Build and push:

```bash
REGISTRY=my-registry.example.com/infra-controller
TAG=v1.0.0

make docker-build IMAGE_REGISTRY=$REGISTRY IMAGE_TAG=$TAG

for image in nico-rest-api nico-rest-workflow nico-rest-site-manager nico-rest-site-agent nico-rest-db nico-rest-cert-manager; do
    docker push "$REGISTRY/$image:$TAG"
done
```

### Available Images

| Image | Description |
|-------|-------------|
| `nico-rest-api` | Main REST API (port 8388) |
| `nico-rest-workflow` | Temporal workflow worker |
| `nico-rest-site-manager` | Site management worker |
| `nico-rest-site-agent` | On-site agent |
| `nico-rest-db` | Database migrations (run to completion) |
| `nico-rest-cert-manager` | Native PKI certificate manager |

## Architecture

| Service | Binary | Description |
|---------|--------|-------------|
| nico-rest-api | `api` | Main REST API server |
| nico-rest-workflow | `workflow` | Temporal workflow service |
| nico-rest-db | `migrations` | Database migrations |
| nico-rest-site-agent | `site-agent` | On-site agent |
| nico-rest-site-manager | `sitemgr` | Site management service |
| nico-rest-cert-manager | `credsmgr` | Native PKI certificate manager |
| nico-cli | `nicocli` | [CLI client](cli/README.md) for the REST API |

Supporting modules:

- **common** - Shared utilities and configurations
- **auth** - Authentication and authorization
- **ipam** - IP Address Management

## OpenAPI Schema Development

OpenAPI schema must be updated whenever the API endpoints are added/updated. Please view instructions at [OpenAPI README](openapi/README.md)

## Pre-commit Hooks

This project uses [pre-commit](https://pre-commit.com/) with [TruffleHog](https://github.com/trufflesecurity/trufflehog) for secret detection to prevent accidentally committing sensitive information like API keys, passwords, or tokens.

### Setup

```bash
# Install pre-commit hooks (first time setup)
make pre-commit-install
```

This will:

1. Install `pre-commit` if not already installed
2. Install `trufflehog` if not already installed
3. Configure git hooks for pre-commit and pre-push

### Usage

Once installed, TruffleHog automatically scans your changes on every `git commit` and `git push`.

To manually run the scan on all files:

```bash
make pre-commit-run
```

Example output:

```text
❯ make pre-commit-run
pre-commit run --all-files
[INFO] Initializing environment for https://github.com/trufflesecurity/trufflehog.
TruffleHog Secret Scan...................................................Passed
```

### Other Commands

```bash
make pre-commit-update  # Update hooks to latest versions
```

## Release Notice

The software is provided "as is" without warranties of any kind. Features,
APIs, and configurations may change in future releases. For production
deployments, please test thoroughly in non-critical environments first.

## License

See [LICENSE](LICENSE) for details.
This project will download and install additional third-party open source software projects. Review the license terms of these open source projects before use.
