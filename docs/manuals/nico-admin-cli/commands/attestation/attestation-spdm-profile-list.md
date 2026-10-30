# `nico-admin-cli attestation spdm profile list`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › [profile](./attestation-spdm-profile.md) › **list***

## NAME

nico-admin-cli-attestation-spdm-profile-list - List every stored
attestation profile

## SYNOPSIS

```text
nico-admin-cli attestation spdm profile list [--sort-by]
[-h|--help]
```

## DESCRIPTION

List every stored attestation profile

## OPTIONS

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli attestation spdm profile list
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
