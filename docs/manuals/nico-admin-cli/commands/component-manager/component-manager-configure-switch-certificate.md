# `nico-admin-cli component-manager configure-switch-certificate`

*[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › **configure-switch-certificate***

## NAME

nico-admin-cli-component-manager-configure-switch-certificate - Rotate
or reinstall switch NVOS mTLS certificates via the switch Maintenance
phase

## SYNOPSIS

**nico-admin-cli component-manager configure-switch-certificate**
\<**--switch-id**\> \[**--domain-name**\]
\[**--bypass-state-controller**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Rotate or reinstall switch NVOS mTLS certificates via the switch
Maintenance phase

## OPTIONS

**--switch-id** *\<SWITCH_IDS\>...*  
Switch IDs to target

**--domain-name** *\<DOMAIN_NAME\>*  
Optional certificate domain passed through to RMS; omit to use the RMS
default

**--bypass-state-controller**  
Bypass the switch state controller and dispatch directly to the
component backend

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
nico-admin-cli component-manager configure-switch-certificate --switch-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli component-manager configure-switch-certificate --switch-id 12345678-1234-5678-90ab-cdef01234567,abcdef01-2345-6789-abcd-ef0123456789
nico-admin-cli component-manager configure-switch-certificate --switch-id 12345678-1234-5678-90ab-cdef01234567 --bypass-state-controller
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
