# `nico-admin-cli attestation spdm machines list`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › [machines](./attestation-spdm-machines.md) › **list***

## NAME

nico-admin-cli-attestation-spdm-machines-list - List predicted machine
attestation statuses

## SYNOPSIS

```text
nico-admin-cli attestation spdm machines list [--passed]
[--failed] [--cancelled] [--in-progress]
[-h|--help]
```

## DESCRIPTION

List predicted SPDM attestation statuses in machine-ID order.

Uses every device in the latest retained controller-triggered job for
each machine, chosen by descending start time and then descending job
ID. External and older jobs are ignored; BMCs are not contacted.
Unfinished devices take precedence over failures, cancellations, then
all-passed. Without a status flag, lists all machines; machines without
a controller job show unknown with an empty job ID; JSON and YAML use
null for both fields. Such machines are excluded by every status flag.
Only one status flag may be supplied. A partially collected job is in
progress even if a device has failed. The CLI fetches all pages
automatically. Predictions can change between pages. Prints the
prediction warning once to stderr, including when no machines match.
Supports table, JSON, and YAML output; CSV is unsupported.

## OPTIONS

`--passed`

Only machines whose latest controller job passed for every device

`--failed`

Only machines whose latest controller job failed with no unfinished
devices

`--cancelled`

Only machines whose latest controller job has cancellations, with no
unfinished or failed devices

`--in-progress`

Only machines whose latest controller job has unfinished devices, even
if another device failed

`-h, --help`

Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli attestation spdm machines list
nico-admin-cli attestation spdm machines list --passed
nico-admin-cli attestation spdm machines list --failed
nico-admin-cli attestation spdm machines list --cancelled
nico-admin-cli attestation spdm machines list --in-progress
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
