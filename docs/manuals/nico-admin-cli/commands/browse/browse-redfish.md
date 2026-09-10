# `nico-admin-cli browse redfish`

_[Hardware commands](../../hardware.md) › [browse](./browse.md) › **redfish**_

## NAME

nico-admin-cli-browse-redfish - Browse a Redfish resource tree via the
API server

## SYNOPSIS

**nico-admin-cli browse redfish** \<**--uri**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Browse a Redfish resource tree via the API server

## OPTIONS

**--uri** *\<URI\>*  
Redfish URI

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
nico-admin-cli browse redfish --uri /redfish/v1/Systems
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
