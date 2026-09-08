# `nico-admin-cli credential registry set`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › [registry](./credential-registry.md) › **set**_

## NAME

nico-admin-cli-credential-registry-set - Set credentials for a container
registry

## SYNOPSIS

**nico-admin-cli credential registry set** \<**--registry**\>
\<**--username**\> \<**--password**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Set credentials for a container registry

## OPTIONS

**--registry** *\<REGISTRY\>*  
Registry hostname (e.g. nvcr.io)

**--username** *\<USERNAME\>*  
Registry username

**--password** *\<PASSWORD\>*  
Registry password or API key

**--extended**  
Extended result output.

This used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field  

  
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli credential registry set --registry nvcr.io --username '$oauthtoken' --password mypassword
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
