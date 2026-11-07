# carbide-lints

This is a custom rustc driver that emits customized lints for things we want to
enforce in the carbide repo, which are carbide-specific and thus shouldn't be
upstreamed into something like clippy.

The (currently) only lint here is `txn_held_across_await`, which enforces that
if any code is holding a sqlx::Transaction (or one of our wrappers around it),
that it can't `await` on anything exept for function calls where the same
Transaction is being passed as a reference.

Run the lints with `cargo make carbide-lints` from the repo root. The relevant
nightly rust will be installed automatically.

## Why the lint?

`txn_held_across_await` enforces that you don't begin a transaction and then go
off and do unrelated work with the transaction open. This is a major
performance issue if the work you're doing takes a long time (like a libredfish
call to an unresponsive BMC with a 60 second timeout.)

This causes a postgres connection slot to be occupied but idle, counting
against the overall connection limit, and if any writes have happened inside
the transaction, the corresponding row can be locked, preventing other
transactions from making progress.

Perf testing against a version of carbide with all long-running transactions
removed allows postgres connection limits to be set to something reasonable
(equal to the number of cores), even when ingesting thousands of hosts. Whereas
when using long-running transactions, the postgres connection limits need to
match the number of simultaneously-ingested hosts, which on my machine needed
48GB of RAM when ingesting 5000 mock hosts.

### What's "unrelated work"?

Unrelated work is awaiting any async function where you're not passing the
transaction as an argument. This means that when holding a transaction, you can
make *database calls*, but you can't do anything else.

This works well: If you're passing the transaction to another function, then
it's *that* function's job to prove that it's not doing any unrelated work.
Eventually, either the txn is used (which makes it safe), or some unrelated
work is awaited on (which makes it not), and the function that is doing the
unrelated work is the one that gets the lint.

## How does it work?

From the point of view of cargo, it's a simple binary installed as
`cargo-carbide-lints`, which you can then call as `cargo carbide-lints`.

From there, it acts as a shim around `rustc`, by calling
`rustc_driver::run_compiler` inside main.rs.

It passes to `run_compiler` a special `rustc_driver_impl::Callbacks`
implementation that overrides the `mir_borrowck` "query" with our own
implementation in `borrowck_shim.rs`. (A "query" is an extension point of the
rust compiler that can be overridden by custom drivers.)

With `BorrowckShim` part of the compilation pipeline, when called, it calls the
original `mir_borrowck` query, but before returning it, calls the
`TxnHeldAcrossAwait` lint, which analyzes expressions to find any transactions
improperly held across await points.

### Implementation details

`TxnHeldAcrossAwait` takes advantage of the fact that rust async functions are
"desugared" into a coroutine, alongside a hidden enum with variants for each
"chunk" of the coroutine, where a chunk is the part between each await point.

The coroutine variants are inspected to see:

- If the variant contains a `sqlx::Transaction` (or other types we've listed)
- If the parameter list of the original function contains a `sqlx::Transaction`
  (this is necessary because a borrowed input paramter may not be listed in
  some variants. But we still consider it a violation because the transaction
  is still "live")
- Whether the "cause" of the variant (the `await` call) is a function which
  passes the txn local or not

Then for any `sqlx::Transaction` local which is being held, uses dataflow
analysis to see if the local is initialized or not at the await point. This
avoids false positives when a transaction is committed via `txn.commit()` (or
rolled back with `.rollback()`) but still in scope.

## Why is it not in `crates/*`, nor in the workspace Cargo.toml?

This lint requires nightly rust, and needs to be built out-of-band of the rest
of the crates in this repo. Including it in `crates/*` would make cargo try to
build it with the rest of the workspace, which would fail. If we fixed it so
cargo didn't build just this crate, it would just create confusion.

## Why does this use nightly rust?

You need nightly rust to use the `rustc_private` feature, which is needed for
writing custom rustc drivers.

## Why doesn't this use something like [`dylint`](https://github.com/trailofbits/dylint)?

dylint, and also clippy in general, use rust's [`LateLintPass`][latelintpass]
trait for custom lints. Unfortunately, `LateLintPass` is run after certain
necessary information (mainly [LocalDecl.local_info][local-info]) is
[cleared][cleared-locals] from scope, which makes it impossible to check if a
local variable is initialized or not (moved) at a given await point. Prior
versions of this lint used dylint/LateLintPass, and trying to gather Move data
would crash the compiler due to the necessary information being cleared.

Without this info during the lint, there would be false positives when a
transaction is committed (and thus moved) prior to an await point.

[latelintpass]: https://doc.rust-lang.org/beta/nightly-rustc/rustc_lint/trait.LateLintPass.html
[cleared-locals]: https://github.com/rust-lang/rust/blob/a463b0e2ee07b232221afd8475bc8f4d7d474609/compiler/rustc_mir_transform/src/lib.rs#L673
[local-info]: https://github.com/rust-lang/rust/blob/a463b0e2ee07b232221afd8475bc8f4d7d474609/compiler/rustc_middle/src/mir/mod.rs#L961
