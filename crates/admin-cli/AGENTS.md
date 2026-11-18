# Admin CLI guidance

Guidance for agents and humans working on `nico-admin-cli`. `CLAUDE.md` is a
symlink to this file; edit `AGENTS.md`. See the root
[AGENTS.md](../../AGENTS.md) for shared build, test, lint, and coding conventions.

## Command help examples

Every leaf subcommand must carry worked examples in Clap's `after_long_help`,
except trivial unit variants as described below. Examples appear at the bottom
of `--help`; `-h` shows only the summary. They must show realistic, copyable
invocations covering how the command is used.

### Coverage

Derive examples from the argument declarations: required and optional
positionals, `ArgGroup`s, `conflicts_with`, and `ValueEnum`s.

- Start with the simplest common invocation.
- Include an example for each meaningful axis: each selector/filter that changes
  the target, each mutually exclusive mode, and important optional flags that
  change behavior. Show both sides of choices such as `--pause`/`--resume` or
  filling missing values versus `--replace-all`.
- For show/list commands, cover both listing all entities and selecting one by ID.
- Show destructive or forceful flags explicitly, including `--force`, `--replace`,
  and `--replace-all` where supported.
- Use representative combinations rather than every flag combination. Two to six
  examples is the normal range; a single-purpose command may need only one.

Do not add repetitive examples to trivial no-argument unit variants whose
`about` text and parent's credential example already explain the invocation.
Add an example only if it reveals information absent from their help: a hidden
alias, non-obvious value format, ordering constraint, or prerequisite. For
example, Redfish's `force-off`, `force-restart`, `graceful-restart`, and
`graceful-shutdown` expose the hidden aliases `off`, `reset`, `restart`, and
`shutdown` in their examples. `visible_alias` already appears in help.

### Format and placeholders

Attach `#[command(after_long_help = "...")]` to the command's argument struct or
enum, or to its variant when required by the placement rules below:

```rust
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all VPCs:
    $ nico-admin-cli vpc show

Show details for one VPC:
    $ nico-admin-cli vpc show 12345678-1234-5678-90ab-cdef01234567

")]
pub struct Args {
    // fields ...
}
```

- Begin with literal `EXAMPLES:` and a blank line. The leading `\` after the
  opening quote removes the first newline.
- Give each example a description ending in `:`, followed by a command indented
  four spaces and prefixed with a dollar sign and a space.
- Leave one blank line between examples and one before the closing quote.
- Use `nico-admin-cli` and the canonical command path as Clap renders it.
- Wrap long source lines with a trailing `\` inside the Rust string. Indent each
  continuation four spaces and retain a space between arguments. Rust removes
  the escaped newline and leading whitespace; the rendered command is one line.
- Escape literal quotes as `\"`, for example `--description \"Front-end subnet\"`.

Use consistent, realistic placeholders instead of angle-bracket `<ID>` values:

| Kind | Placeholder |
| --- | --- |
| UUID / resource ID | `12345678-1234-5678-90ab-cdef01234567`; second: `abcdef01-2345-6789-abcd-ef0123456789` |
| Tenant org ID | `fds34511233a` |
| BMC / Host IP | `192.0.2.10` (TEST-NET-1), Host BMC `192.0.2.20`; with port: `192.0.2.10:22` |
| Username | `admin` |
| Password | `mypassword`; use `mynewpassword` for a password being set. Keep both obviously fictional. |
| MAC address | `00:11:22:33:44:55` |
| SKU ID | `DGX-H100-640GB` |
| File / directory | `./skus.json`, `/path/to/tpm-ca.der` |

For enum values, inspect `ValueEnum` or the displayed possible values and use a
real variant, such as `internal-maintenance`, `nvidia`, or `ready` where accepted.

### Command path and placement

Check the parent `enum Cmd` and its attributes. Variants default to kebab-case,
but `rename_all` and `name` can change the path. Use the canonical name rather
than an alias, and include every nested `subcommand` segment.

| Command declaration | Place `after_long_help` on |
| --- | --- |
| Command's own argument struct or enum | That type |
| Unit variant that needs an example | The variant, between its doc comment and declaration |
| Payload type shared by commands with different names | Each variant, with its own command path; leave the shared type bare |
| Type alias | The aliased type |
| Flattened newtype | The outer type; flattening contributes fields, not command-level help |
| Nested subcommand enum | The enum for group help, and each child's type or variant for child help |

### Verify rendered help

Build, render the affected command's long help to a file, and read its
`EXAMPLES:` section. Write to a file rather than piping into `grep`. The agent
sandbox can drop piped output, so a pipe can print nothing even when the block
is present. Replace `vpc show` with the affected command path.

```bash
mkdir -p tmp
cargo build -p nico-admin-cli
cargo run -q -p nico-admin-cli -- vpc show --help > tmp/help.txt 2>&1
sed -n '/EXAMPLES/,$p' tmp/help.txt
```

Check that continued source lines render as complete single-line commands and
that every command path matches the parsed subcommand name.

## Documentation

The hidden `generate-man` and `generate-cli-docs` subcommands build reference
documentation from the live Clap tree. Both take `--out-dir`:

- `generate-man` uses `clap_mangen` to create a roff file per command/subcommand,
  named by its full path, for example `nico-admin-cli-vpc-show.1`.
- `generate-cli-docs` converts those man pages to GitHub-flavored Markdown using
  the `pandoc` binary, which must be on `PATH` and is installed in CI build images.
  It writes `docs/manuals/nico-admin-cli/commands/<command>/<command>.md`, with
  descendant pages beside it as `<command>-<sub>.md`, plus the domain indexes.
  OPTIONS/SYNOPSIS supply flag details; the source `EXAMPLES:` commands supply
  each page's Examples section.

The overview [README.md](../../docs/manuals/nico-admin-cli/README.md) is
handwritten and is not regenerated; leave it alone when regenerating references.
See [the generator](src/generate_docs/cmd.rs) for output ownership.

Every visible top-level command must have a Hardware, Network, Tenant, or Admin
entry in [cli_domains.yaml](cli_domains.yaml). This is the grouping source of
truth, read by `domain_for_command` in [CliOptions](src/cfg/cli_options.rs).
Use the rendered command name. `every_command_has_a_domain` tests for missing
entries; `no_unknown_commands_in_domain_map` tests for renamed/removed commands
left in the map.

Whenever a command's help, examples, structure, or domain changes, regenerate
and include the updated reference in the commit. Run the local check as well:

```bash
cargo make gen-cli-docs   # Regenerate Markdown; requires pandoc on PATH
cargo make check-cli-docs # Test command/domain mappings, regenerate, check diff
cargo make gen-man        # Generate man pages in target/man
```

`check-cli-docs` fails on a `git diff` in `docs/manuals/nico-admin-cli` after
regeneration. It is not invoked by the [CI workflows](../../.github/workflows).

## Error messages

CLI errors must be understandable to an operator. Preserve `main`'s color-eyre
settings that disable the source-location section and the `RUST_BACKTRACE`
footer; do not re-enable them. An explicitly set `RUST_BACKTRACE=1` still captures
a backtrace.

For operational CLI errors, add `.context()` before every `?`. With source
locations hidden, this chain is the error message. Write each context to
complete "while attempting to ..." so the error and its causes read coherently.

Keep argument parsing and invocation validation on Clap's native parsing and
exit paths. Invalid invocations must produce Clap's `error:` prefix, usage,
`--help` hint, and exit code `2`. Do not replace them with a generic `eyre!`
that exits `1` without usage.

### Required and global arguments

Prefer Clap-native enforcement. A non-`Option` value such as `String` without a
default is required automatically; prefer it over `required = true` on
`Option<_>` and an unreachable `None` branch. Required arguments appear
unbracketed in parent and child usage. Skip a custom
`help_heading = "Required"`: it only groups the parent's options and adds
nothing to the child's option list.

Clap rejects a required global argument. Choose the scope deliberately:

| Argument form | Enforcement | Child help and argument position |
| --- | --- | --- |
| Required parent value | Clap | Required in usage, absent from child option list; must precede the child command |
| Global parent value | Validate at dispatch | Full entry in child options; may follow the child command |

Redfish uses the required-parent form for `--address`, an `IP:port` value whose
port defaults to `443`. Every Redfish child talks to the BMC directly, and the
flag must precede the child command. `--username` and `--password` remain
optional. Browsing a Redfish tree through the API server is the separate
top-level `browse redfish` command, which does not take `--address`.

Use the global form only when an argument must be accepted after the child
command or be required for some children but not others. Validate at dispatch
and raise a Clap error, rather than a runtime `eyre!("Missing --foo")` inside a handler:

```rust
use clap::CommandFactory;
use clap::error::ErrorKind;

CliOptions::command()
    .error(ErrorKind::MissingRequiredArgument, "actionable message")
    .exit(); // Prints to stderr, exits 2, never returns.
```
