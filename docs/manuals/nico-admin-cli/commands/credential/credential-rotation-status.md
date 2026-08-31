# `nico-admin-cli credential rotation-status`

*[Hardware commands](../../hardware.md) › [credential](./credential.md) › **rotation-status***

## NAME

nico-admin-cli-credential-rotation-status - Show convergence status of a
site-wide credential rotation

## SYNOPSIS

**nico-admin-cli credential rotation-status** \<**--type**\>
\[**--mac-address**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Show convergence status of a site-wide credential rotation

## OPTIONS

**--type**=*\<CREDENTIAL_TYPE\>*  
Credential family to report on\

\
*Possible values:*

- bmc

- host-uefi

- dpu-uefi

- nvos

- lockdown-ikm

- dpu-bmc-service

**--mac-address** *\<MAC_ADDRESS\>*  
Report on a single device by MAC instead of the whole site

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
nico-admin-cli credential rotation-status --type=bmc
nico-admin-cli credential rotation-status --type=lockdown-ikm
nico-admin-cli credential rotation-status --type=bmc --mac-address 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
