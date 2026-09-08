# `nico-admin-cli operating-system create`

*[Tenant commands](../../tenant.md) › [operating-system](./operating-system.md) › **create***

## NAME

nico-admin-cli-operating-system-create - Create a new operating system
definition.

## SYNOPSIS

```text
nico-admin-cli operating-system create <-n|--name>
[-o|--org] [--id] [-d|--description]
[--is-active] [--allow-override]
[--phone-home-enabled] [--user-data] [--ipxe-script]
[--ipxe-template-id] [--param] [--extended]
[--sort-by] [-h|--help]
```

## DESCRIPTION

Create a new operating system definition.

## OPTIONS

`-n, --name <NAME>`

Name of the operating system definition.

`-o, --org <ORG>`

Optional tenant organization identifier for this OS definition. Omit for
OS definitions owned by provider.

`--id <ID>`

Optional UUID for the new OS definition (default: server-generated).

`-d, --description <DESCRIPTION>`

Optional human-readable description.

`--is-active <IS_ACTIVE>`

Whether this OS definition is active (default: true).

*Possible values:*

- true

- false

`--allow-override`

Allow instance requests to override the raw iPXE boot script stored by
this OS definition. Applies only when the definition stores a raw iPXE
script; does not affect templated definitions or user data.

`--phone-home-enabled`

Whether instances using this OS definition wait for a guest phone-home
callback before reporting ready. If the callback never arrives, the
instance remains in a provisioning state. REST workflows inject the
cloud-init phone_home block and require valid cloud-init YAML; callers
using Core directly must arrange the callback. See
[Phone-home](../../../../configuration/tenant_management.md#phone-home).

`--user-data <USER_DATA>`

Optional cloud-init / user-data script.

`--ipxe-script <IPXE_SCRIPT>`

Raw iPXE boot script (mutually exclusive with --ipxe-template-id).

`--ipxe-template-id <IPXE_TEMPLATE_ID>`

ID of the iPXE template to use (mutually exclusive with --ipxe-script).

`--param <KEY=VALUE>`

iPXE parameter in KEY=VALUE format. May be repeated.

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

`-h, --help`

Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli operating-system create --name ubuntu-22.04 --org fds34511233a
nico-admin-cli operating-system create --name ubuntu-22.04 --org fds34511233a --description "Ubuntu 22.04 base" --is-active false
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
