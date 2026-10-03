# nvidia.infra_controller Ansible Collection

This collection provides Ansible modules for managing NVIDIA Infra Controller (NICo)
resources — hardware inventory, provisioning, power control, IP allocation, and more.

## Requirements

- Ansible Core 2.14+
- Python 3.9+

## Installation

```bash
ansible-galaxy collection install nvidia.infra_controller
```

## Authentication

Each module accepts `api_url`, `api_token`, and `org` parameters, or reads them from
`NVIDIA_NICO_API_URL`, `NVIDIA_NICO_API_TOKEN`, and `NVIDIA_NICO_ORG` environment variables.

## Documentation

See the [NICo documentation](https://docs.nvidia.com/infra-controller/) for full module
reference and getting-started guides.
