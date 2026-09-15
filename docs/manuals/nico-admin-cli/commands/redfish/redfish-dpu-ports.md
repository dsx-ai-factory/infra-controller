# `nico-admin-cli redfish dpu ports`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › [dpu](./redfish-dpu.md) › **ports**_

## NAME

nico-admin-cli-redfish-dpu-ports - Show ports information

## SYNOPSIS

**nico-admin-cli redfish dpu ports** \[**-a**\|**--all**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \[*PORT*\]

## DESCRIPTION

Show ports information

## OPTIONS

**-a**, **--all**  
Show all ports (DEPRECATED)

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

\[*PORT*\] \[default: \]  
The port to query (e.g. eth0, eth1), leave empty for all (default)

## Examples

```sh
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword dpu ports
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword dpu ports eth0
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
