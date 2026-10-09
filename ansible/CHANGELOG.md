# Changelog

## v1.0.0 (unreleased)

### Major Changes

- Initial release of the `nvidia.infra_controller` Ansible collection.
- Modules auto-generated from the NICo OpenAPI specification covering
  hardware inventory, provisioning, power control, IP allocation, VPCs,
  networking, BMC/UEFI credentials, SSH keys, and instance lifecycle.

### New Modules

See the [Module Reference](https://docs.nvidia.com/infra-controller/automation/ansible/modules/)
for the full list. All modules follow the same authentication pattern via
`api_url`, `api_token`, and `org` parameters (or their `NVIDIA_NICO_*`
environment variable equivalents).
