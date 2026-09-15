# `nico-admin-cli redfish get-task`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **get-task**_

## NAME

nico-admin-cli-redfish-get-task - Get detailed info on a Redfish task

## SYNOPSIS

**nico-admin-cli redfish get-task** \<**--taskid**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Get detailed info on a Redfish task

## OPTIONS

**--taskid** *\<TASKID\>*  
Task ID

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword get-task --taskid JID_123456789012
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
