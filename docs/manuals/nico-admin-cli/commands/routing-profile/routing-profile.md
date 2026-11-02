# `nico-admin-cli routing-profile`

*[Network commands](../../network.md) › **routing-profile***

## NAME

nico-admin-cli-routing-profile - Inspect site-configured named routing
profiles

## SYNOPSIS

```text
nico-admin-cli routing-profile [-h|--help]
<subcommands>
```

## DESCRIPTION

Inspect named profiles from the fnn.routing_profiles configuration
loaded by the connected Core server. This is site-wide configuration
inventory, not the permitted choices for a tenant or the effective
policy for a VPC. The inline admin VPC profile is excluded.

## OPTIONS

`-h, --help`

Print help (see a summary with -h)

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./routing-profile-show.md) | Show one or more configured routing profile definitions |

---

**Related:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
