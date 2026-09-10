# `nico-admin-cli machine nvlink-info populate`

_[Hardware commands](../../hardware.md) › [machine](./machine.md) › [nvlink-info](./machine-nvlink-info.md) › **populate**_

## NAME

nico-admin-cli-machine-nvlink-info-populate - Build NVLink info from
Redfish + NMX-C and populate DB

## SYNOPSIS

**nico-admin-cli machine nvlink-info populate** \[**--update-db**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*MACHINE_ID*\>

## DESCRIPTION

Build NVLink info from Redfish + NMX-C and populate DB

## OPTIONS

**--update-db**  
Update the database with the nvlink_info

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

\<*MACHINE_ID*\>  
Machine ID to populate

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
