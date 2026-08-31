# `nico-admin-cli redfish delete-bmc-user`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **delete-bmc-user***

## NAME

nico-admin-cli-redfish-delete-bmc-user - Create new BMC user

## SYNOPSIS

**nico-admin-cli redfish delete-bmc-user** \<**--user**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Create new BMC user

## OPTIONS

**--user** *\<USER\>*  
BMC user

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword delete-bmc-user --user svc-ops
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
