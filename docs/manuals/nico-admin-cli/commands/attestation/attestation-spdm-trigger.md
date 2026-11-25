# `nico-admin-cli attestation spdm trigger`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › **trigger***

## NAME

nico-admin-cli-attestation-spdm-trigger - Trigger attestation for a
given machine with id

## SYNOPSIS

```text
nico-admin-cli attestation spdm trigger
<--redfish-timeout-secs> [--attester-id-regex]
[--sort-by] [-h|--help] <MACHINE_ID>
```

## DESCRIPTION

Trigger attestation for a given machine with id

## OPTIONS

`--redfish-timeout-secs <REDFISH_TIMEOUT_SECS>`

Timeout in seconds for Redfish operations while triggering attestation
(passed to carbide-api)

`--attester-id-regex <REGEX>`

Override profile selection, including mode NONE or a missing profile,
for this request only. Uses Rust regex syntax; matching is
case-sensitive by default and searches within each reported attester ID
unless anchored with ^ and \$. Device eligibility checks and site-wide
SPDM enablement still apply. Empty or invalid patterns are rejected. If
no eligible attester matches, no job is created. Omit this option to use
the profile for this machine.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with '-h')

`<MACHINE_ID>`

Machine ID

## Examples

```sh
nico-admin-cli attestation spdm trigger fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg --redfish-timeout-secs 60
nico-admin-cli attestation spdm trigger fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg --redfish-timeout-secs 60 --attester-id-regex 'GPU_'
nico-admin-cli attestation spdm trigger fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg --redfish-timeout-secs 60 --attester-id-regex '^HGX_IRoT_GPU_0$'
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
