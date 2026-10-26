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

Inspect named profiles from the connected Core servers loaded
fnn.routing_profiles configuration. This is site-wide configuration
inventory, not a tenants permitted choices or a VPCs effective policy.
The inline admin VPC profile is excluded.

## OPTIONS

`-h, --help`

Print help (see a summary with -h)

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./routing-profile-show.md) | Show one or more configured routing profile definitions |

---

**Related:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
