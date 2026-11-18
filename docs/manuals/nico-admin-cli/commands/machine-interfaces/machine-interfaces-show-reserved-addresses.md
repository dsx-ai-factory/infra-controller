# `nico-admin-cli machine-interfaces show-reserved-addresses`

*[Hardware commands](../../hardware.md) › [machine-interfaces](./machine-interfaces.md) › **show-reserved-addresses***

## NAME

nico-admin-cli-machine-interfaces-show-reserved-addresses - List parked
address reservations that outlived their interface

## SYNOPSIS

```text
nico-admin-cli machine-interfaces show-reserved-addresses
[--mac-address] [--address] [--extended]
[--sort-by] [-h|--help]
```

## DESCRIPTION

List parked address reservations that outlived their interface.

A reservation is an address kept by its interface MAC after the
interface row was deleted, so the same MAC can reclaim it on
re-ingestion. With no filter, every parked reservation is listed; the
filters narrow the listing.

## OPTIONS

`--mac-address <MAC_ADDRESS>`

Only show reservations owned by this MAC address.

`--address <ADDRESS>`

Only show the reservation for this exact address.

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
nico-admin-cli machine-interfaces show-reserved-addresses
nico-admin-cli machine-interfaces show-reserved-addresses --mac-address 00:11:22:33:44:55
nico-admin-cli machine-interfaces show-reserved-addresses --address 192.0.2.10
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
