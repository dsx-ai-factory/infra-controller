# `nico-admin-cli dpu`

_[Hardware commands](../../hardware.md) › **dpu**_

## NAME

nico-admin-cli-dpu - DPU specific handling

## SYNOPSIS

**nico-admin-cli dpu** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

DPU specific handling

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
| [`reprovision`](./dpu-reprovision.md) | DPU Reprovisioning handling |
| [`agent-upgrade-policy`](./dpu-agent-upgrade-policy.md) | Get or set forge-dpu-agent upgrade policy |
| [`versions`](./dpu-versions.md) | View DPU firmware status |
| [`status`](./dpu-status.md) | View DPU Status |
| [`network`](./dpu-network.md) | Networking information |
| [`health-report`](./dpu-health-report.md) | Manage DPU health report sources |
| [`set-uefi-password`](./dpu-set-uefi-password.md) | Set DPU UEFI password directly on the device (via Redfish) |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
