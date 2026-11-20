# `nico-admin-cli credential add-dpu-os`

*[Hardware commands](../../hardware.md) › [credential](./credential.md) › **add-dpu-os***

## NAME

nico-admin-cli-credential-add-dpu-os - Add the set-once site-wide DPU
Ubuntu login credential

## SYNOPSIS

```text
nico-admin-cli credential add-dpu-os <--password-stdin>
[--extended] [--sort-by] [-h|--help]
```

## DESCRIPTION

Add the set-once site-wide DPU Ubuntu login credential

## OPTIONS

`--password-stdin`

Read the password from standard input; password arguments are not
accepted

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
IFS= read -r -s -p 'DPU OS password: ' dpu_password; printf '\n'
printf '%s' "$dpu_password" | nico-admin-cli credential add-dpu-os --password-stdin
unset dpu_password
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
