# `nico-admin-cli inventory`

*[Hardware commands](../../hardware.md) › **inventory***

## NAME

nico-admin-cli-inventory - Generate Ansible Inventory

## SYNOPSIS

**nico-admin-cli inventory** \[**-f**\|**--filename**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Generate Ansible Inventory

## OPTIONS

**-f**, **--filename** *\<FILENAME\>*  
Write to file

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
nico-admin-cli inventory
nico-admin-cli inventory --filename ./inventory.ini
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
