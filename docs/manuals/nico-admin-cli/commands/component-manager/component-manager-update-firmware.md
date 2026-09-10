# `nico-admin-cli component-manager update-firmware`

_[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › **update-firmware**_

## NAME

nico-admin-cli-component-manager-update-firmware - Queue component
firmware updates

## SYNOPSIS

**nico-admin-cli component-manager update-firmware** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Queue component firmware updates

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
nico-admin-cli component-manager update-firmware switch --switch-id 12345678-1234-5678-90ab-cdef01234567 --target-version fw-1.2.3
nico-admin-cli component-manager update-firmware switch --switch-id 12345678-1234-5678-90ab-cdef01234567 --component bmc,bios --force-update --target-version fw-1.2.3
nico-admin-cli component-manager update-firmware compute-tray --machine-id 12345678-1234-5678-90ab-cdef01234567 --sot-json-file ./sot.json --access-token mytoken
nico-admin-cli component-manager update-firmware compute-tray --mac-address 00:11:22:33:44:55 --sot-json-file ./sot.json
nico-admin-cli component-manager update-firmware power-shelf --power-shelf-id 12345678-1234-5678-90ab-cdef01234567 --target-version fw-1.2.3
nico-admin-cli component-manager update-firmware rack --rack-id 12345678-1234-5678-90ab-cdef01234567 --target-version fw-1.2.3
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`switch`](./component-manager-update-firmware-switch.md) | Queue firmware on NVLink switches |
| [`power-shelf`](./component-manager-update-firmware-power-shelf.md) | Queue firmware on power shelves |
| [`compute-tray`](./component-manager-update-firmware-compute-tray.md) | Queue firmware on compute trays |
| [`rack`](./component-manager-update-firmware-rack.md) | Queue firmware on all eligible devices in racks |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
