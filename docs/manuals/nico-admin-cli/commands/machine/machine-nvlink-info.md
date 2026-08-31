# `nico-admin-cli machine nvlink-info`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › **nvlink-info***

## NAME

nico-admin-cli-machine-nvlink-info - Update/show NVLink info for an
MNNVL machine

## SYNOPSIS

**nico-admin-cli machine nvlink-info** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Update/show NVLink info for an MNNVL machine

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
nico-admin-cli machine nvlink-info show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli machine nvlink-info populate 12345678-1234-5678-90ab-cdef01234567 --update-db
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./machine-nvlink-info-show.md) | Show existing NVLink info |
| [`populate`](./machine-nvlink-info-populate.md) | Build NVLink info from Redfish + NMX-C and populate DB |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
