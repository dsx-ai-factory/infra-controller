# `nico-admin-cli rack profile list`

*[Hardware commands](../../hardware.md) › [rack](./rack.md) › [profile](./rack-profile.md) › **list***

## NAME

nico-admin-cli-rack-profile-list - List configured rack profiles

## SYNOPSIS

**nico-admin-cli rack profile list** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List rack profiles from the effective runtime configuration. Rack
profiles are not persisted rack resources. To add or change a profile,
update the runtime configuration.

## OPTIONS

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field\

\
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli rack profile list
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
