# `nico-admin-cli vpc routing-state`

*[Network commands](../../network.md) › [vpc](./vpc.md) › **routing-state***

## NAME

nico-admin-cli-vpc-routing-state - Inspect the VPCs persisted routing
profile and VNI allocations

## SYNOPSIS

```text
nico-admin-cli vpc routing-state [--extended]
[--sort-by] [-h|--help] <ID>
```

## DESCRIPTION

Inspect the VPCs persisted routing profile, observed version, active
VNI, and retained allocation. Each RPC attempt uses the client request
timeout (300 seconds by default, configurable with
FORGE_CLIENT_REQUEST_TIMEOUT_SECS), including connection setup and
response reads. This is not the effective routing profile or evidence of
DPU/fabric convergence; site-global VNI overrides are not reflected
here.

Use --format ascii-table (default), json, or yaml and --output PATH
before vpc. CSV output is unsupported.

## OPTIONS

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

`-h, --help`

Print help (see a summary with -h)

`<ID>`

VPC ID to inspect

## Examples

```sh
nico-admin-cli vpc routing-state 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli --format json --output ./routing-state.json vpc routing-state 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
