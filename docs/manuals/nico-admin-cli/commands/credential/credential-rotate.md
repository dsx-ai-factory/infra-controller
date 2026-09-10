# `nico-admin-cli credential rotate`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › **rotate**_

## NAME

nico-admin-cli-credential-rotate - Stage a site-wide credential rotation
(auto-generate or explicit password)

## SYNOPSIS

**nico-admin-cli credential rotate** \<**--type**\> \[**--password**\]
\[**--reason**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Stage a site-wide credential rotation (auto-generate or explicit
password)

## OPTIONS

**--type**=*\<CREDENTIAL_TYPE\>*  
Credential family to rotate\

\
*Possible values:*

- bmc

- host-uefi

- dpu-uefi

- nvos

- lockdown-ikm

- dpu-bmc-service

**--password**=*\<PASSWORD\>*  
Explicit rotate-to password. Omit to have the server auto-generate a
strong one.

**--reason** *\<REASON\>*  
Free-form note recorded with the rotation (must not contain secrets)

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
nico-admin-cli credential rotate --type=bmc
nico-admin-cli credential rotate --type=host-uefi --password=Str0ng-Explicit-Pw!
nico-admin-cli credential rotate --type=nvos --password=MyNvosPassword-2026
nico-admin-cli credential rotate --type=lockdown-ikm --reason="quarterly rotation"
nico-admin-cli credential rotate --type=dpu-bmc-service
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
