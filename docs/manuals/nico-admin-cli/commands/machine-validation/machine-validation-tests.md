# `nico-admin-cli machine-validation tests`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › **tests***

## NAME

nico-admin-cli-machine-validation-tests - Supported Tests

## SYNOPSIS

**nico-admin-cli machine-validation tests** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Supported Tests

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
nico-admin-cli machine-validation tests show
nico-admin-cli machine-validation tests verify --test-id gpu_bandwidth --version 1.2.0
nico-admin-cli machine-validation tests enable --test-id gpu_bandwidth --version 1.2.0
nico-admin-cli machine-validation tests disable --test-id gpu_bandwidth --version 1.2.0
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./machine-validation-tests-show.md) | Show tests |
| [`verify`](./machine-validation-tests-verify.md) | Verify a given test |
| [`add`](./machine-validation-tests-add.md) | Add new test case |
| [`update`](./machine-validation-tests-update.md) | Update existing test case |
| [`enable`](./machine-validation-tests-enable.md) | Enabled a test |
| [`disable`](./machine-validation-tests-disable.md) | Disable a test |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
