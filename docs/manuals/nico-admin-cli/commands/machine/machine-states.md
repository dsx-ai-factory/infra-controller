# `nico-admin-cli machine states`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › **states***

## NAME

nico-admin-cli-machine-states - Display machine lifecycle states without
full records

## SYNOPSIS

```text
nico-admin-cli machine states [OPTIONS] [MACHINE_ID]...
```

## DESCRIPTION

Display machine IDs and their current lifecycle states without health,
hardware, or history data.

With no IDs, list hosts, predicted hosts, and DPUs. Unknown IDs are
omitted. --hosts includes predicted hosts; --dpus selects only DPUs.
Both flags select all types. Explicit IDs cannot be combined with either
flag.

Requests use the smaller of --internal-page-size and the batch limit
advertised by Core. Each RPC operation shares one
FORGE_CLIENT_REQUEST_TIMEOUT_SECS deadline across its retries and
complete response. This accepts positive whole seconds; unset, zero, or
invalid values use 300 seconds. A failed call fails the command without
partial output.

If Core returns UNIMPLEMENTED or PERMISSION_DENIED for the compact RPC,
the command uses separately authorized full machine reads for that page
and the remaining pages. Other failures are reported.

ASCII and CSV contain Id and State columns; JSON and YAML contain a
machines list of id/state records. --sort-by selects primary-id
(default) or state order, with IDs breaking state ties. State strings
preserve the capitalization from Core. --max-width affects only ASCII
output.

## OPTIONS

`--hosts`

Show only hosts and predicted hosts; combine with --dpus to show all
types

`--dpus`

Show only DPUs; combine with --hosts to show all types

`--max-width <[COLUMN=]WIDTH>`

Limit displayed column width to WIDTH characters, truncating longer
values with an ellipsis ('...'). A column never narrows below its
header's length, so WIDTH is an upper bound on values, not a guaranteed
rendered width: a WIDTH shorter than the header still lets values fill
the header's width for free, and the ellipsis is only added when that
effective width (WIDTH, or the header's length if longer) exceeds 3
characters; at 3 or fewer there's no room for one, so the value is
truncated without it. WIDTH 0 means no limit (the same as not specifying
that column at all; useful as COLUMN=0 to exempt one column from a
blanket --max-width). Repeatable. A bare WIDTH applies to every column;
COLUMN=WIDTH limits just that column, where COLUMN must exactly match
the column's displayed header text (case-insensitive), e.g. State=40.
For a header containing spaces, quote the whole COLUMN=WIDTH argument,
e.g. "COLUMN NAME=40". An unmatched COLUMN is ignored with a warning
listing the valid headers for this invocation.

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

Print help (see a summary with '-h')

[*MACHINE_ID*]

Machine IDs to query. Omit to list all machines. Repeated IDs return one
entry.

## Examples

```sh
nico-admin-cli machine states
nico-admin-cli machine states --hosts
nico-admin-cli machine states --dpus
nico-admin-cli machine states fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli --format json --sort-by state machine states
nico-admin-cli machine states --max-width State=40
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
