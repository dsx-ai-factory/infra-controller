# `nico-admin-cli operating-system create`

*[Tenant commands](../../tenant.md) › [operating-system](./operating-system.md) › **create***

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

Exactly one OS variant must be specified: --ipxe-script or
--ipxe-template-id.

For templated iPXE requirements, artifact configuration, and
synchronization rules, see
[Templated iPXE Operating Systems](../../../../configuration/templated-ipxe-operating-systems.md).

## OPTIONS

**-n**, **--name** *\<NAME\>*  
Name of the operating system definition.

**-o**, **--org** *\<ORG\>*  
Optional tenant organization identifier for this OS definition. Omit for
a provider-owned definition. An explicitly empty value is invalid.

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
Allow an Instance request to override the user data of this OS
definition.

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
nico-admin-cli operating-system create --name provider-ubuntu-22.04 --ipxe-template-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli operating-system create --name tenant-ubuntu-22.04 --org fds34511233a --ipxe-template-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli operating-system create --name tenant-ubuntu-22.04 --org fds34511233a --description "Ubuntu 22.04 base" --is-active false --allow-override --ipxe-template-id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
