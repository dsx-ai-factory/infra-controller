# `nico-admin-cli ip find`

*[Network commands](../../network.md) › [ip](./ip.md) › **find***

## NAME

nico-admin-cli-ip-find

## SYNOPSIS

**nico-admin-cli ip find** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*IP*\>

## DESCRIPTION

## OPTIONS

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

\<*IP*\>  
The IP address we are looking to identify

## Examples

```sh
nico-admin-cli ip find 192.0.2.10
nico-admin-cli ip find 2001:db8::1
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
