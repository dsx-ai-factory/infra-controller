# `nico-admin-cli machine-validation external-config`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › **external-config***

## NAME

nico-admin-cli-machine-validation-external-config - External config

## SYNOPSIS

**nico-admin-cli machine-validation external-config** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

External config

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
nico-admin-cli machine-validation external-config show --name my-config
nico-admin-cli machine-validation external-config add-update --name my-config --file-name ./config.toml --description "validation overrides"
nico-admin-cli machine-validation external-config remove --name my-config
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./machine-validation-external-config-show.md) | Show External config |
| [`add-update`](./machine-validation-external-config-add-update.md) | Update External config |
| [`remove`](./machine-validation-external-config-remove.md) | Remove External config |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
