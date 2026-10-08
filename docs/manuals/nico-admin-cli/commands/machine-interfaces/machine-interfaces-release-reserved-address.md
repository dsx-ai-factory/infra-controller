# `nico-admin-cli machine-interfaces release-reserved-address`

*[Hardware commands](../../hardware.md) › [machine-interfaces](./machine-interfaces.md) › **release-reserved-address***

## NAME

nico-admin-cli-machine-interfaces-release-reserved-address - Release a
parked address reservation

## SYNOPSIS

```text
nico-admin-cli machine-interfaces release-reserved-address
[--mac-address] [--address] [--extended]
[--sort-by] [-h|--help]
```

## DESCRIPTION

Release parked address reservations, making their addresses available to
allocators again.

At least one of --mac-address or --address must be specified so a
release cannot clear every reservation by accident. Only parked
reservations are affected; active interface addresses are never
released.

## OPTIONS

`--mac-address <MAC_ADDRESS>`

Release reservations owned by this MAC address.

`--address <ADDRESS>`

Release the reservation for this exact address.

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

## Examples

```sh
nico-admin-cli machine-interfaces release-reserved-address --mac-address 00:11:22:33:44:55
nico-admin-cli machine-interfaces release-reserved-address --address 192.0.2.10
nico-admin-cli machine-interfaces release-reserved-address --mac-address 00:11:22:33:44:55 --address 192.0.2.10
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
