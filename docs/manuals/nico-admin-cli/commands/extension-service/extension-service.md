# `nico-admin-cli extension-service`

*[Tenant commands](../../tenant.md) › **extension-service***

## NAME

nico-admin-cli-extension-service - Extension service management

## SYNOPSIS

**nico-admin-cli extension-service** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Extension service management

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`create`](./extension-service-create.md) | Create an extension service |
| [`update`](./extension-service-update.md) | Update an extension service |
| [`delete`](./extension-service-delete.md) | Delete an extension service |
| [`show`](./extension-service-show.md) | Show extension service information |
| [`get-version`](./extension-service-get-version.md) | Get extension service version information |
| [`show-instances`](./extension-service-show-instances.md) | Show instances using an extension service |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
