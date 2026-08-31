# `nico-admin-cli site-explorer re-explore`

*[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › **re-explore***

## NAME

nico-admin-cli-site-explorer-re-explore - Asks carbide-api to explore a
single host in the next exploration cycle. The results will be stored.

## SYNOPSIS

**nico-admin-cli site-explorer re-explore** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*ADDRESS*\>

## DESCRIPTION

Asks carbide-api to explore a single host in the next exploration cycle.
The results will be stored.

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

\<*ADDRESS*\>  
BMC IP address

## Examples

```sh
nico-admin-cli site-explorer re-explore 192.0.2.10
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
