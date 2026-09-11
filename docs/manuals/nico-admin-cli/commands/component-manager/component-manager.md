# `nico-admin-cli component-manager`

_[Hardware commands](../../hardware.md) › **component-manager**_

## NAME

nico-admin-cli-component-manager - Component manager actions

## SYNOPSIS

**nico-admin-cli component-manager** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Component manager actions

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
| [`update-firmware`](./component-manager-update-firmware.md) | Queue component firmware updates |
| [`get-firmware-update-status`](./component-manager-get-firmware-update-status.md) | Get component firmware update status |
| [`get-firmware-versions`](./component-manager-get-firmware-versions.md) | List available component firmware versions |
| [`component-power-control`](./component-manager-component-power-control.md) | Issue a power-control action against components (switches, power shelves, compute trays) |
| [`configure-switch-certificate`](./component-manager-configure-switch-certificate.md) | Rotate or reinstall switch NVOS mTLS certificates via the switch Maintenance phase |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
