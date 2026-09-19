# NVIDIA Infra Controller

> **Repository move notice:** On September 4, 2026, the NICo repository moved
> from the NVIDIA GitHub organization to
> [`dsx-ai-factory/infra-controller`](https://github.com/dsx-ai-factory/infra-controller).
> Existing repository URLs and standard Git operations continue to work through
> GitHub redirects. If you maintain automation or integrations that reference
> `NVIDIA/infra-controller`, such as GitHub Actions, webhooks, or pinned
> repository URLs, update them to `dsx-ai-factory/infra-controller`.

NVIDIA Infra Controller (NICo) delivers zero-touch lifecycle automation for
bare-metal systems that secures datacenter infrastructure at its foundation.

NICo is an open-source infrastructure component in DSX OS. It provides
site-local, zero-trust, bare-metal lifecycle management with DPU-enforced
isolation. NICo automates the complexity of the bare-metal lifecycle to
fast-track building next-generation AI cloud offerings.

## Project Resources

- [Documentation](https://docs.nvidia.com/infra-controller/documentation/home)
  and the [Quick Start Guide](https://docs.nvidia.com/infra-controller/documentation/getting-started/quick-start-guide)
- [Contribution guidelines](CONTRIBUTING.md) for development setup and pull
  request requirements
- [GitHub Discussions](https://github.com/dsx-ai-factory/infra-controller/discussions)
  for questions and community conversations
- [Governance](GOVERNANCE.md) and [maintainer roles](MAINTAINERS.md) for project
  decision-making and ownership
- [Code of Conduct](CODE_OF_CONDUCT.md) for participation expectations and
  [conduct reporting](mailto:GitHub_Conduct@nvidia.com)
- [Security policy](SECURITY.md) for the approved private vulnerability
  reporting paths
- [Support policy](SUPPORT.md) for community, security, and commercial support
  paths and the supported-release lifecycle
- [Release notes](https://docs.nvidia.com/infra-controller/documentation/release-notes)
  for current, maintenance, and end-of-life release status

## Getting Started

- Go to the
  [NVIDIA Infra Controller overview](docs/overview/what-is-nico.md) for an
  overview of NICo architecture and capabilities.
- Go straight to the
  [Quick Start Guide](https://docs.nvidia.com/infra-controller/documentation/getting-started/quick-start-guide)
  to start setting up your site for NICo.
- The
  [NICo web documentation](https://docs.nvidia.com/infra-controller/documentation/home)
  is available online.
- Use
  [Local Development with DevSpace](dev/deployment/devspace/README.md) to run
  NICo locally with mock systems.

## Bare-Metal Cluster Setup

`helm-prereqs/setup.sh` deploys the full NVIDIA Infra Controller stack onto a
bare-metal Kubernetes cluster in three layers:

| Layer | What it installs | Helm release |
|-------|-----------------|--------------|
| **Common services** | MetalLB, cert-manager, Vault, external-secrets, PostgreSQL | via `helmfile` in `helm-prereqs/` |
| **NICo Core** | NVIDIA Infra Controller (this repo's `helm/` chart) | `nico` in `nico-system` |
| **NICo REST** | NVIDIA Infra Controller's REST API, Temporal, Keycloak, site-agent | `nico-rest` + `nico-rest-site-agent` in `nico-rest` |

### Prerequisites

- A running Kubernetes cluster with `KUBECONFIG` set
- `helm`, `helmfile`, `kubectl`, `jq` installed
- Images pushed to your container registry

### Quick start

```bash
# 1. Build and push amd64/arm64 container images from this clone.
#    See docs/manuals/building_nico_containers.md for the build-host prerequisites.
export IMAGE_REGISTRY=my-registry.example.com/infra-controller
# Build NICo Core (nico) and REST service images.
make images IMAGE_REGISTRY="${IMAGE_REGISTRY}"

# 2. Set environment variables
export KUBECONFIG=/path/to/kubeconfig
export NICO_IMAGE_REGISTRY="${IMAGE_REGISTRY}"
export NICO_CORE_IMAGE_TAG=NICO_CORE_TAG             # e.g. 2.0.0-pr-58-g38a54a3f
export NICO_REST_IMAGE_TAG=NICO_REST_TAG             # e.g. 2.0.0-pr-58-g38a54a3f
# Optional raw key for authenticated registries:
# export REGISTRY_PULL_SECRET=RAW_API_KEY

# DPF (DOCA Platform Framework) DPU provisioning installs BY DEFAULT.
# Set these three variables, or pass --skip-dpf to opt out:
# Controller NIC for the DPU cluster VIP:
export NICO_DPF_DPU_INTERFACE=<nic-facing-dpus>
# Floating IP the DPUs use to reach their control plane:
export NICO_DPF_DPU_CLUSTER_VIP=<free-routable-ip>
export NICO_DPF_BMC_ROOT_PASSWORD=<bmc-password>    # site-wide BMC root password
# Refer to helm-prereqs/README.md §DPF for full variable reference.

# 3. Customize site-specific values
#    Edit helm-prereqs/values/nico-core.yaml:
#      nico-api.hostname      — your site's external API hostname
#      nico-api.siteConfig    — network pools, VLAN ranges, IB config, MetalLB VIPs
#    Edit helm-prereqs/values/metallb-config.yaml:
#      IPAddressPool, BGPPeer    — your site's VIP ranges and TOR switch config
#    Edit helm-prereqs/values.yaml:
#      siteName                  — short site identifier

# 4. Run setup — installs common services, NICo Core, and NICo REST in order
cd helm-prereqs
./setup.sh                # interactive: prompts before deploying Core and REST
./setup.sh -y             # non-interactive: deploys everything including DPF (CI/CD)
./setup.sh -y --skip-dpf  # non-interactive: skip DPF (no DPUs, or still on iPXE)
```

To tear everything down:

```bash
cd helm-prereqs
./clean.sh
```

See [helm-prereqs/README.md](helm-prereqs/README.md) for the full reference,
including PKI architecture, PostgreSQL setup, phase-by-phase descriptions,
secrets, and troubleshooting.

## Contributing

See the [contribution guide](CONTRIBUTING.md) for instructions on setting
up a development environment and submitting changes, and the
[code of conduct](CODE_OF_CONDUCT.md) for contributor expectations.

## Release Notice

The software is provided "as is" without warranties of any kind. Features,
APIs, and configurations may change in future releases. For production
deployments, please test thoroughly in non-critical environments first.
