# `nico-admin-cli switch metadata add-label`

*[Hardware commands](../../hardware.md) › [switch](./switch.md) › [metadata](./switch-metadata.md) › **add-label***

## NAME

nico-admin-cli-switch-metadata-add-label - Adds a label to the Metadata
of a Switch

## SYNOPSIS

**nico-admin-cli switch metadata add-label** \<**--key**\>
\[**--value**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*SWITCH*\>

## DESCRIPTION

Adds a label to the Metadata of a Switch

## OPTIONS

**--key** *\<KEY\>*  
The key to add

**--value** *\<VALUE\>*  
The optional value to add

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
nico-admin-cli switch metadata add-label 12345678-1234-5678-90ab-cdef01234567 --key edge
nico-admin-cli switch metadata add-label 12345678-1234-5678-90ab-cdef01234567 --key env --value prod
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
