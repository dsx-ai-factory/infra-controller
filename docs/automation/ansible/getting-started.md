# Getting Started with the NICo Ansible Collection

The `nvidia.infra_controller` Ansible collection provides modules for managing every NICo resource
from your playbooks — machines, VPCs, allocations, instances, networking, and more.

## Installation

```bash
ansible-galaxy collection install nvidia.infra_controller
```

Or install from a specific release tarball attached to the [GitHub release page](https://github.com/dsx-ai-factory/infra-controller/releases):

```bash
ansible-galaxy collection install nvidia-infra_controller-<version>.tar.gz
```

## Authentication

Every module accepts the following parameters (or reads them from environment variables):

| Parameter | Environment variable | Description |
|-----------|---------------------|-------------|
| `api_url` | `NVIDIA_NICO_API_URL` | Base URL of the NICo REST API |
| `api_token` | `NVIDIA_NICO_API_TOKEN` | Bearer token for authentication |
| `org` | `NVIDIA_NICO_ORG` | Organization name |

Store credentials in an Ansible Vault-encrypted variables file rather than in plaintext.

## Example playbook

```yaml
- name: Provision a NICo VPC
  hosts: localhost
  gather_facts: false
  vars_files:
    - vault_nico_creds.yml

  tasks:
    - name: Create VPC
      nvidia.infra_controller.vpc:
        api_url: "{{ nico_api_url }}"
        api_token: "{{ nico_api_token }}"
        org: "{{ nico_org }}"
        state: present
        name: my-vpc
        site_id: "{{ site_id }}"
      register: vpc

    - name: Show VPC ID
      ansible.builtin.debug:
        msg: "VPC ID: {{ vpc.resource.id }}"
```

## Module reference

See the [Module Reference](modules/) for the full list of available modules. Each module page documents
its parameters and usage examples.
