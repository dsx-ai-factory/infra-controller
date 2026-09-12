# AGENTS.md — admin-cli

Guidance for AI coding agents (and humans) working on the admin CLI
(`nico-admin-cli`). This file is symlinked to `CLAUDE.md`; edit
`AGENTS.md` and the other name follows.

See the repo-root [`AGENTS.md`](../../AGENTS.md) for build/test/lint
commands and overall conventions. This file documents admin-CLI–specific
conventions: the per-command help **examples** under `--help` (below),
and how we surface **errors** to the user ("Error messages" at the end).

## Command help examples (`after_long_help`)

Every leaf subcommand carries a worked `EXAMPLES:` section, rendered by
clap's `after_long_help`. It appears at the bottom of `--help` (the long
help; `-h` shows only the summary). The goal is that a reader who runs
`nico-admin-cli <path> --help` sees concrete, copy-pasteable
invocations covering the realistic ways to use that command — not just a
list of flags.

### The convention

Attach `#[command(after_long_help = "...")]` to the clap `Args`/`Opts`
struct (or enum) that backs the subcommand:

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

Formatting rules, all load-bearing:

- Open with the literal `EXAMPLES:` header, then a blank line.
- Each example is a **description line ending in `:`**, then the command
  on the next line indented **four spaces** and prefixed with `$ `.
- One blank line between examples; one trailing blank line before the
  closing `"`.
- The leading `\` after the opening quote swallows the first newline so
  the block starts cleanly at `EXAMPLES:`.
- Always use the real binary name `nico-admin-cli` and the **kebab-case
  command path** as clap renders it (see "Getting the command path"
  below).
- For commands too long for one source line, break with a trailing `\`
  inside the string literal. clap collapses the wrapped string back to a
  single rendered line, so the example still copy-pastes as one command.
  **Indent each continuation line four spaces** to align it under the `$`
  prompt. Rust's `\`-newline escape strips that leading whitespace, so the
  indent is invisible in the rendered help — it is purely for source
  readability, but keep it consistent:

  ```rust
  #[command(after_long_help = "\
  EXAMPLES:

  Peer two VPCs:
      $ nico-admin-cli vpc-peering create 12345678-1234-5678-90ab-cdef01234567 \
      abcdef01-2345-6789-abcd-ef0123456789

  ")]
  ```

- Escape any literal `"` inside the string as `\"` (e.g. quoted
  `--description \"Front-end subnet\"`).

### Realistic placeholder values

Use believable, consistent placeholders rather than `<ID>` angle
brackets — they read better and copy-paste into a real shell shape:

| Kind             | Placeholder                                |
| ---------------- | ------------------------------------------ |
| UUID / resource id | `12345678-1234-5678-90ab-cdef01234567` (use `abcdef01-2345-6789-abcd-ef0123456789` for a second) |
| Tenant org id    | `fds34511233a`                             |
| BMC / host IP    | `192.0.2.10` (TEST-NET-1; host BMC `192.0.2.20`), with port as `192.0.2.10:22` |
| Username         | `admin`                                    |
| Password         | `mypassword` (use `mynewpassword` for a password being *set*) — keep it obviously a placeholder, never something that reads like a real secret |
| MAC address      | `00:11:22:33:44:55`                        |
| SKU id           | `DGX-H100-640GB`                           |
| File / dir paths | `./skus.json`, `/path/to/tpm-ca.der`       |

Where a field is an enum, use a **real variant** (`internal-maintenance`,
`nvidia`, `ready`), not a made-up token — read the `ValueEnum` /
`possible values` to pick one.

### Deciding what goes in EXAMPLES

The flag list already documents *what each option is*. Examples exist to
show *how the command is actually used*. Derive them from the struct's
fields, not from imagination:

1. **The happy path first.** One example of the simplest, most common
   invocation (often "list all" or the single required positional).
2. **One example per meaningful axis of variation**, where an axis is a
   distinct way to drive the command:
   - each **selector / filter** that changes *what* is acted on
     (`--vpc-id`, `--contains`, `--tenant-org-id`, a positional id vs.
     no id);
   - each **mode** in a mutually-exclusive set — surface both sides
     (`--pause` *and* `--resume`; "fill missing" vs `--replace-all`);
   - **important optional flags** that change behavior
     (`--force`, `--delete-interfaces`, `--print-only`, `--id` override).
3. **Cover both "all" and "one"** for show/list commands (list
   everything, then show a single entity by id).
4. **Show destructive / forceful flags explicitly** so readers see how to
   invoke them deliberately (`--force`, `--replace`, `--replace-all`).
5. **Stop there.** Don't enumerate every flag combination — pick
   representative ones. Two to six examples is the normal range; a
   single-purpose command may have just one.

Let the field declarations drive coverage: positionals (required vs
`Option`), `ArgGroup`s, `conflicts_with`, and `ValueEnum`s each map
naturally to an example or a chosen value.

### Getting the command path right

The path in the example must match what clap actually parses. Check the
parent `enum Cmd` and any attributes before assuming the name:

- Enum variants are kebab-cased by default (`ShowMachines` →
  `show-machines`), but verify — some enums set `rename_all`.
- `#[clap(name = "refresh")]` renames a command (`RefreshEndpoint` is
  invoked as `refresh`, not `refresh-endpoint`).
- `visible_alias` / `alias` add alternatives but the canonical name is
  what you should write.
- Nested groups (`#[clap(subcommand)]`) add a path segment
  (`site-explorer get-report endpoint`).

### Placement for tricky clap shapes

A block can live on the command's argument struct/enum **or directly on
the enum variant** that defines the subcommand. Both render on that
subcommand's `--help`. Pick whichever gives an accurate, per-command
example:

- **Struct/enum that is the command's own argument type** — the default.
  Put the block there (as in every `Args` example above).
- **Variant-level `#[command(after_long_help = "...")]`** — attach the
  attribute to the *variant* in the subcommand enum rather than to a
  payload struct. This is the right tool for the two cases below:
  - **Unit variants** (`ForceOff` → `force-off`): they have no payload
    struct, so a variant-level block is the only place to put an example.
    But don't reflexively add one — see "When a unit variant is worth an
    example" below.
  - **A payload struct shared by two or more variants**
    (`ChangeUefiPassword(UefiPassword)` *and*
    `ClearUefiPassword(UefiPassword)`; `SetBootOrderDpuFirst` *and*
    `IsBootOrderSetup`, both `SetBootOrderDpuFirstArgs`): a block on the
    shared struct would render the *same* text — with the wrong command
    name — under every variant. Put a distinct block on **each variant**
    instead, and leave the shared struct bare.
- **Type alias** (`pub type Args = ShowSkuOptions;`): put the attribute
  on the aliased struct itself (`ShowSkuOptions` in `common.rs`). It
  renders for the command that uses the alias directly.
- **Flattened newtype** (`#[clap(flatten)] pub inner: ShowSkuOptions`):
  a flattened struct contributes *fields only* — its command-level help
  is ignored. Put the `EXAMPLES:` block on the **outer newtype**, and do
  not rely on a block on a struct that is only ever flattened.
- **Nested subcommand enum**: put a block on the enum for the group's own
  `--help` (covering its variants), and a block on each variant's struct
  (or variant) for that variant's `--help`.

A variant-level block sits between the variant's doc comment and the
variant itself:

```rust
    /// Change UEFI password
    #[command(after_long_help = \"\\
EXAMPLES:

Change the UEFI password:
    $ nico-admin-cli redfish ... change-uefi-password --current-password X --new-password Y

\")]
    ChangeUefiPassword(UefiPassword),
```

### When a unit variant is worth an example

A no-argument subcommand (`get-power-state`, `boot-hdd`, `on`,
`thermal-metrics`, …) is already documented by its doc-comment `about`
plus the credential pattern shown in the parent command's top-level
block. A per-variant `EXAMPLES:` block that just repeats
`... --password mypassword <verb>` adds noise, not signal. So **do not
add a block to a trivial unit variant.**

Add one only when the example teaches something the `--help` page
wouldn't otherwise show, e.g.:

- a **hidden alias** declared with `#[clap(alias = "...")]` (as opposed
  to `visible_alias`, which clap already prints). `force-off`,
  `force-restart`, `graceful-restart` and `graceful-shutdown` each carry
  a hidden alias (`off`, `reset`, `restart`, `shutdown`), so their block
  surfaces it — that's the only place the reader learns the short form
  exists;
- a non-obvious value format, ordering constraint, or prerequisite that
  the flag/about text doesn't capture.

The same "does the example add information?" test from "Deciding what
goes in EXAMPLES" governs here — it just resolves to *zero* examples for
the common trivial case.

### Verifying the rendered output

Build, then render the long help and read the bottom:

```bash
cargo build -p nico-admin-cli
# Capture to a file — the sandbox has a pipe-drop bug, so `... | grep`
# may silently show nothing even when the block is present.
cargo run -q -p nico-admin-cli -- <command path> --help > tmp/help.txt 2>&1
sed -n '/EXAMPLES/,$p' tmp/help.txt
```

Confirm the wrapped (`\`-continued) commands collapsed to single lines
and that the command path matches the real subcommand name.

## Generated reference docs (man pages and `docs/manuals/nico-admin-cli`)

Two hidden subcommands turn the live clap tree into documentation, so the
reference can't drift from the code. Both are `hide = true` (they don't show
in `--help`) and write to a directory you pass with `--out-dir`:

- `generate-man` renders a roff man page per command/subcommand via
  `clap_mangen` (one file each, named by the full path, e.g.
  `nico-admin-cli-vpc-show.1`). Run it with `cargo make gen-man`.
- `generate-cli-docs` builds the `docs/manuals/nico-admin-cli` markdown
  reference. It renders the same man pages, converts each to GitHub-flavored
  markdown with **pandoc** (the `pandoc` *binary* must be on `PATH` — it's
  installed in the CI build images), and reorganizes them into one directory per
  top-level command
  (`docs/manuals/nico-admin-cli/commands/<command>/<command>.md`, with each
  descendant beside it as `<command>-<sub>.md`). The man page's OPTIONS/SYNOPSIS
  give each page its per-flag detail; the `EXAMPLES:` block you write becomes
  that page's `## Examples`. Run it with `cargo make gen-cli-docs`.

The hand-authored overview (`docs/manuals/nico-admin-cli/README.md`) is
editorial and is **not** regenerated — leave it be.

### Categorizing a new command

Every visible top-level command must be assigned a domain (Hardware, Network,
Tenant, or Admin) in `crates/admin-cli/cli_domains.yaml` — that file is the
single source of truth for how the reference is grouped, parsed by
`domain_for_command` in `cfg/cli_options.rs`. When you add a top-level command,
add its rendered name (kebab-case, as clap prints it) under the right domain.
Two tests keep the YAML honest:

- `every_command_has_a_domain` fails if a visible command is missing from the
  YAML;
- `no_unknown_commands_in_domain_map` fails if the YAML names a command that no
  longer exists.

### Keeping `docs/manuals/nico-admin-cli` in sync

`docs/manuals/nico-admin-cli` is committed, so regenerate and commit it
whenever you change a command's help, examples, structure, or domain:

```bash
cargo make gen-cli-docs   # regenerate docs/manuals/nico-admin-cli (needs the pandoc binary)
```

CI runs `cargo make check-cli-docs`, which runs the two domain tests, then
regenerates and fails on any `git diff` in `docs/manuals/nico-admin-cli` —
so a forgotten regeneration (or an uncategorized command) is caught there.

## Error messages

This is a user-facing CLI, so errors should read like a tool talking to
an operator, not a Rust program dumping its guts. Two rules:

### Errors render as a message chain, not a stack trace

`main` installs color-eyre with both the source-location section and the
"run with `RUST_BACKTRACE`" footer turned off:

```rust
color_eyre::config::HookBuilder::default()
    .display_location_section(false)
    .display_env_section(false)
    .install()?;
```

So a failed command prints just its error and the cause chain — nothing
about which `.rs:line` propagated it. **Do not re-enable those sections.**
A backtrace is still captured when the user explicitly sets
`RUST_BACKTRACE=1`.

Because the location is gone, **the `.context()` chain *is* the error
message.** This makes the repo-wide "`.context()` before every `?`" rule
load-bearing here: write context that completes "while attempting to …"
so the rendered chain reads as a coherent story
(`Network error talking to BMC at … → error sending request → tcp connect
error → deadline has elapsed`).

### A bad invocation is a clap error, not an eyre error

A problem with *how the command was invoked* — a missing argument, a value
that should have been required — should look like every other clap error
(the red `error:` prefix, a `Usage:` line, `try '--help'`, and **exit code
2**), never a generic `eyre!` that exits 1 with no usage.

The first choice is to **let clap enforce it** by giving the arg a
non-`Option` type. A `String` (not `Option<String>`) field with no default is
required automatically, so clap renders it as mandatory in the `Usage:` line
(`--address <ADDRESS>`, unbracketed) on the parent *and* every subcommand —
that usage line is the requiredness signal, so you don't need a custom
`help_heading = "Required"` to advertise it (a heading only re-groups the
arg on the *parent* `--help` and shows nothing on subcommands; skip it).
Prefer the non-`Option` type over an explicit `required = true` on an
`Option<_>`: it removes the always-`Some` field, so the handler reads
`action.address` directly with no dead `None` branch (a `None` branch there is
unreachable and would have to raise a runtime `eyre!`, which is exactly the
exit-1 error this section forbids). But a required arg and `global` are
mutually exclusive (clap rejects a required `global` arg), and only `global`
propagates an arg into each subcommand's *option list*, so for a flag shared
across a family of subcommands you pick:

| approach | enforced by | flag listed in subcommand `--help` | `--foo` after the subcommand |
| --- | --- | --- | --- |
| required parent arg (non-`Option` type) | clap (auto) | usage line only (not the option list) | ✗ (must precede the subcommand) |
| `global` parent arg + hand-validation | **you** | yes, full option entry | ✓ |

redfish uses the **first row** — prefer clap-native enforcement whenever you
can live with its constraints:

```rust
// on RedfishAction: a non-Option String is required by clap automatically,
// so the handler uses `action.address` directly — no None branch to handle.
#[clap(long, help = "IP:port of machine BMC. Port is optional and defaults to 443")]
pub address: String,
```

```text
Usage: nico-admin-cli redfish [OPTIONS] --address <ADDRESS> <COMMAND>
```

With the first row clap does the work — no validation code in `main.rs`, and
a missing flag prints clap's own "the following required arguments were not
provided" (exit 2). Accept its two constraints: the flag must come *before*
the subcommand, and the requirement applies to **every** subcommand of that
parent — including ones that don't actually use it (redfish's `browse`
proxies through the API server and ignores `--address`, but is still
required to pass it). That uniformity was the accepted trade for clap-native
enforcement. (redfish keeps only `--address` required this way;
`--username`/`--password` are left optional.)

Fall back to the **second row** only when an arg genuinely must stay
`global` (positionable after the subcommand, or required for some
subcommands but not others). Then hand-validate at the dispatch site and
raise a clap error so it still looks native — never a runtime
`eyre!("Missing --foo")` deep in a handler:

```rust
use clap::CommandFactory;          // already imported in main.rs
use clap::error::ErrorKind;

CliOptions::command()
    .error(ErrorKind::MissingRequiredArgument, "…actionable message…")
    .exit();                       // prints to stderr, exits 2, never returns
```
