# `nico-admin-cli switch metadata set`

_[Hardware commands](../../hardware.md) › [switch](./switch.md) › [metadata](./switch-metadata.md) › **set**_

## NAME

nico-admin-cli-switch-metadata-set - Set the Name or Description of the
Switch

## SYNOPSIS

**nico-admin-cli switch metadata set** \[**--name**\]
\[**--description**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*SWITCH*\>

## DESCRIPTION

Set the Name or Description of the Switch

## OPTIONS

**--name** *\<NAME\>*  
The updated name of the Switch

**--description** *\<DESCRIPTION\>*  
The updated description of the Switch

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

\<*SWITCH*\>  
The switch which should get updated metadata

## Examples

```sh
nico-admin-cli switch metadata set 12345678-1234-5678-90ab-cdef01234567 --name spine-01 --description "Rack 4 spine"
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
