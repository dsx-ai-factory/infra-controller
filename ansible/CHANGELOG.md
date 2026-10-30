# Changelog

## Unreleased

### Major Changes

- Initial release of the `nvidia.infra_controller` Ansible collection.
- Modules auto-generated from the NICo OpenAPI specification covering
  hardware inventory, provisioning, power control, IP allocation, VPCs,
  networking, BMC/UEFI credentials, SSH keys, and instance lifecycle.

### New Modules

See the [Module Reference](https://docs.nvidia.com/infra-controller/documentation/automation/ansible-collection/module-reference/module-index)
for the full list. All modules follow the same authentication pattern using
`api_url`, `api_token`, `org`, and `api_path_prefix` parameters (or their
`NVIDIA_NICO_*` environment variable equivalents).
