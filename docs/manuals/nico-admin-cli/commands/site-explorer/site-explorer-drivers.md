# `nico-admin-cli site-explorer drivers`

*[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › **drivers***

## NAME

nico-admin-cli-site-explorer-drivers - Show the drivers a BMC uses, the
rule that chose each, and the identity they were selected from.

## SYNOPSIS

```text
nico-admin-cli site-explorer drivers [--extended]
[--sort-by] [-h|--help] <ADDRESS>
```

## DESCRIPTION

Show the drivers a BMC uses, the rule that chose each, and the identity
they were selected from.

## OPTIONS

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with -h)

`<ADDRESS>`

BMC IP address

## Examples

```sh
nico-admin-cli site-explorer drivers 192.0.2.10
nico-admin-cli --format json site-explorer drivers 192.0.2.10
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
