# `nico-admin-cli firmware nic machine show`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › [machine](./firmware-nic-machine.md) › **show***

## NAME

nico-admin-cli-firmware-nic-machine-show - Show saved NIC firmware
selections and the machine edit version

## SYNOPSIS

```text
nico-admin-cli firmware nic machine show [OPTIONS] <HOST>
```

## DESCRIPTION

Read saved machine and card NIC firmware selections, including selectors
that no longer match the stored device report. This is configuration,
not effective firmware; use firmware nic desired for resolution and
errors. The machine version is the shared edit token required by machine
set, not a profile version.

Use matching v2.4 CLI and Core NIC firmware schemas, and finish
upgrading all Core replicas before relying on the reported selections.

Put --format ascii-table (default), csv, json, or yaml and --output PATH
before firmware. JSON/YAML returns one object with machine_id, version,
profile_id (null means inherit), and card_selections. YAML selectors use
!BaseMac and !BaseGuid tags. Extract card_selections as JSON for
--cards-file; YAML is not accepted as input. Tables have one machine row
plus one row per card in saved order; an empty machine profile cell
means inherit. Card ordering has no selection significance.

An unknown host fails. Output defaults to stdout; --output must name a
new file and creates it before the read, even on failure. --sort-by and
--extended have no effect. No configuration, readiness, or hardware is
changed.

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

`<HOST>`

Stable or predicted host machine ID, not a DPU ID

## Examples

```sh
nico-admin-cli firmware nic machine show fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli --format json --output ./nic-machine.json firmware nic machine show fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
