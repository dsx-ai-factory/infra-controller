# `nico-admin-cli secrets re-wrap`

_[Admin commands](../../admin.md) › [secrets](./secrets.md) › **re-wrap**_

## NAME

nico-admin-cli-secrets-re-wrap - Re-wrap secret DEKs to use the
currently active KEK per routing config

## SYNOPSIS

**nico-admin-cli secrets re-wrap** \[**--batch-size**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Re-wrap secret DEKs to use the currently active KEK per routing config

## OPTIONS

**--batch-size** *\<BATCH_SIZE\>*  
Rows scanned per batch during the walk. The server applies its own
default and limits.

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
nico-admin-cli secrets re-wrap
nico-admin-cli secrets re-wrap --batch-size 25
```

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../README.md)
