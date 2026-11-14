# `nico-admin-cli credential add-ufm`

*[Hardware commands](../../hardware.md) › [credential](./credential.md) › **add-ufm***

## NAME

nico-admin-cli-credential-add-ufm - Add UFM credential

## SYNOPSIS

```text
nico-admin-cli credential add-ufm <--url> [--token]
[--extended] [--sort-by] [-h|--help]
```

## DESCRIPTION

Add UFM credential

## OPTIONS

`--url <URL>`

The UFM url

`--token <TOKEN> [default: ]`

The UFM token; omit it to store an empty password, which selects the
SPIFFE default client certificate. Postgres storage and local credential
sources change this; see
https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/configuration/credential-sources.md#ufm-ownership-policy

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

## Examples

```sh
nico-admin-cli credential add-ufm --url https://192.0.2.10 --token mypassword
nico-admin-cli credential add-ufm --url https://192.0.2.10
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
