# `nico-admin-cli firmware nic site clear`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › [site](./firmware-nic-site.md) › **clear***

## NAME

nico-admin-cli-firmware-nic-site-clear - Clear a site NIC firmware
selection at its observed version

## SYNOPSIS

```text
nico-admin-cli firmware nic site clear [OPTIONS] --part-number <PN> --psid <PSID> --if-version-match <VERSION>
```

## DESCRIPTION

Clear the site NIC firmware selection for one exact part-number/PSID
pair. The complete observed selection version is required; the profile
edit version is not interchangeable. Core returns NOT_FOUND for an
absent selection and FAILED_PRECONDITION for a stale token. Part number
and PSID are case-sensitive literal identifiers; Core requires 1–256
UTF-8 bytes each without surrounding whitespace or controls, returning
INVALID_ARGUMENT for invalid hardware.

This removes only the site selection. It leaves the live profile and any
machine or card selections unchanged, and does not fetch artifacts or
start firmware updates. Devices with no other selection become
unmanaged. The CLI does not read a fresh token or retry a rejected
write. After a connection error, inspect the hardware pair with firmware
nic site list before another write; the clear may have committed. Keep
the original token when repeating the same request.

Success writes no bytes in any --format. Global --format and --output
PATH must precede the firmware subcommand. Output defaults to stdout.
--output requires a path that does not exist; the file is created before
the operation and remains empty even if the clear fails. --sort-by and
--extended have no effect.

## OPTIONS

`--part-number <PN>`

Literal manufacturer part number

`--psid <PSID>`

Literal firmware parameter-set identifier

`--if-version-match <VERSION>`

Original observed selection version, such as V1-T1789080000000000; keep
unchanged when repeating this request

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
nico-admin-cli firmware nic site clear --part-number MCX75310AAS-NEAT --psid MT_0000000838 --if-version-match V1-T1789080000000000
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
