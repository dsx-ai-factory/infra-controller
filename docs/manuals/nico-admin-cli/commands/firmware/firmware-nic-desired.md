# `nico-admin-cli firmware nic desired`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › **desired***

## NAME

nico-admin-cli-firmware-nic-desired - Show desired NIC firmware and
selection diagnostics

## SYNOPSIS

```text
nico-admin-cli firmware nic desired [OPTIONS] <HOST>
```

## DESCRIPTION

Read desired NIC firmware for a host using Cores stored Scout
observation. This command does not contact Scout, fetch artifacts,
change selections, flash firmware, or change readiness. Reported
identities and timestamps do not prove physical-card identity,
observation freshness, or update eligibility.

Use matching v2.4 Core and CLI schemas; finish upgrading all Core
replicas before relying on saved card selections or these
desired-firmware diagnostics.

Core resolves selections in precedence order: card, machine, allocation,
then site. The optional allocation profile previews one requirement
against every usable, non-managed-DPU device; it does not persist an
allocation. Conflicts with card or machine selections, invalid
selections, and excluded devices remain per-device errors without
fallback. Exact targets may be downgrades. No selection is Unmanaged.

Targets retain server report order. Core-reported per-device errors do
not hide other devices or fail an otherwise successful report. A
malformed or unsupported Core response fails the entire read without a
partial report. Warnings identify saved card selectors absent from the
observation, including when no observation exists. Without an
observation, the report has no targets and does not check an allocation
profiles existence.

Use --format ascii-table (default), csv, json, or yaml and --output PATH
before the firmware subcommand. JSON/YAML contains observed_at, targets
(original identity, result, and site_selection_version), and warnings.
JSON results use Firmware or Error objects; YAML uses !Firmware or
!Error tags. Both use null for unmanaged devices. Firmware results
include scope, profile ID/version, and the full entry with artifacts.
ASCII/CSV includes reported Base MAC and Base GUID for comparison with
saved card selectors. ASCII prints observation time and warnings before
the table. CSV repeats Observed At and Warnings columns per device, with
multiple warnings newline-separated in one cell. With no targets, CSV
emits one metadata row with Status No devices and empty device fields.
An absent observation reads Not observed in ASCII/CSV and null in
JSON/YAML. Missing table values are blank.

Output defaults to stdout. --output requires a path that does not exist;
the file is created before the operation, even if the operation later
fails. --sort-by and --extended have no effect.

## OPTIONS

`--allocation-profile <ID>`

Optional case-sensitive profile ID for a machine-wide allocation
preview; nonempty without surrounding whitespace or controls; omission
uses saved selections

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

`<HOST>`

Stable or predicted host machine ID; DPU IDs are rejected

## Examples

```sh
nico-admin-cli firmware nic desired fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli firmware nic desired fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg --allocation-profile connectx-production
nico-admin-cli --format json --output ./nic-desired.json firmware nic desired fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
