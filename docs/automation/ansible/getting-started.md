# Getting Started with the NICo Ansible Collection

The `nvidia.infra_controller` Ansible collection provides modules for managing every NICo resource
from your playbooks — machines, VPCs, allocations, instances, networking, and more.

## Installation

The collection is published to [Ansible Galaxy](https://galaxy.ansible.com/ui/repo/published/nvidia/infra_controller/)
from tagged NICo releases; the collection version always matches the NICo
release version it was built from (see the
[release artifacts](https://github.com/dsx-ai-factory/infra-controller/releases)).
After the first release that includes the collection is published, install it with:

```bash
# --upgrade replaces a source-built 0.0.0 installation with the release.
ansible-galaxy collection install nvidia.infra_controller --upgrade
```

Until that first release is tagged, build and install the collection from a
checkout of this repository (requires Python 3 with PyYAML installed, plus
`ansible-galaxy` from Ansible Core):

```bash
git clone https://github.com/dsx-ai-factory/infra-controller.git
cd infra-controller
# Check out the tag you want: prerelease tags (vX.Y.Z-rc.N, vX.Y.Z-pr) are
# never published to Galaxy, so building from source is the only way to try
# them — and the default branch may not match the prerelease you want.
git checkout <tag>
make ansible
# --force overwrites an earlier build's tarball in ./dist so repeated builds
# of the same 0.0.0 version succeed.
ansible-galaxy collection build ansible --output-path ./dist --force
# Every source build is version 0.0.0; --force replaces an earlier source
# build instead of silently keeping it installed.
ansible-galaxy collection install ./dist/nvidia-infra_controller-*.tar.gz --force
```

## Authentication

Every module accepts the following parameters (or reads them from environment variables):

| Parameter | Environment variable | Description |
|-----------|---------------------|-------------|
| `api_url` | `NVIDIA_NICO_API_URL` | Base URL of the NICo REST API |
| `api_token` | `NVIDIA_NICO_API_TOKEN` | Bearer token for authentication |
| `org` | `NVIDIA_NICO_ORG` | Organization name |
| `api_path_prefix` | `NVIDIA_NICO_API_PATH_PREFIX` | API path prefix used in REST API routes (default `nico`; set it when a proxy serves the API under a different prefix) |

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
        # A stable, user-specified UUID: if the create times out after the
        # server dispatched it, this ID still identifies the VPC for the
        # recovery steps below and a retry cannot create a second one.
        id: "{{ vpc_id }}"
        name: my-vpc
        site_id: "{{ site_id }}"
      register: vpc

    - name: Show VPC ID
      ansible.builtin.debug:
        msg: "VPC ID: {{ vpc.resource.id }}"
      # In check mode the VPC is not created and resource is empty.
      when: vpc.resource.id is defined
```

A retried create can fail with HTTP 409 (conflict): the VPC already exists by
name, but is not yet visible to lookups while inventory reconciliation
completes. If a run fails ambiguously (for example, on a timeout), wait for
reconciliation and look the VPC up by the stable `id` you supplied — for
example with `vpc_info` — before rerunning the task, rather than retrying
immediately.

## Module reference

See the [Module Reference](modules/index.md) for the full list of available modules. Each module page documents
its parameters and usage examples.
