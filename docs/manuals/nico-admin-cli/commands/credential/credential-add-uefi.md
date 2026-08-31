# `nico-admin-cli credential add-uefi`

*[Hardware commands](../../hardware.md) › [credential](./credential.md) › **add-uefi***

## NAME

nico-admin-cli-credential-add-uefi - Add site-wide DPU UEFI default
credential (NOTE: this parameter can be set only once)

## SYNOPSIS

**nico-admin-cli credential add-uefi** \<**--kind**\> \<**--password**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Add site-wide DPU UEFI default credential (NOTE: this parameter can be
set only once)

## OPTIONS

**--kind**=*\<KIND\>*  
The UEFI kind\

\
*Possible values:*

- dpu

- host

**--password**=*\<PASSWORD\>*  
The UEFI password

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
nico-admin-cli credential add-uefi --kind=dpu --password=mynewpassword
nico-admin-cli credential add-uefi --kind=host --password=mynewpassword
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
