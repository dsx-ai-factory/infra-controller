# `nico-admin-cli redfish dpu firmware`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › [dpu](./redfish-dpu.md) › **firmware***

## NAME

nico-admin-cli-redfish-dpu-firmware - BMCs FW Commands

## SYNOPSIS

**nico-admin-cli redfish dpu firmware** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

BMCs FW Commands

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword dpu firmware status
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword dpu firmware update --package ./bmc-fw.fwpkg
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword dpu firmware show --all
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`status`](./redfish-dpu-firmware-status.md) | Print FW update status |
| [`update`](./redfish-dpu-firmware-update.md) | Update BMC's FW to the given FW package |
| [`show`](./redfish-dpu-firmware-show.md) | Show FW versions of different components |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
