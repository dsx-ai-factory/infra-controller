# `nico-admin-cli routing-profile show`

*[Network commands](../../network.md) › [routing-profile](./routing-profile.md) › **show***

## NAME

nico-admin-cli-routing-profile-show - Show one or more configured
routing profile definitions

## SYNOPSIS

```text
nico-admin-cli routing-profile show [--name]
[--internal] [--minimum-access-tier] [-h|--help]
```

## DESCRIPTION

Show one or more configured routing profile definitions in name order,
preserving unset values

ASCII and CSV output show one summary row per profile unless exactly one
distinct name is explicitly requested. A single explicit name shows all
properties: labeled lines in ASCII, or one row per property in CSV.
Filtering a list down to one result does not select the detailed view.
Unset values are displayed as unset. JSON and YAML contain a list,
including when one name is supplied. Omitted names fetch matching
profiles in batches. All supplied filters must match, including when
explicit names are supplied. Configuration can change between calls;
names removed after discovery are omitted. Explicit missing names cause
an error before any output is written.

## OPTIONS

`--name <NAME>...`

Exact, case-sensitive names; omit to discover matching profiles. Unknown
explicit names fail; repeat --name or supply multiple values

`--internal <INTERNAL>`

True returns only internal profiles; false returns only external
profiles. Omit to include both. Profiles with no value for the internal
setting are treated as external.

*Possible values:*

> - true
>
> - false

`--minimum-access-tier <MINIMUM_ACCESS_TIER>`

Inclusive minimum access tier; profiles without an access_tier setting
count as zero

`-h, --help`

Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli routing-profile show
nico-admin-cli --format json routing-profile show --name MAINTENANCE
nico-admin-cli routing-profile show --name MAINTENANCE EXTERNAL
nico-admin-cli routing-profile show --minimum-access-tier 1
nico-admin-cli routing-profile show --name MAINTENANCE INTERNAL --internal true --minimum-access-tier 1
nico-admin-cli routing-profile show --internal false
```

---

**Related:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
