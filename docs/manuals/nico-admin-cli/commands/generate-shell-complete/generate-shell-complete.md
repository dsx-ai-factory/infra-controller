# `nico-admin-cli generate-shell-complete`

*[Admin commands](../../admin.md) › **generate-shell-complete***

## NAME

nico-admin-cli-generate-shell-complete - Generate shell autocomplete.
Source the output of this command: \`source \<(nico-admin-cli
generate-shell-complete bash)\`

## SYNOPSIS

**nico-admin-cli generate-shell-complete** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Generate shell autocomplete. Source the output of this command: \`source
\<(nico-admin-cli generate-shell-complete bash)\`

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
source <(nico-admin-cli generate-shell-complete bash)
source <(nico-admin-cli generate-shell-complete zsh)
nico-admin-cli generate-shell-complete zsh > ~/.zfunc/_nico-admin-cli && echo 'compdef _nico-admin-cli carbide-admin-cli forge-admin-cli' >> ~/.zshrc
nico-admin-cli generate-shell-complete fish > ~/.config/fish/completions/nico-admin-cli.fish
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`bash`](./generate-shell-complete-bash.md) |  |
| [`fish`](./generate-shell-complete-fish.md) |  |
| [`zsh`](./generate-shell-complete-zsh.md) |  |

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../index.md)
