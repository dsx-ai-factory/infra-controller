# `nico-admin-cli component-manager get-firmware-update-status`

_[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › **get-firmware-update-status**_

## NAME

nico-admin-cli-component-manager-get-firmware-update-status - Get
component firmware update status

## SYNOPSIS

**nico-admin-cli component-manager get-firmware-update-status**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*subcommands*\>

## DESCRIPTION

Get component firmware update status

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

## Examples

```sh
nico-admin-cli component-manager get-firmware-update-status switch --switch-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli component-manager get-firmware-update-status compute-tray --machine-id 12345678-1234-5678-90ab-cdef01234567,abcdef01-2345-6789-abcd-ef0123456789
nico-admin-cli component-manager get-firmware-update-status compute-tray --mac-address 00:11:22:33:44:55
nico-admin-cli component-manager get-firmware-update-status rack --rack-id 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`switch`](./component-manager-get-firmware-update-status-switch.md) | Target NVLink switches |
| [`power-shelf`](./component-manager-get-firmware-update-status-power-shelf.md) | Target power shelves |
| [`compute-tray`](./component-manager-get-firmware-update-status-compute-tray.md) | Target compute trays |
| [`rack`](./component-manager-get-firmware-update-status-rack.md) | Target racks |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
