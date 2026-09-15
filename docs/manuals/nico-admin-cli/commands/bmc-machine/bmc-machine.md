# `nico-admin-cli bmc-machine`

_[Hardware commands](../../hardware.md) › **bmc-machine**_

## NAME

nico-admin-cli-bmc-machine - BMC Machine related handling

## SYNOPSIS

**nico-admin-cli bmc-machine** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

BMC Machine related handling

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
| [`bmc-reset`](./bmc-machine-bmc-reset.md) | Reset BMC |
| [`admin-power-control`](./bmc-machine-admin-power-control.md) | Redfish Power Control |
| [`create-bmc-user`](./bmc-machine-create-bmc-user.md) |  |
| [`delete-bmc-user`](./bmc-machine-delete-bmc-user.md) |  |
| [`enable-infinite-boot`](./bmc-machine-enable-infinite-boot.md) | Enable infinite boot |
| [`is-infinite-boot-enabled`](./bmc-machine-is-infinite-boot-enabled.md) | Check if infinite boot is enabled |
| [`lockdown`](./bmc-machine-lockdown.md) | Enable or disable lockdown |
| [`lockdown-status`](./bmc-machine-lockdown-status.md) | Check lockdown status |
| [`set-root-password`](./bmc-machine-set-root-password.md) | Set a BMC's root password out-of-band (for fleet rotation use `credential rotate`) |
| [`probe-vendor`](./bmc-machine-probe-vendor.md) | Resolve a BMC's Redfish vendor |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
