# `nico-admin-cli firmware nic site set`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › [site](./firmware-nic-site.md) › **set***

## NAME

nico-admin-cli-firmware-nic-site-set - Create or conditionally replace a
site NIC firmware selection

## SYNOPSIS

```text
nico-admin-cli firmware nic site set [OPTIONS] --part-number <PN> --psid <PSID> --profile <ID>
```

## DESCRIPTION

Select a compatible live profile for one exact site hardware pair. Omit
--if-version-match only to create a new selection; Core returns
ALREADY_EXISTS if the pair already has a selection. To replace a
selection, pass its complete observed selection version, not the profile
edit version. Stale versions return FAILED_PRECONDITION and absent
selections return NOT_FOUND. A missing profile returns NOT_FOUND; an
incompatible profile returns FAILED_PRECONDITION. Core checks profile
compatibility before the selection version.

Part number and PSID are case-sensitive literal identifiers. Core
requires 1–256 UTF-8 bytes each, without surrounding whitespace or
controls; invalid hardware returns INVALID_ARGUMENT. Wildcards are not
expanded. The CLI neither reads a fresh token nor retries a rejected
write. After a connection error, inspect the hardware pair with firmware
nic site list before another write; the change may have committed. Keep
the original token when repeating the same request.

The selection follows subsequent edits to the live profile without
changing its independent selection token. This command leaves the
profile definition unchanged and does not fetch artifacts or start
firmware updates. Finish upgrading all Core replicas before assigning
site selections.

Use --format ascii-table (default), csv, json, or yaml and --output PATH
before the firmware subcommand. JSON/YAML returns a one-element array
with hardware (part_number and psid), profile_id, and version. Table/CSV
returns one row with Part Number, PSID, Profile ID, and Selection
Version columns. Output defaults to stdout. --output requires a path
that does not exist and creates the file before the operation, even if
it later fails. --sort-by and --extended have no effect.

## OPTIONS

`--part-number <PN>`

Literal manufacturer part number

`--psid <PSID>`

Literal firmware parameter-set identifier

`--profile <ID>`

Case-sensitive compatible profile ID; nonempty, without surrounding
whitespace or controls

`--if-version-match <VERSION>`

Original observed selection version, such as V1-T1789080000000000; omit
only to create

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
nico-admin-cli firmware nic site set --part-number MCX75310AAS-NEAT --psid MT_0000000838 --profile connectx-production
nico-admin-cli --format json firmware nic site set --part-number MCX75310AAS-NEAT --psid MT_0000000838 --profile connectx-candidate --if-version-match V1-T1789080000000000
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
