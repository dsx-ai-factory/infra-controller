# REST API agent guidance

Applies to `rest-api/`. Follow the root [AGENTS.md](../AGENTS.md), relevant
[STYLE_GUIDE.md](../STYLE_GUIDE.md) sections, and the shared
[Engineering Guidelines](../CONTRIBUTING.md#engineering-guidelines) for scope,
reuse, evidence, and verification.

NICo REST is the Go management backend for multi-tenant bare-metal inventory,
provisioning, and lifecycle operations. It coordinates Core through cloud/site
Temporal workflows and an on-site agent, and includes IPAM, authentication,
native PKI, component managers, and the `nicocli` CLI/TUI. APIs, configuration,
and features are under active development and may change between releases.

## Find the owning code

Read [go.mod](go.mod) for the Go version and dependencies. The import path is
`github.com/NVIDIA/infra-controller/rest-api`; the repository is
`dsx-ai-factory/infra-controller`. The stack uses Echo v4, PostgreSQL with pgx v5
and Bun, Temporal, Connect-RPC/gRPC, buf, OpenTelemetry/Prometheus/Sentry,
Keycloak/JWT/service accounts, and Make. Test dependencies include testify,
go-sqlmock, testcontainers-go, and gomock.

| Concern | Source |
| --- | --- |
| Routes, handlers, REST models | `api/pkg/api/routes.go`, `api/pkg/api/handler/`, `api/pkg/api/model/` |
| Authentication and request audit redaction | `auth/`, `api/pkg/middleware/audit.go` |
| DB models, transactions, migrations | `db/pkg/db/model/`, `db/pkg/db/tx.go`, `db/pkg/migrations/`, `db/cmd/migrations/` |
| Shared utilities | `common/` |
| Native PKI and site management | `cert-manager/`, `site-manager/` |
| Cloud/site workflows and workers | `workflow/`, `site-workflow/`, `site-agent/` |
| Core/Flow proto copies and generated clients | `proto/core/`, `proto/flow/` |
| Flow, IPAM, component managers | `flow/`, `ipam/`, `nvswitch-manager/`, `powershelf-manager/` |
| Published schema and Go clients | `openapi/spec.yaml`, `sdk/standard/`, `sdk/simple/` |
| CLI/TUI | `cli/` |
| Deployment, images, charts | `deploy/`, `docker/`, `../helm/rest/`, `temporal-helm/` |
| Build and CI | `Makefile`, `../.github/workflows/rest-ci.yml`, `../.github/workflows/rest-lint-and-test.yml` |

## Build and verification

Run the commands below from `rest-api/`. From the repository root,
`make rest-api/<target>` delegates to the same target; `rest-build`, `rest-test`,
`rest-lint`, and `rest-fmt` are aliases for the main workflows.

```bash
make build                 # Static linux/amd64 binaries
make nico-cli              # Install nicocli into the configured INSTALL_DIR
make docker-build          # Production images
make docker-build-local    # Local development images with public base images
```

### Testing

Use Makefile test targets rather than bare `go test`; they start required
services. Tests needing a database use testcontainers-go or the Makefile-managed
PostgreSQL container. Its defaults are `postgres:14.4-alpine` on port `30432`.

| Targets | Setup and package scope |
| --- | --- |
| `test-api`, `test-auth`, `test-db`, `test-flow`, `test-ipam`, `test-nvswitch-manager`, `test-powershelf-manager`, `test-workflow` | Call `ensure-postgres` before testing their packages |
| `test-site-agent` | Starts mock Core/Flow gRPC servers; tests only `site-agent/pkg/components/...` with `CGO_ENABLED=1` and `-race` |
| `test-common`, `test-cert-manager`, `test-site-manager`, `test-site-workflow`, `test-proto`, `test-sdk-simple`, `test-mcp` | Other package targets; see `Makefile` for their scope |
| `test` | Runs its listed targets, not every package; CLI and the nested standard-SDK module are outside this target |

Without mock servers, site-agent component tests retry on a `40s` backoff until
the `10m` timeout. Database fixtures reset shared tables, so run packages
serially; pass `-p 1` whenever running multiple database-backed packages directly.
Targets generally use `-p 1`, and some use `-race`.

```bash
make test                  # Aggregate test target
make test-api              # Example package target
make postgres-up           # Start test PostgreSQL
make postgres-down         # Stop it
make ensure-postgres       # Start if absent and wait until ready
make migrate               # Apply migrations to the test database
```

Use testify `assert`/`require`. Organize tests by the production function or
method: one top-level `Test<Function>` or `Test<Receiver>_<Method>`, with scenarios
in a table executed as named `t.Run` subtests. Do not create scenario-specific
top-level tests. Before requesting review, group every changed Go function's
scenarios this way; separate scenario tests are a review failure even when the
suite passes. Use the root rules to choose which regression cases are needed.

### Linting and formatting

Follow standard Go conventions. Formatting is enforced in CI; linting uses
`golangci-lint` with the v2 `.golangci.yml` configuration and `revive` with
`.revive.toml`.

```bash
make fmt-go                # Runs go fmt, then checks the Git diff
make lint-go               # go vet, golangci-lint, revive
```

Before review of a Go change, run `make lint-go` and inspect all analyzer output.
Its golangci-lint invocation uses `--issues-exit-code 0`; also run the pinned
`go tool golangci-lint run` with changed package patterns and without that
override, and fix every finding in those packages. `make fmt-go` checks the whole
Git diff, so intentional or pre-existing edits can make it fail. Use
`go fmt ./...` to apply formatting.

### Protobuf generation

```bash
make core-proto             # Copy Core protos, format, and generate Go clients
make core-protogen          # Generate from the existing Core proto copies
make flow-proto             # Copy Flow protos and generate Go clients
make flow-protogen          # Generate from the existing Flow proto copies
```

Core sources are `../crates/rpc/proto/`; Flow sources are `flow/proto/v1/`.
Change those sources and regenerate instead of editing copies in `proto/`.

### Local development and hooks

| Command | Purpose |
| --- | --- |
| `make kind-reset` | Reset the cluster, infrastructure, and Helm deployment |
| `make kind-reset-kustomize` | Reset with Kustomize deployment |
| `make kind-redeploy`, `make helm-redeploy` | Rebuild and restart for iteration |
| `make kind-status`, `make kind-logs`, `make kind-verify` | Inspect pods, API logs, and health |
| `make kind-down` | Tear down the cluster |
| `make pre-commit-install` | Install pre-commit and TruffleHog hooks |
| `make pre-commit-run` | Scan all files for secrets |
| `make pre-commit-update` | Update hook versions |

## Go authoring conventions

- Split assignment and condition into separate statements; do not use
  initializer clauses such as `if err := operation(); err != nil`.
- Document intentionally omitted code that readers would otherwise expect.
  Add TODO comments for deferred features or nuances.
- Avoid types that merely wrap an array. A composite with domain
  validation or conversion behavior is a named type as specified below.
- Prefer inline logic when a one-use utility with arbitrary arguments would
  make its caller harder to follow.

### Range-based iteration

Prefer range-based iteration over three-clause loops:

- Counts: `for i := range n` or `for range n`, including `v.NumField()` and
  `v.Len()` reflection walks.
- Reverse slices: `for i, v := range slices.Backward(s)`; use only `i` when
  taking `&s[i]` or mutating an element.
- Forward slices: `for i, v := range s`, `for _, v := range s`, or index-only
  `for i := range s` when the body needs `&s[i]`.

Keep three-clause loops for byte-wise string scans, indices advanced or changed
by the body, lookahead/lookbehind algorithms, and one-based or offset-start
ranges. Explain the reason when it is not obvious. String range advances over
UTF-8 code points, not every byte; assigning a range index does not change the
iterator's next index. Leave generated loops to their generator. For a backward
byte search for a delimiter, use `strings.LastIndexByte`, as in
`powershelf-manager/pkg/db/model/types.go`'s `IPAddr.Scan`.

## REST endpoint contracts

Before adding code, find the nearest endpoint family in
[the route list](api/pkg/api/routes.go) and follow its structure:

- DB resources use `handler/<resource>.go`, `model/<resource>.go`, and matching
  tests under `api/pkg/api/`. VPC, Instance Type, SSH Key Group, Network Security
  Group, and Operating System are reference families.
- Site operations pass `scp *site.ClientPool` into the handler constructor and
  resolve the site before Temporal/Core calls. References include VPC, Subnet,
  Expected Machine/Rack/Switch, Flow Rack/Tray, Machine Validation, and Tenant
  Identity.
- Flow inventory/task models use Flow request/response protobufs. Keep target
  helpers with the model or handler that owns the REST contract; Rack, Tray, Task,
  and Task Rule are references. Use the generic proxy as described below.

Use these naming conventions consistently, substituting the resource name:

| Operation | Handler prefix | Request DTO |
| --- | --- | --- |
| GET one / many | `GetInstanceHandler` / `GetAllInstanceHandler` | Typically no request object |
| POST one / many | `CreateInstanceHandler` / `BatchCreateInstanceHandler` | `APIInstanceCreateRequest` |
| PUT | `CreateOrUpdateInstance` | `APIInstanceCreateOrUpdateRequest` |
| PATCH one / many | `UpdateInstance` / `BatchUpdateInstance` | `APIInstanceUpdateRequest` |
| DELETE | `DeleteInstance` | `APIInstanceDeleteRequest` |

Use `APIInstance` for full details and `APIInstanceSummary` for a nested summary.
Name files, handlers, DTOs, and OpenAPI schemas after the REST resource and
operation, not an internal Core method or authorization concept.

### Routes and models

- Inspect existing routes and attributes before choosing names. Published names
  require deprecation before replacement. POST creates, PATCH updates, and PUT
  is for endpoints supporting both create and update. End PATCH/GET/DELETE
  resource routes with the ID unless the resource has no unique identifier.
- Use query parameters for filters/read-only selectors, preferably GET lists.
  When a JSON body exists, put IDs such as `siteId` in it and validate them on
  the DTO. List responses include `X-Pagination`; reuse `pagination.PageRequest`,
  `common.ValidateKnownQueryParams`, and `common.GetSearchQuery`.
- Register normal routes in `NewAPIRoutes` under `/org/:orgName/<apiName>`.
  System and public discovery routes are exceptions. Public `.well-known/*`
  routes belong in `NewWellKnownRoutes`, which is mounted before auth.
- Place static routes (`/batch`, `/stats`, `/validation`, `/power`, `.well-known/*`)
  before parameterized routes that could capture them. Update the route-family
  count and `assertRouteExists` in `api/pkg/api/routes_test.go`; add
  `assertRouteBefore` for a potentially shadowed static route.
- Give every API model attribute a structured schema; do not expose schemaless
  JSON. Use camelCase JSON names and PascalCase API constants, such as
  `SiteWideRoot` and `BMCRoot`. External standards such as JWKS can require
  snake_case. Format exact enum literals in prose as code.
- Follow the root REST response-field policy, including the `omitempty`
  exceptions for date-gated deprecated attributes and opt-in `includeRelation`
  expansions.
- Optional PATCH strings that support clearing use `*string`: omission or
  `null` preserves the value, and `""` clears it. Do not add custom presence
  tracking for this contract.
- Adding a field to a resource with single and batch endpoints requires the
  complete surface: create/update DTOs, validation, handlers, DAO inputs,
  persistence, OpenAPI, SDK, and tests. Preserve endpoint-specific nullability
  in constructors or distinct models, and test unavailable values at every
  affected response boundary.

### Handler flow and responses

1. Call `common.SetupHandler`, defer the span end, check the request user, then
   check org membership and roles with neighboring authorization helpers. Use
   `common.IsProviderOrTenant` to obtain Provider/Tenant objects.
2. Bind the DTO and call `Validate` before conversion or side effects. Keep
   syntax and cross-field rules in `Validate`; keep RBAC, tenant privileges,
   ownership, site readiness/configuration, and database lookups in the handler.
3. For a disruptive machine operation, check `Machine.IsAssigned` or its
   equivalent and reject unless the product requirement explicitly permits a
   Provider Admin override. Encode that policy, including any required
   acknowledgement; see `api/pkg/api/handler/machinepower.go`.
4. Return curated REST models, not DB records, internal protobufs, or
   secret-bearing requests. Successful POST/PUT responses return the updated
   object without credentials. Persist a contract-critical value returned by an
   authoritative create in the request transaction before returning 2xx; assert
   response and database state independently, not a cache or later reconciliation.
5. Use the established success codes below and `APIError` for errors. Keep raw
   database errors in contextual server logs, not the response payload.

| Operation | Success status |
| --- | --- |
| GET / PATCH | 200 |
| POST | 201 for a created object; 202 for async work with no object; otherwise 200 |
| PUT | 201 for creation; otherwise 200 |
| DELETE | 204 for immediate deletion; 202 for asynchronous deletion |

Use `APIMessageResponse` for a 202 response with no object. A 204 response has no
body. Document the real completion signal and terminal states for async work;
a resource read or poll is not completion unless the contract exposes that
transition.

| Error | Status |
| --- | --- |
| Data validation | 400 |
| Object named in URL path not found | 404 |
| Object named in query/body not found | 400 |
| Existing resource prevents creation | 409 |
| Insufficient permission | 403 |
| Unexpected failure | 500 |

### Endpoint tests

Tests must exercise the changed surface:

- Models: `Validate`, query validation, enums, list-level constraints, and
  `ToProto`/`FromProto` mappings.
- Handlers: auth role, org membership, missing user, invalid IDs, ownership/site
  checks, request validation, status, and response fields.
- Lists: paging inputs and the `X-Pagination` header.
- Site/Core/Flow handlers: workflow/proxy arguments, workflow ID inputs, secret
  field names, relevant timeout behavior, and no secrets in responses or logs.
- Include route tests and OpenAPI checks in endpoint changes; regenerate SDKs
  in the same change when required by the repository workflow.

## CLI and sensitive data

- A generated CLI path test must assert a non-nil leaf `Action`, not just path
  presence. After changing OpenAPI tags or operation IDs, build the command
  tree and run affected `nicocli ... --help` paths to catch alias collisions.
- Tenant-owned TUI pickers explicitly request the current tenant and reject
  every row with another tenant ID. Test a dual-role response containing
  provider-owned and other-tenant resources.
- A site-wide picker that clears a narrower scope invalidates filtered caches
  before fetching and after restoring scope on every exit. Test the scoped case.
- When an option consumes a separate value, test that another option token is
  rejected instead of consumed as that value.
- Restore terminal state before another selector; propagate restoration errors.
  Return one non-nil error unchanged and use `errors.Join` only when both the
  operation and restoration fail. Only a direct cancellation sentinel counts
  as successful cancellation. Test normal PTY, cancellation, and restoration
  failure paths.
- Table-test shell quoting for empty input, whitespace, quotes, backslashes,
  control characters, non-ASCII text, and shell metacharacters.
- Mutation success output rejects malformed JSON, `null`, empty objects, and
  missing display fields. Use server-returned values, not request/discovery
  values; test a response that differs from the pre-request value.
- For configuration overrides, copy the complete configured object and overlay
  only explicitly set flags. Test no override, one override, and unset configured
  fields so unrelated options survive.
- Before using an auth/protocol convenience API, read its current encoding
  contract. Exercise delimiters, percent/plus signs, and other reserved
  characters through the real encode/decode boundary.
- Add sensitive request-field names to `obfuscateFields` in
  `api/pkg/middleware/audit.go`, following its lowercase-key convention. Log
  identifiers, method names, kinds, and site IDs rather than credential-bearing
  payloads. Operator examples use supported prompts, stdin, or protected files
  instead of credentials in command arguments.

## Validation and conversion

### Validation before mapping

Use ozzo rules for simple validation and DTO `Validate` methods for cross-field
rules. All `Validate` errors have type `validation.Errors`.

- Prefer built-ins such as `Required`, `Min`, `Max`, `In`, `Length`, `By`, and
  `Each` in `ValidateStruct`; add custom helpers only when no built-in fits.
- A list with element and cross-element rules uses a named slice with its own
  `Validate()`: call `validation.Validate(s, validation.Each())`, then enforce
  uniqueness, ordering, or other list rules. The parent uses
  `validation.Field(&parent.Xs)`; ozzo discovers `Validatable.Validate`.
- Put cross-field checks in named receiver methods and use
  `validation.By(t.validateX)`, not anonymous closures. See
  `APIMachineCapability.validateDeviceType` and `validateInactiveDevices`.
- Reusable `validation.By` helpers match `func(value interface{}) error` and
  assert the concrete type internally, as `util.ValidateLabels` and
  `util.ValidateNameCharacters` do.

`ToProto` does not return errors. Call `Validate` and handler context checks
first; width bounds, enum validity, and structural rules belong there, not in
mapping. Primitive converters in `common/pkg/util/converter.go`, such as
`IntPtrToUint32Ptr`, are trusted casts: no errors or warnings, with bounds checked
upstream.

### Conversion ownership and presence

- Use receiver `FromDBModel`/`ToDBModel` methods between API and DB models.
  Prefer `FromDBModel` over new `NewX` conversion constructors as models change.
- Define symmetric entity `ToProto`/`FromProto` methods together on the DB model.
  `FromProto` mutates the receiver without returning an error. A nil entity
  proto is a no-op; preserve required IDs on missing/invalid input, and clear
  optional pointers on omitted or invalid values. `Vpc.FromProto` and its
  `NVLinkLogicalPartitionID` handling are the reference.
- Request-to-proto conversion belongs on the API request DTO. Overlay its
  explicitly supplied fields on entity-derived values, including an explicit
  zero/clear value even if the merged entity has the same zero. Preserve nil
  versus supplied-zero presence on the wire.
- Assemble path and body context on the DTO before conversion. Set path-derived
  fields such as `MachineID` with `json:"-"` in the handler, then call
  `req.ToProto()` rather than passing hidden path context to `req.ToProto(id)`.
- Requests with no API body, such as deletion by path or entity maintenance,
  stay on the entity as `ToDeletionRequestProto`/`ToMaintenanceRequestProto`.
- Pass non-model side inputs explicitly, preferably in a nearby `XCredentials`
  struct that explains why they are not persisted.
- Name reusable sub-message converters after their result, such as
  `ToTargetSpec`/`ToFilters`; see `RackFilter`/`APIRackGetAllRequest`.
- Proto-based API constructors such as `NewAPITray`/`NewAPIRack` return nil for
  nil input, otherwise initialize the model and call `FromProto`. Build simple
  response models inline; reserve helpers for real construction logic. Shared
  handler helpers return `*cutil.APIError` when status/message must survive.

`Vpc` and `APIVpcCreateRequest`/`APIVpcUpdateRequest` demonstrate entity/request
conversion. `InstanceType`, `AttachCapabilities`, `APIMachineCapabilities`, and
`APIMachineCapability` demonstrate typed collection validation and conversion.

### Named types

A map, slice, or composite representing a domain concept with conversion needs
must own that behavior through named-type methods, not free conversion functions.
Use typed string enums with receiver conversion methods too. Entity fields that
round-trip with protos use the named type; CreateInput/UpdateInput fields may
retain the primitive type until they need its methods.

Leaf `FromProto` uses a pointer receiver and mutates in place. Nil input clears
the receiver, unlike entity-level nil input. `db/pkg/db/model/common.go`'s
`Labels` is the map reference: `Labels.ToProto()` returns `[]*corev1.Label`,
`Labels.FromProto(nil)` sets nil, and a nonnil empty list produces an empty map.
Call `entity.Labels.FromProto(proto.Metadata.GetLabels())`; the proto getter is
nil-safe.

`MachineCapabilityType` and `MachineCapabilityDeviceType` own their enum mapping;
the database representation remains a string. Unknown enum values map to the
empty string with a warning so `MachineCapability.Validate` can reject them;
the defined unspecified device type maps to empty without a warning.
`MachineCapability.FromProto` preserves a supplied `DeviceType` pointer even when
conversion yields empty, so validation can reject unsupported values; only an
omitted pointer becomes nil. See `db/pkg/db/model/machinecapability.go`.

## Database transactions

Handler database writes use `cdb.WithTx` or `cdb.WithTxResult`, not manual
Begin/Commit/Rollback. Keep these contracts:

1. `WithTxResult[T]` returns one value; `WithTx` returns an error and uses outer
   variables for outputs. Do not mix `WithTxResult` with outer partner outputs.
2. Keep pure validation reads outside the transaction. Keep reads inside when
   they are under an advisory lock, drive a write from locked state, or need
   read-your-writes consistency.
3. For read-then-mutate flows vulnerable to concurrent writers passing the same
   check, acquire `tx.TryAcquireAdvisoryLock` with
   `cdb.GetAdvisoryLockIDFromString(key)` at the top of the closure. Re-read
   decision inputs under the lock and remove redundant preflight checks when
   the locked check covers them.
4. Inside the closure return `cutil.NewAPIError`; after it unwinds use
   `common.HandleTxError`, or `cutil.NewAPIErrorResponse` for direct outer error
   responses. When moving reads outside, change their error constructor too.
5. Pass nil as error-response `data`, not raw database errors. Log the underlying
   error with the contextual logger. Split assignment and condition as required
   by the Go conventions above.

### Synchronous workflow timeouts

Reuse the nearest handler's workflow ID, timeout, task queue, and
`common.UnwrapWorkflowError` protocol. Keep activity/RPC deadlines shorter than
workflow execution, shorter than caller wait, and within the HTTP write deadline.
Test every affected boundary; a caller timeout alone does not bound started work.

For bespoke handlers that start and await a workflow inside a transaction,
terminate on timeout only after the transaction unwinds. Preserve the
`timeoutResp` pattern:

1. Declare an outer `var timeoutResp func() error`.
2. In the timeout branch, explicitly capture `timeoutCause := wferr`, set the
   closure to call `common.TerminateWorkflowOnTimeOut`, and return an API error
   to roll back the transaction. Prefer this helper to inline termination code;
   pass the exact workflow name used by `ExecuteWorkflow`, not an action label.
3. After `WithTx`, send real transaction-helper errors through `HandleTxError`.
   Only the timeout's APIError with a non-nil `timeoutResp` falls through to
   that deferred response. Use the gate
   `!errors.As(err, &apiErr) || timeoutResp == nil` when `err != nil`.
4. Call a non-nil `timeoutResp` after the transaction has unwound.

The generic gRPC proxy is an exception: it returns its timeout error without
terminating the workflow. Do not call `TerminateWorkflowOnTimeOut` after it;
termination cannot stop the in-flight RPC and can free a deterministic ID for a
duplicate call. See `api/pkg/api/handler/util/common/grpcproxy.go`.

References: `handler/instance.go` under `api/pkg/api/` demonstrates the
transaction flows; `handler/sshkeygroup.go` demonstrates validation reads outside
the transaction; `handler/nvlinklogicalpartition.go` demonstrates the deferred
timeout response. Retain the explicit capture and exact-name rules above even
where an older handler does not yet follow them.

### Bulk PATCH updates

A shared `UpdateMultiple` column list writes each listed column for every row.
Do not add a PATCH-preserved field to that shared list when some rows omit it:
carry per-row presence into the SQL or group rows by identical column sets. For
flat JSONB patches, prefer the Site config `column = column || ?::jsonb` pattern;
an empty object is a no-op. Create omission stores the zero/empty value; update
omission must retain the stored value.

Add a mixed-batch test with set and omitted values and assert omitted rows retain
their prior value. `ExpectedMachine.UpdateMultiple` handles scalar
`bmc_ip_address` and JSONB `host_lifecycle_profile` this way.

## Site RPCs and inventory workflows

For Core proxy endpoints follow
[the Core proxy skill](skills/rest-core-grpc-proxy/SKILL.md). Curated unary
`forge.Forge` calls use `common.ExecuteCoreGRPC` with a typed request, not a new
bespoke workflow. BMC Credential is the reference for auth, site resolution,
secret redaction, and password-free responses.

For Flow endpoints follow
[the Flow proxy skill](skills/rest-flow-grpc-proxy/SKILL.md). Use
`common.ProxyFlowGRPC` or `common.ExecuteFlowGRPC` in helpers returning an error;
use `ProxyFlowGRPCWithSecrets` when redaction is needed. Callers supply the
workflow ID and conflict policy. Do not add bespoke workflows per method.
Preserve the skill's multi-release registration, migration, and retirement order.

When adding or renaming Site inventory workflows, update their registration in
`workflow/cmd/workflow/main.go` and `inventoryWorkflowTypes` in
`workflow/pkg/activity/site/retention.go`. Missing types retain successful runs
for the full Site namespace retention instead of
`temporal.siteNamespace.successfulInventoryRetention`.

## OpenAPI and generated clients

Update `openapi/spec.yaml` in the same change when published routes are added or
modified; intentionally unpublished system/public discovery routes are exempt.
Keep operation IDs, summaries, handler constructors/godoc, and SDK-facing names
aligned. Descriptions explain the REST contract, not internal gRPC details.
Every new OpenAPI object needs a realistic example backed by a contract,
configuration, or test fixture. Keep IDs, enums, nullability, and timing semantics
valid under OpenAPI lint.

```bash
make lint-openapi
make preview-openapi       # Redoc at http://127.0.0.1:8090
make generate-sdk
```

The generator version and checksum are pinned in `Makefile`; the verified jar
is downloaded to `.tools/`. Do not bypass the pin with a separate installation.
A version bump belongs in its own change with regenerated `sdk/standard/`, since
it can rewrite files unrelated to the spec edit. Change source/templates and
regenerate standard SDK output; its handwritten helpers, such as `pagination.go`,
are separate. Do not give new helpers generated `api_`/`model_` prefixes.
`sdk/simple/` remains handwritten. See [OpenAPI development](openapi/README.md).

## Commits, PRs, and CI

- Use `git mv` for tracked-file moves. Explain non-obvious tradeoffs in commit
  messages; wrap prose, use semantic titles such as `feat:`, `fix:`, or `chore:`,
  backtick short types/code, and indent full code lines.
- All commits require DCO sign-off (`git commit -s`); missing sign-off blocks
  merging.
- Write PR descriptions for readers without prior context: explain why, link
  related issues, and keep one change per PR. Do not add unused code unless
  the PR is too large to review otherwise. Ensure all CI checks pass before
  requesting review, including the Go lint and table-test requirements above.

[REST CI](../.github/workflows/rest-ci.yml) declares current triggers and jobs:
manual runs, `main` and pull-request branches, and supported release tags. It
coordinates lint/tests, generated-file checks, builds, secret scanning, images,
and Helm checks. See [lint/test CI](../.github/workflows/rest-lint-and-test.yml)
for package selection, PostgreSQL setup, formatting, linters, OpenAPI, and
regeneration checks rather than assuming the local aggregate target covers all
CI packages. Current release conditions live in the workflows.

Further procedures: [README](README.md), [contribution guide](../CONTRIBUTING.md),
[CLI guide](cli/README.md), [deployment quickstart](deploy/README.md), and
[detailed installation](deploy/INSTALLATION.md).
