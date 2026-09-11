# `nico-admin-cli instance release`

_[Tenant commands](../../tenant.md) › [instance](./instance.md) › **release**_

## NAME

nico-admin-cli-instance-release - De-allocate instance

## SYNOPSIS

**nico-admin-cli instance release** \[**-i**\|**--instance**\]
\[**-m**\|**--machine**\] \[**--label-key**\] \[**--label-value**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

De-allocate instance

## OPTIONS

**-i**, **--instance** *\<INSTANCE\>*  
**-m**, **--machine** *\<MACHINE\>*  
**--label-key** *\<LABEL_KEY\>*  
The key of label instance to query

**--label-value** *\<LABEL_VALUE\>*  
The value of label instance to query

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
nico-admin-cli instance release --instance 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli instance release --machine 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli instance release --label-key role --label-value training
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
