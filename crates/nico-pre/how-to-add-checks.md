# How to add a check to nico-pre

For a developer who has been handed one check, or one checklist, to implement. Read
[`README.md`](README.md) first for the terms: site-def.yaml, register, checklist, check, gate.

## What a check is

One entry in a checklist's table and one row in the output. The row shows the check's id, the
expected condition, what the check observed, a status (✓ pass, ⚠ warning, ✗ fail, – skipped,
! could not run) and, when something is wrong, a fix the provider can act on.

```text
check                     expected                                      observed                              fix
✓ config.segments.admin   the admin segment fits the fleet: ...         10.10.60.0/24: 249 usable for 128 hosts, mtu 9000 within pathMtu 9000
```

The harness (`src/harness.rs`) owns everything around the check: it builds the context, calls
the function, times it, turns an error or a panic into a `!` row for that check alone, applies
`--only` and `--skip`, prints the table and writes the JSON result. A developer writes the
function and the table entry, and a test. Nothing else.

## Where the code goes

| You are adding | Where |
|---|---|
| A check to an existing checklist | The checklist's file: `src/checklists/config.rs`, `host.rs`, `build.rs`, `stage.rs`, `fabric.rs`, `platform.rs`, `install.rs`, `ingest.rs`. No new file. |
| Code several checks of one checklist share (a Redfish client, a TOML loader) | A sibling module the checklist file declares, for example `src/checklists/host/redfish.rs` with `mod redfish;` in `host.rs`. |
| A new checklist (a new stage) | A new file `src/checklists/<name>.rs` with its own `CHECKS` table; in `src/checklists/mod.rs` one `mod <name>;` line, one enum variant with its `name()`/`checks()` arms, and the variant added to `Checklist::ALL` (and its array length), which is what `nico-pre checks` and `CHECKS.md` enumerate; and one subcommand in `src/main.rs`. Rare; agree on it first. |

## The two things you write

### 1. The function

```rust
fn admin_segment(ctx: &Ctx) -> eyre::Result<Outcome>
```

Every check has this exact signature. `Ctx` and `Outcome` come from `crate::harness`.

### 2. The table entry

Find your check's id in the file's `CHECKS` table. It is registered already with `run: todo`,
which prints as ⚠ "not implemented yet". Point it at your function:

```rust
Check {
    id: "config.segments.admin",
    expected: "the admin segment fits the fleet: usable addresses beyond the reserved block \
               cover counts.hosts with headroom, and its MTU is within pathMtu",
    detail: "Reads: `network.segments.admin`, `network.pathMtu`, `counts.hosts`.\n\
             Pass: usable addresses at or above `counts.hosts` with 20% spare ...\n\
             Warn: under 20% spare ...\n\
             Fail: fewer usable addresses than hosts, or MTU above `pathMtu` ...\n\
             Skip: never.\n\
             Test: the example spliced one line at a time; cases: ...",
    // the registered spec, kept as is: a test checks that every entry carries all its labelled lines
    run: admin_segment,
},
```

- `id` is `<checklist>.<area>.<name>`, lower-case, dot-separated. It is stable: `--only`,
  `--skip` and the result files refer to it. Do not rename an id that has shipped.
- `expected` is the condition as the row prints it, one sentence, no trailing period. Write it
  so the provider understands the rule without reading code.
- `detail` is the spec of the check, and it is already written for every registered entry.
  One part per line, in this order, each starting with its label:
  - `Reads:` the site-def fields, collaterals, decisions and endpoints the check consults.
  - `Pass:` the condition and the observed text, with numbers.
  - `Warn:` (optional) and `Fail:` each condition, what `observed` names, and the fix text.
  - `Skip:` the decision that rules the check out, or `never`.
  - `Test:` the fixture and the minimum cases: the passing case, one per fail or warn branch,
    the skip.

  Implement what the spec says. If the spec turns out wrong or incomplete (a field that does
  not exist, a branch that cannot happen), change the spec in the same PR and say so in the
  description; the spec and the function must agree when the check lands. [`CHECKS.md`](CHECKS.md)
  renders the spec under the check's id; a test in `src/checklists/mod.rs` enforces the shape.

If your check is not in the table yet, add an entry in the position where the row should print.
A test in `src/checklists/mod.rs` fails the build when an id is duplicated, carries the wrong
checklist prefix or has an empty `expected`.

## How you get the data

Nothing is global. The harness builds one `Ctx` per run and passes it to every check:

| Field or method | What it gives you |
|---|---|
| `ctx.sd` | The parsed site-def.yaml as Rust structs, `crate::sitedef::SiteDef`. One struct per YAML table, camelCase keys as snake_case fields: `ctx.sd.network.segments.admin.prefix`, `ctx.sd.counts.hosts`, `ctx.sd.hosts.site_controllers`. The gate has already proven the file parses, prefixes are aligned and disjoint, gateways lie inside, node ids are contiguous. Do not re-check those. |
| `ctx.resolve(path)` | A collateral path from site-def.yaml (`ctx.sd.collaterals.core_values`) made absolute, relative to the directory site-def.yaml was read from. You open and parse the file yourself; the harness reads no collateral. |
| `ctx.decided(id, option)` | True when the effective decisions hold that option, case-insensitive short name: `ctx.decided("dpu-in-the-site-controllers", "no DPU")`. Effective means after the register's implication rules ran, so an implied answer counts. `ctx.decision(id)` returns all values of a multi-select. |
| `ctx.evidence` | `Option<&Path>`: a directory the operator asked for with `--evidence`, to keep raw output (a Redfish response, a command transcript) next to the result. Absent most of the time; never required. |
| `ctx.site_def_path` | Where site-def.yaml was read from. Rarely needed directly; `resolve` uses it. |

A check reads what `ctx.sd` names, plus the external inputs its own spec's `Reads:` line
names (the `PATH` for a tool probe, a variable the spec calls out such as `NICO_RMS_CHART`,
a file a collateral points at). It never looks at argv or at other environment variables,
never prints, never writes anywhere but `ctx.evidence`.

For reaching the outside world, a BMC, a registry, a cluster, the check does the I/O itself
with the crate's dependencies. Shared clients belong in a sibling module as described above,
not in the harness.

## What you return

| Return | Meaning | Row |
|---|---|---|
| `Ok(Outcome::Pass(observed))` | The site agrees with `expected`. `observed` is the value you saw, with numbers: "249 usable for 128 hosts". | ✓ |
| `Ok(Outcome::Warn { observed, fix })` | NICo tolerates it; the provider should know. Never blocks. | ⚠, exit 1 |
| `Ok(Outcome::Fail { observed, fix })` | Blocks the next step until fixed. | ✗, exit 2 |
| `Ok(Outcome::Skip(why))` | A decision rules the check out for this site: "no DPU in the site controllers". | –, counts as pass |
| `Err(e)` via `?` | The check could not reach a verdict: a file it needs is unreadable, the TOML does not parse, a tool it relies on is missing. Not a verdict on the site. When reachability is the very thing being checked (a BMC that does not answer in `stage.reach.bmcs`), return `Outcome::Fail` instead: that is the verdict. | !, exit 3 |

Rules of thumb:

- `observed` names the actual values, both sides when two things disagree: "datacenterAsn
  4266030000 in site-def, 4266030001 in ytl.yaml".
- `fix` is an action the provider can take, not a restatement of the failure.
- Several problems in one check: join them with "; " into one `Fail`, as `admin_segment` does.
  Do not return the first one you find.
- Use `?` for anything that stops the check from running. Never `unwrap()` on site data and
  never `panic!`; a panic is caught and reported, but it is a bug.
- Do not loop over the checklist's other concerns. One check, one condition.

## How you test it

Next to the function, a `#[cfg(test)]` module that runs it against `examples/site-def.yaml`
with one value spliced, and asserts the outcome. The example passes every check, so every
failure case is the example with one line changed. The template is `admin_segment_tests` in
`src/checklists/config.rs`:

```rust
const EXAMPLE: &str = include_str!("../../examples/site-def.yaml");

/// The example with one line changed, run through the check.
fn outcome(from: &str, to: &str) -> Outcome {
    assert!(EXAMPLE.contains(from), "splice target `{from}` not in the example");
    let text = EXAMPLE.replacen(from, to, 1);
    let sd = crate::sitedef::parse(&text).unwrap();
    let dec = crate::gate::effective_decisions(&sd, Register::builtin());
    let ctx = Ctx { sd: &sd, site_def_path: Path::new("examples/site-def.yaml"), decisions: &dec, evidence: None };
    admin_segment(&ctx).unwrap()
}
```

Then a local case table, one row per distinct behaviour: the example as is passes, one case
per failure branch, one per warning branch. Each case asserts the `Outcome` variant and a
fragment of `observed`, so a wrong message is caught too. Smallest set that exercises
different code paths; no Cartesian products of inputs.

A check that reads a collateral tests against a fixture file under `examples/` or a temporary
directory; one that talks to a BMC tests against `bmc-mock` profiles; one that talks to a
cluster against kind. Say in the PR which fixture the test uses.

## Running it

```bash
cargo test -q -p nico-pre                                                     # unit tests
cargo run -q -p nico-pre -- config --site-def crates/nico-pre/examples/site-def.yaml \
    --only config.segments.admin                                              # your row alone, after the gate
cargo run -q -p nico-pre -- config --site-def <a real site-def.yaml> -o json   # the JSON the result file holds
cargo make clippy && cargo make check-format-nightly                          # before the PR
```

`CHECKS.md` is generated from the tables and a test fails while it is stale:

```bash
cargo run -q -p nico-pre -- checks --format md > crates/nico-pre/CHECKS.md
```

The team repository's `sites/ytl/test-checklists.sh` runs every checklist against the ytl site
definition and asserts each registered id appears exactly once. Add your id to its table when
you add an entry to `CHECKS`.

## Checklist for the PR

- [ ] Table entry points at the function; `expected` reads as a rule; `detail` (the spec)
      agrees with what the function does, every branch it names implemented.
- [ ] `CHECKS.md` regenerated.
- [ ] Function returns `Skip` when a decision rules it out, `?` for things that stop it, one
      `Fail` or `Warn` with every problem it found and a fix.
- [ ] Test module with the cases the spec's `Test:` line names, at least.
- [ ] `cargo test`, `cargo make clippy`, `cargo make check-format-nightly` clean.
- [ ] Id added to `sites/ytl/test-checklists.sh` if the entry is new.
