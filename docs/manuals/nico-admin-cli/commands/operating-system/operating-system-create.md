# `nico-admin-cli operating-system create`

_[Tenant commands](../../tenant.md) › [operating-system](./operating-system.md) › **create**_

## NAME

nico-admin-cli-operating-system-create - Create a new operating system
definition.

## SYNOPSIS

**nico-admin-cli operating-system create** \<**-n**\|**--name**\>
\[**-o**\|**--org**\] \[**--id**\] \[**-d**\|**--description**\]
\[**--is-active**\] \[**--allow-override**\]
\[**--phone-home-enabled**\] \[**--user-data**\] \[**--ipxe-script**\]
\[**--ipxe-template-id**\] \[**--param**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Create a new operating system definition.

## OPTIONS

**-n**, **--name** *\<NAME\>*  
Name of the operating system definition.

**-o**, **--org** *\<ORG\>*  
Optional tenant organization identifier for this OS definition. Omit for
OS definitions owned by provider.

**--id** *\<ID\>*  
Optional UUID for the new OS definition (default: server-generated).

**-d**, **--description** *\<DESCRIPTION\>*  
Optional human-readable description.

**--is-active** *\<IS_ACTIVE\>*  
Whether this OS definition is active (default: true).\

\
*Possible values:*

- true

- false

**--allow-override**  
Allow users to override OS parameters.

**--phone-home-enabled**  
Enable phone-home on first boot.

**--user-data** *\<USER_DATA\>*  
Optional cloud-init / user-data script.

**--ipxe-script** *\<IPXE_SCRIPT\>*  
Raw iPXE boot script (mutually exclusive with --ipxe-template-id).

**--ipxe-template-id** *\<IPXE_TEMPLATE_ID\>*  
ID of the iPXE template to use (mutually exclusive with --ipxe-script).

**--param** *\<KEY=VALUE\>*  
iPXE parameter in KEY=VALUE format. May be repeated.

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
nico-admin-cli operating-system create --name ubuntu-22.04 --org fds34511233a
nico-admin-cli operating-system create --name ubuntu-22.04 --org fds34511233a --description "Ubuntu 22.04 base" --is-active false --allow-override
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
