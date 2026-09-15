# `nico-admin-cli credential firmware-access-token set`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › [firmware-access-token](./credential-firmware-access-token.md) › **set**_

## NAME

nico-admin-cli-credential-firmware-access-token-set - Set or rotate a
firmware artifact access token

## SYNOPSIS

**nico-admin-cli credential firmware-access-token set** \<**--name**\>
\<**--token-file**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Set or rotate a firmware artifact access token

## OPTIONS

**--name** *\<NAME\>*  
Non-secret credential name

**--token-file** *\<TOKEN_FILE\>*  
File containing the token, or - to read standard input

**--extended**  
Extended result output.

This used by measured boot, where basic output contains just what you
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
nico-admin-cli credential firmware-access-token set --name repository-a --token-file ./firmware-token.txt
cat ./firmware-token.txt | nico-admin-cli credential firmware-access-token set --name repository-a --token-file -
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
