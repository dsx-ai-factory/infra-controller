# `nico-admin-cli credential registry set`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › [registry](./credential-registry.md) › **set**_

## NAME

nico-admin-cli-credential-registry-set - Set credentials for a container
registry

## SYNOPSIS

**nico-admin-cli credential registry set** \<**--registry**\>
\<**--username**\> \<**--password-stdin**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Set credentials for a container registry

## OPTIONS

**--registry** *\<REGISTRY\>*  
Registry hostname (e.g. nvcr.io)

**--username** *\<USERNAME\>*  
Registry username

**--password-stdin**  
Read the registry password or API key from standard input; it is never
accepted in command arguments

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
read -r -s -p 'Registry token: ' registry_token; printf '\n'
printf '%s' "$registry_token" | nico-admin-cli credential registry set --registry nvcr.io --username '$oauthtoken' --password-stdin
unset registry_token
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
