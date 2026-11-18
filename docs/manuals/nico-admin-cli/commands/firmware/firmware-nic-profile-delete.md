# `nico-admin-cli firmware nic profile delete`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › [profile](./firmware-nic-profile.md) › **delete***

## NAME

nico-admin-cli-firmware-nic-profile-delete - Delete a NIC firmware
profile with no selections at its observed version

## SYNOPSIS

```text
nico-admin-cli firmware nic profile delete [OPTIONS] --if-version-match <VERSION> <ID>
```

## DESCRIPTION

Delete a NIC firmware profile using its original observed
--if-version-match token. For an unknown profile, Core returns server
status NOT_FOUND. A stale version or any site, machine, card, or tenant
selection referencing the profile returns server status
FAILED_PRECONDITION, including saved card selectors no longer in
inventory.

The command preserves the server diagnostic, does not read a fresh
version, and does not retry deletion. After a connection error, inspect
the profile; deletion may have committed. Keep the original token when
repeating the same request. No hardware selection is changed and no
firmware update is started.

Delete writes no bytes to stdout or the requested output file, for every
--format. Put --output PATH before the firmware subcommand. PATH must
not exist and is created before deletion, even if deletion later fails;
successful deletion leaves that file empty. --sort-by and --extended
have no effect.

## OPTIONS

`--if-version-match <VERSION>`

Original observed edit version, such as V1-T1789080000000000; keep
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

`<ID>`

Case-sensitive profile ID; nonempty, without surrounding whitespace or
controls

## Examples

```sh
nico-admin-cli firmware nic profile delete connectx-retired --if-version-match V3-T1789080002000000
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
