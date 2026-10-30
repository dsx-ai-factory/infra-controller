# nvidia.infra_controller Ansible Collection

This collection provides Ansible modules for managing NVIDIA Infra Controller (NICo)
resources — hardware inventory, provisioning, power control, IP allocation, and more.

## Requirements

- Ansible Core 2.14+
- Python 3.9+

## Installation

Install the published collection from Ansible Galaxy:

```bash
ansible-galaxy collection install nvidia.infra_controller
```

See the [collection getting-started guide](https://docs.nvidia.com/infra-controller/documentation/automation/ansible-collection/getting-started)
for the pre-release installation path and authentication details.

## Authentication

Each module accepts `api_url`, `api_token`, `org`, and `api_path_prefix` parameters, or reads
them from the `NVIDIA_NICO_API_URL`, `NVIDIA_NICO_API_TOKEN`, `NVIDIA_NICO_ORG`, and
`NVIDIA_NICO_API_PATH_PREFIX` environment variables. `api_path_prefix` defaults to `nico`;
set it when a proxy serves the REST API under a different prefix.

## Documentation

See the [NICo documentation](https://docs.nvidia.com/infra-controller/) for full module
reference and getting-started guides.
