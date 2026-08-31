# `nico-admin-cli operating-system update`

*[Tenant commands](../../tenant.md) › [operating-system](./operating-system.md) › **update***

## NAME

nico-admin-cli-operating-system-update - Update an existing operating
system definition.

## SYNOPSIS

**nico-admin-cli operating-system update** \[**-n**\|**--name**\]
\[**-d**\|**--description**\] \[**--is-active**\]
\[**--allow-override**\] \[**--phone-home-enabled**\]
\[**--user-data**\] \[**--ipxe-script**\] \[**--ipxe-template-id**\]
\[**--param**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*ID*\>

## DESCRIPTION

Update an existing operating system definition.

For templated iPXE requirements, artifact configuration, and
synchronization rules, see
[Templated iPXE Operating Systems](../../../../configuration/templated-ipxe-operating-systems.md).

## OPTIONS

**-n**, **--name** *\<NAME\>*  
New name for the operating system definition.

**-d**, **--description** *\<DESCRIPTION\>*  
New description.

**--is-active** *\<IS_ACTIVE\>*  
Set whether this OS definition is active.\

\
*Possible values:*

- true

- false

**--allow-override** *\<ALLOW_OVERRIDE\>*  
Set whether an Instance request can override the user data of this OS
definition.\

\
*Possible values:*

- true

- false

**--phone-home-enabled** *\<PHONE_HOME_ENABLED\>*  
Set whether phone-home on first boot is enabled.\

\
*Possible values:*

- true

- false

**--user-data** *\<USER_DATA\>*  
Update the cloud-init / user-data script.

**--ipxe-script** *\<IPXE_SCRIPT\>*  
Update the raw iPXE boot script.

**--ipxe-template-id** *\<IPXE_TEMPLATE_ID\>*  
Update the iPXE template ID.

**--param** \[*\<KEY=VALUE\>...*\]  
Replace all iPXE parameters with these KEY=VALUE pairs. May be repeated.
Pass without values to clear.

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
UUID of the operating system definition to update.

## Examples

```sh
nico-admin-cli operating-system update 12345678-1234-5678-90ab-cdef01234567 --name ubuntu-22.04 --description "Ubuntu 22.04 base"
nico-admin-cli operating-system update 12345678-1234-5678-90ab-cdef01234567 --is-active false
nico-admin-cli operating-system update 12345678-1234-5678-90ab-cdef01234567 --ipxe-script "#!ipxe …"
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
