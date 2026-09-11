# `nico-admin-cli operating-system set-cached-url`

*[Tenant commands](../../tenant.md) › [operating-system](./operating-system.md) › **set-cached-url***

## NAME

nico-admin-cli-operating-system-set-cached-url - Set or clear cached_url
on OS artifacts.

## SYNOPSIS

**nico-admin-cli operating-system set-cached-url** \<**--set**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \<*ID*\>

## DESCRIPTION

Set or clear cached_url on OS artifacts.

For cache strategies, readiness behavior, and the complete templated
iPXE workflow, see
[Templated iPXE Operating Systems](../../../../configuration/templated-ipxe-operating-systems.md).

## OPTIONS

**--set** *\<NAME=URL\>*  
Set cached_url for an artifact. Use NAME=URL to set, NAME= to clear. May
be repeated.

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

\<*ID*\>  
UUID of the operating system definition.

## Examples

```sh
nico-admin-cli operating-system set-cached-url 12345678-1234-5678-90ab-cdef01234567 --set kernel=https://cache.example.com/vmlinuz
nico-admin-cli operating-system set-cached-url 12345678-1234-5678-90ab-cdef01234567 --set kernel=
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
