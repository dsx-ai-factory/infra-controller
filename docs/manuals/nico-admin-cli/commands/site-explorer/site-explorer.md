# `nico-admin-cli site-explorer`

*[Tenant commands](../../tenant.md) › **site-explorer***

## NAME

nico-admin-cli-site-explorer - Site explorer functions

## SYNOPSIS

**nico-admin-cli site-explorer** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Site explorer functions

## OPTIONS

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field\

\
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

## Subcommands

| Subcommand | Description |
|---|---|
| [`get-report`](./site-explorer-get-report.md) | Retrieves the latest site exploration report |
| [`mlx-devices`](./site-explorer-mlx-devices.md) | Report Mellanox/BlueField device NIC firmware from explored Redfish data. |
| [`explore`](./site-explorer-explore.md) | Asks carbide-api to explore a single host and prints the report. Does not store it. |
| [`re-explore`](./site-explorer-re-explore.md) | Asks carbide-api to explore a single host in the next exploration cycle. The results will be stored. |
| [`refresh`](./site-explorer-refresh.md) | Immediately probes a BMC endpoint and persists the report. |
| [`clear-error`](./site-explorer-clear-error.md) | Clear the last known error for the BMC in the latest site exploration report. |
| [`delete`](./site-explorer-delete.md) | Delete an explored endpoint from the database. |
| [`remediation`](./site-explorer-remediation.md) | Control remediation actions for an explored endpoint. |
| [`is-bmc-in-managed-host`](./site-explorer-is-bmc-in-managed-host.md) |  |
| [`have-credentials`](./site-explorer-have-credentials.md) |  |
| [`copy-bfb-to-dpu-rshim`](./site-explorer-copy-bfb-to-dpu-rshim.md) |  |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
