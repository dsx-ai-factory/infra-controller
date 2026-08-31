# `nico-admin-cli credential add-host-factory-default`

*[Hardware commands](../../hardware.md) › [credential](./credential.md) › **add-host-factory-default***

## NAME

nico-admin-cli-credential-add-host-factory-default - Add manufacturer
factory default BMC user/pass for a given vendor

## SYNOPSIS

**nico-admin-cli credential add-host-factory-default**
\<**--username**\> \<**--password**\> \<**--vendor**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Add manufacturer factory default BMC user/pass for a given vendor

## OPTIONS

**--username** *\<USERNAME\>*  
Default username: root, ADMIN, etc

**--password** *\<PASSWORD\>*  
Manufacturer default password

**--vendor** *\<VENDOR\>*  
\
*Possible values:*

- lenovo

- lenovo-ami

- dell

- supermicro

- hpe

- nvidia

- liteon

- delta

- unknown

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
nico-admin-cli credential add-host-factory-default --vendor nvidia --username admin --password mypassword
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
