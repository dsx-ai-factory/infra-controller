# `nico-admin-cli redfish dpu`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **dpu**_

## NAME

nico-admin-cli-redfish-dpu - DPU specific operations

## SYNOPSIS

**nico-admin-cli redfish dpu** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

DPU specific operations

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword dpu firmware show --all
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword dpu ports
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`firmware`](./redfish-dpu-firmware.md) | BMC's FW Commands |
| [`ports`](./redfish-dpu-ports.md) | Show ports information |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
