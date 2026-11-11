# `nico-admin-cli attestation spdm trigger`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › **trigger***

## NAME

nico-admin-cli-attestation-spdm-trigger - Trigger attestation for a
given machine with id

## SYNOPSIS

```text
nico-admin-cli attestation spdm trigger
<--redfish-timeout-secs> [--sort-by] [-h|--help]
<MACHINE_ID>
```

## DESCRIPTION

Trigger attestation for a given machine with id

## OPTIONS

`--redfish-timeout-secs <REDFISH_TIMEOUT_SECS>`

Timeout in seconds for Redfish operations while triggering attestation
(passed to carbide-api)

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with -h)

`<MACHINE_ID>`

Machine ID

## Examples

```sh
nico-admin-cli attestation spdm trigger 12345678-1234-5678-90ab-cdef01234567 --redfish-timeout-secs 60
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
