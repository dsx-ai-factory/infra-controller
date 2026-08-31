# `nico-admin-cli operating-system show`

*[Tenant commands](../../tenant.md) › [operating-system](./operating-system.md) › **show***

## NAME

nico-admin-cli-operating-system-show - Show operating system definitions
(all, or one by ID).

## SYNOPSIS

**nico-admin-cli operating-system show** \[**--org**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Show operating system definitions (all, or one by ID).

## OPTIONS

**--org** *\<ORG\>*  
Filter by organization identifier (when listing).

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

\[*ID*\]  
Operating system definition ID; omit to list all.

## Examples

```sh
nico-admin-cli operating-system show
nico-admin-cli operating-system show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli operating-system show --org fds34511233a
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
