# `nico-admin-cli machine metadata set`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › [metadata](./machine-metadata.md) › **set***

## NAME

nico-admin-cli-machine-metadata-set - Set the Name or Description of the
Machine

## SYNOPSIS

**nico-admin-cli machine metadata set** \[**--name**\]
\[**--description**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*MACHINE*\>

## DESCRIPTION

Set the Name or Description of the Machine

## OPTIONS

**--name** *\<NAME\>*  
The updated name of the Machine

**--description** *\<DESCRIPTION\>*  
The updated description of the Machine

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

\<*MACHINE*\>  
The machine which should get updated metadata

## Examples

```sh
nico-admin-cli machine metadata set 12345678-1234-5678-90ab-cdef01234567 --name gpu-node-01 --description "Rack 4, tray 2"
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
