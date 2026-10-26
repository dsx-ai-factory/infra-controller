# `nico-admin-cli firmware nic machine set`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › [machine](./firmware-nic-machine.md) › **set***

## NAME

nico-admin-cli-firmware-nic-machine-set - Set or clear machine and card
selections at an observed machine version

## SYNOPSIS

```text
nico-admin-cli firmware nic machine set [OPTIONS] --if-version-match <VERSION> <--profile <ID>|--clear-profile> <HOST>
```

## DESCRIPTION

Atomically set or clear a hosts machine and card NIC firmware selections
at the observed Machine.version from firmware nic machine show. Choose
exactly one of --profile or --clear-profile, even when changing only
cards. Resubmitting a machine profile rechecks compatibility for devices
inheriting it; clearing it restores allocation/site inheritance without
clearing saved cards.

Omitting --cards-file preserves all saved card selections. Supplying it
replaces the complete card list in the same request; include every
selection you want to keep, or use [] to clear all cards. Removing a
card restores inheritance from the requested machine profile,
allocation, or site selection. A supplied machine profile must support
newly inheriting devices. Profiles are live, not pinned revisions.
Finish upgrading Core before using these configuration commands.

Core validates each supplied card against its latest stored device
report. Missing, ambiguous, overlapping, managed-DPU, or incompatible
devices return FAILED_PRECONDITION. Missing profiles or hosts return
NOT_FOUND; a stale machine version returns FAILED_PRECONDITION.
Rejection leaves all selections unchanged. Every successful write
advances the shared machine edit token, including unchanged selections.
No firmware is fetched or applied, and machine readiness does not
change.

The command never refreshes a token or retries a mutation. After a
connection error, inspect machine show before another write: the change
may have committed. Keep the original token when repeating the same
request. Success writes no output; use machine show to read the new
version. Global --format, --sort-by and --extended have no effect.
--output PATH before firmware still creates a new empty file before the
operation, including when the operation fails.

## OPTIONS

`--profile <ID>`

Live machine profile ID; case-sensitive, nonempty, without surrounding
whitespace or controls

`--clear-profile`

Clear the machine profile to inherit; saved cards are preserved unless
--cards-file replaces them

`--if-version-match <VERSION>`

Complete observed Machine.version; keep unchanged when repeating this
request

`--cards-file <PATH>`

Read a complete UTF-8 JSON array of card selections. Omission preserves
saved cards without reading stdin; - reads stdin until EOF and any other
value is a file path. [] clears all card selections. Null, an
enclosing object, or YAML is not accepted. Each entry requires selector
and profile_id. The selector is exactly one case-sensitive BaseMac or
BaseGuid tag containing a string.

BaseMac accepts twelve hex digits or six two-digit octets separated
consistently by colons or hyphens; it must be nonzero and unicast.
BaseGuid is an exact reported string of 1–256 UTF-8 bytes without
surrounding whitespace or controls; -- and N/A are invalid. Profile IDs
are literal, nonempty strings without surrounding whitespace or
controls. Unknown entry fields are rejected. Selector and profile-ID
syntax are checked locally; Core checks profile existence, device
matches, compatibility, duplicate selections and its max_find_by_ids
card count limit.

Each selector must match exactly one stored device. Ordering is not
priority. To reuse machine show JSON output, extract card_selections as
JSON. A terminal stdin prints a reading hint to stderr. Unreadable or
invalid JSON fails before the configuration RPC.

Example replacement (use identities from the hosts stored device
report):

```json

[ {"selector": {"BaseMac": "00:11:22:33:44:55"}, "profile_id":
"connectx-production"}, {"selector": {"BaseGuid": "0011223344556677"},
"profile_id": "connectx-production"} ]

```

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
nico-admin-cli firmware nic machine set fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg --profile connectx-production --if-version-match V1-T1789080000000000
nico-admin-cli firmware nic machine set fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg --profile connectx-production --cards-file ./nic-cards.json --if-version-match V2-T1789080001000000
nico-admin-cli firmware nic machine set fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg --clear-profile --if-version-match V3-T1789080002000000
printf '[]' | nico-admin-cli firmware nic machine set fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg --clear-profile --cards-file - --if-version-match V4-T1789080003000000
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
