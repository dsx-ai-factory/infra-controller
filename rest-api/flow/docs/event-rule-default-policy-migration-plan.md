# Event Rule Default Policy Migration Plan

This implementation plan replaces the built-in leakage fallback with a normal
persisted policy and, when no Site binding exists, a Site binding. The migration
and runtime changes described here remain to be implemented. The related
[REST API proposal](event-rule-rest-api-proposal.md) describes policy and binding
operations; this plan defines their selection semantics after the migration.

## Target behavior

For `hardware.leak.detected`, select the first enabled policy in this order:

1. The source Rack's explicit binding.
2. The Site binding.
3. No policy: do not start policy actions.

There is no built-in selection tier or lookup by a reserved policy UUID. A
disabled Rack policy permits Site inheritance. A disabled or absent Site policy
does not prevent an enabled Rack policy from acting. An enabled Rack `NoAction`
policy remains an explicit decision that prevents Site inheritance.

The supplied leakage policy becomes an ordinary catalog entry. Operators can
enable, disable, bind, replace, and delete it through normal policy operations.
Enabling an unbound policy has no processing effect. Replacing its definition
through REST means creating a new policy and switching the binding; REST PATCH
continues to change only `enabled`. Deleting any bound policy returns `409`.

## One-time initialization

Always create the supplied policy during the versioned migration. Decide its
initial enabled state and binding from the presence of a Site binding for the
same event type:

| Configuration before migration | Supplied policy | Site binding after migration |
| --- | --- | --- |
| No Site binding | Enabled | Create a binding to the supplied policy |
| Site binding to an enabled policy | Disabled and unbound | Preserve the existing binding and policy |
| Site binding to a disabled policy | Disabled and unbound | Preserve the existing binding and policy |

Binding presence is the deciding factor, including when its referenced policy is
disabled. Preserve all Rack policies and bindings. Rack bindings alone do not
prevent creation of the Site default. Each Flow database represents one Site;
the binding uses the existing Site scope with a null `scope_id`.

Initialization runs once per database, through the migration ledger. Subsequent
starts and upgrades must preserve operator changes, including disabling the
supplied policy, replacing or removing its binding, and deleting it while
unbound. Removing a different Site binding must not activate the supplied policy
automatically. An operator who wants the unbound supplied policy must explicitly
enable and bind it.

## Seed identity and definition

Use the existing leakage fallback UUID,
`f34b87f7-cb1b-4b08-aa51-30c0b3b58680`, for the persisted policy. This keeps the
identity consistent with historical handling snapshots. The UUID does not confer
special behavior after migration.

Freeze the policy payload in the migration, matching
[`leakage.DefaultRule`](../internal/eventrule/leakage/leakage.go):

| Attribute | Value |
| --- | --- |
| Name | `Default leakage response` |
| Description | `Power off components affected by a detected hardware leak.` |
| Event type | `hardware.leak.detected` |
| Conditions | No additional restrictions |
| Action name | `power_off_affected_components` |
| Action | Submit a Task with the force-power-off operation |
| Readiness check | Override enabled |
| Target strategy | Affected components, with the existing leakage target resolution |
| Conflict strategy | Queue |
| Task description | `Leakage response` |
| Operation Rule UUID | Absent; use normal operation-rule resolution |

Preserve the remaining operation defaults, including the unset `Forced` flag;
do not infer extra flags from the force-power-off operation name. Encode the
payload using the stored policy format, rather than REST JSON or protobuf JSON.
A migration test must decode that frozen payload through the policy codec and
verify its meaning. Later changes to a Go default constructor must not alter an
already versioned seed.

## Database migration

Add a new timestamped migration under
[`internal/db/migrations`](../internal/db/migrations/). Do not modify an applied
migration or add a startup routine that recreates missing defaults. The existing
runner applies pending migrations and records their hashes inside a transaction;
use that ledger as the initialization marker, without a separate bootstrap table.

Perform these steps in the migration transaction:

1. Lock `event_rule_bindings` in `SHARE ROW EXCLUSIVE` mode before checking for
   a Site binding. This blocks concurrent binding writes while allowing ordinary
   reads, including writes from binaries that do not use advisory scope locks.
2. Check for a Site binding for `hardware.leak.detected`, regardless of the
   referenced policy's enabled state.
3. Insert the supplied policy into `event_rules`. Set `enabled` to true only if
   step 2 found no binding. Do not update any existing policy.
4. If no Site binding existed, insert one referencing the supplied policy.
   Generate its binding UUID using the existing database convention.
5. Let the runner record successful application and commit everything together.

The binding decision and insert must be protected by the same table lock. The
migration ledger lock serializes migrators, but does not serialize ordinary API
writes. Do not rely only on that lock or on a new advisory-lock convention that
older writers may not follow.

A preexisting row using the supplied UUID is an unexpected collision: fail and
roll back visibly. Do not silently skip or overwrite it with `ON CONFLICT`.
Any migration failure must prevent startup before event processing begins.

Treat this data migration as forward-only. The paired down migration required by
the runner must explicitly reject rollback, preserving the ledger and operator
data. Do not delete a policy that operators may have rebound, and do not remove
the ledger entry while retaining seed rows. Use a forward repair migration if
the seed requires correction.

## Runtime and API changes

Remove the built-in registry from policy resolution, catalog merging, lookup by
UUID, and mutation restrictions. Persisted policies supply all these operations.
The seeded policy has ordinary persisted origin and is not read-only. If the
REST `readOnly` attribute is retained, it returns false for this policy; no new
origin field or special seed endpoint is needed.

Separate supported-event-type validation from the existence of a built-in policy.
Deleting the supplied policy must not make `hardware.leak.detected` unsupported.
Keep action, condition, and target validation in their owning registries and
validators. Additional event types require explicit registration; any supplied
policies for them require their own versioned seed migration.

Update the resolver and manager scope reads to return no policy after exhausting
enabled Rack and Site bindings. Database errors and invalid references remain
errors; they must not become a successful no-policy result. Preserve atomic
binding replacement and the one-binding-per-event-type-and-scope constraints.

No new endpoint is required. Site-binding reads return the persisted assignment
or `404`. Catalog filtering and pagination operate only on persisted policies;
the seed matches `bound=true` only when explicitly bound. Update the REST
proposal, OpenAPI descriptions, Flow guide, architecture, and gRPC documentation
to remove the built-in fallback and immutable-policy exceptions. Regenerate
affected SDK and reference outputs from their sources.

## Processing, history, and deployment

Preserve the processor's no-policy behavior: return without creating an event,
handling execution, or Task. This differs from selecting an explicit `NoAction`
policy, which retains the selected policy and decision in normal handling data.
Any internal no-policy diagnostic must distinguish these outcomes without
inventing a policy UUID or a handling record. No history-schema change is needed.

Do not rewrite historical `applied_rule_id` values or captured effective policies.
Pending and retrying executions continue with their captured plans. Migration,
disable, unbind, and rebind do not cancel existing Tasks or replay previously
handled deduplication keys. An observation skipped because no policy exists has
no stored event; a later observation can be handled after a policy is assigned.

Keep migration execution before manager startup, ingestion, and scheduling, as
in [`service.New`](../internal/service/service.go). The seed preserves the
supplied response when no Site binding existed. An existing disabled Site policy
will stop inheriting the old built-in fallback after upgrade; this is an
intentional selection change that must be documented in release notes.

Database initialization supports concurrent older binding writers through its
table lock. Processing semantics still differ between binaries: older instances
retain their implicit fallback. Complete replacement of event-processing
instances before relying on disable, unbind, or delete to stop inherited actions.
Rolling back a binary restores its older selection semantics; it is not a way
to preserve the new no-policy behavior. Do not roll back the data migration.
Pause/resume mechanisms are outside this migration's scope.

## Implementation and verification

Implement the database seed first, then remove runtime fallback behavior and
special restrictions, then reconcile API and operator documentation. Inventory
and adapt existing tests rather than repeating the same policy-selection matrix
at every layer.

- Migration tests: the three initialization cases above, preservation of Rack
  bindings, codec decoding of the frozen seed, and complete rollback on a UUID
  collision. Use populated predecessor schemas where configuration exists.
- Concurrency test: a normal binding writer using predecessor SQL must serialize
  with seeding without overwriting an assignment or producing duplicate bindings.
- Restart tests: after successful migration, disabling, unbinding, rebinding, or
  deleting the supplied policy must survive a fresh startup without reseeding.
- Resolver and manager tests: enabled Rack precedes enabled Site; disabled Rack
  permits Site inheritance; absent/disabled Site ends with no policy; `NoAction`
  stops inheritance; deleting the seed does not unregister the event type.
- Processing tests: distinguish no policy from `NoAction`, preserve captured
  plans and historical snapshots, and retain deduplication behavior across changes.
- API tests: the seeded policy supports normal enable/disable and binding flows;
  bound deletion remains `409`; Site binding reads and catalog filters reflect
  persisted state with no synthesized fallback entry.

Run the relevant Make-managed Flow and API tests, lint changed Go packages, and
build the affected services. If OpenAPI changes, run `make lint-openapi`,
`make generate-sdk`, and the repository-root `make openapi-breaking`. Check and
render the changed documentation using the repository documentation workflow.
