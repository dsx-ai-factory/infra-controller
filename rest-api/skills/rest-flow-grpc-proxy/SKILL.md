---
name: rest-flow-grpc-proxy
description: Build or migrate infra-controller REST API endpoints that call on-site Flow through the generic Flow gRPC proxy. Use when working on REST-to-Flow operations, ExecuteFlowGRPC, grpcproxy, v1.Flow methods, or migrating bespoke TaskRun/Flow workflows to the gRPC proxy.
---

# REST Flow gRPC Proxy Skill

Use this guidance when building or converting `infra-controller` REST API
endpoints that need to call on-site Flow through the generic Flow gRPC proxy.

## Current Proxy Contract

- Cloud helper: `rest-api/api/pkg/api/handler/util/common/grpcproxy.go`,
  `ExecuteFlowGRPC`.
- Shared contract: `rest-api/common/pkg/grpcproxy/grpcproxy.go`, with
  `grpcproxy.Request` and `grpcproxy.Response`. Flow and Core share every layer
  of the proxy; the backend is named by `grpcproxy.Flow`, and each layer keeps a
  per-backend wrapper only because Temporal dispatches on the registered
  workflow and activity names.
- Site workflow/activity: `InvokeFlowGRPC` and `InvokeFlowGRPCOnSite`.
- Site Flow invocation: `FlowGrpcClient.InvokeJSON`.
- Temporal transport payload: protojson, so non-secret request fields and
  responses remain readable in Temporal UI.
- Secret transport payload: selected top-level protojson fields are redacted
  from `RequestJSON` and carried separately in `EncryptedSecrets`.
  `grpcproxy.RedactSecrets` / `grpcproxy.MergeSecrets` do the splitting and are
  shared with the Core proxy.
- Final site-to-Flow call: normal binary gRPC. The JSON step is only the
  generic Temporal payload representation.

## Critical Difference From CoreProxy

`ExecuteCoreGRPC` always generates a fresh workflow ID
(`core-grpc-{Method}-{uuid}`) and does not set `USE_EXISTING`.

`ExecuteFlowGRPC` requires the caller to supply:

1. `workflowID` — deterministic for read/list dedup, fresh UUID for creates.
2. `conflictPolicy` — the two travel together. A deterministic ID takes
   `WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING` so identical in-flight requests
   coalesce onto one Flow call; a fresh ID takes
   `WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED`, matching `ExecuteCoreGRPC`, whose
   IDs are always fresh.

`common.ProxyFlowGRPC` dispatches and renders a proxy failure as an Echo response.
Use `ExecuteFlowGRPC` directly only in helpers that hand the error back to a
caller instead of rendering a response, as `resolveTrayIDsBySlot` does; return
the `*cutil.APIError` unwrapped so the status the proxy chose survives.

Kinds of ID derivation that must stay in the handler, using the TaskRun
endpoints as the worked example:

- A read whose response shape depends on a query flag must encode that flag, so
  `GetTaskRun` needs `includeStats` in the ID
  (`task-run-get-{runID}-{true|false}`). Attaching to an execution
  started with the other value would return stats presence that contradicts the
  request.
- A list must hash its query parameters (`QueryParamHash`) into the ID, so two
  different filters never coalesce.
- A create must never coalesce at all, so `CreateTaskRun` uses a fresh UUID
  (`task-run-create-{uuid}`) and `USE_EXISTING` can never match.

Keep the descriptive IDs derived by handlers; do not add a transport prefix.
The `flow-grpc-` namespace was a temporary migration safeguard and was retired
with the bespoke Flow workflows.

## Timeout Ladder

The ladder is bounded from the outside, not chosen from the on-site budget:
`server.WriteTimeout` is a deadline on the response write, so a handler that
answers later than that cannot deliver its answer. Keep
`ActivityStartToCloseTimeout` < `grpcproxy.WorkflowExecutionTimeout` <
`cutil.WorkflowContextTimeout` < `server.WriteTimeout`, currently
40s < 45s < 50s < 60s. `Test_ProxyTimeoutsFitWriteTimeout` in
`api/internal/server` guards the outer bound. Raising the on-site budget means
raising the server and load-balancer timeouts first.

Because the workflow timeout sits inside the caller's context timeout, a timeout
that Temporal itself reports comes from an execution that has closed. The ladder
does not cover the other way a caller loses its result: `wfCtx` derives from the
request context, so a client disconnect or a shorter upstream deadline can end
the wait while the execution runs on. `wfCtx.Err()` is the only evidence that
this happened, which is why a `context.DeadlineExceeded` in the workflow result
alone does not classify as it — it can come from inside the execution.

## Before Coding

Confirm these details before editing:

- REST operation path, method, auth role, org/site scoping, Flow enablement
  check, request model, response model, and expected status code.
- Target Flow method, usually `/v1.Flow/<Method>`, and whether it is unary.
  The proxy does not support streaming methods.
- Typed protobuf request and optional typed protobuf response.
- Workflow ID derivation and conflict policy for this call.
- Secret fields that must not appear in Temporal history (top-level protojson
  field names).
- Whether the call fits in the proxy's budget. The activity is cut off at
  `grpcproxy.ActivityStartToCloseTimeout`, currently 40s. The bespoke workflows
  declared 2 minutes, and bring-up and firmware 5 minutes, but every Flow caller
  bounded itself with the 50s `cutil.WorkflowContextTimeout`, so no declared
  budget was reachable. Migrating still narrows the window: a Flow call that took
  41-49s used to succeed and now fails.
- Whether the call tolerates losing an activity retry. `InvokeFlowGRPC` runs the
  activity with `MaximumAttempts: 1`, so a transient Flow error surfaces to the
  client instead of being retried. Bespoke workflows that allowed a second
  attempt lose it on migration, and the retry policy lives in the site workflow,
  so making it configurable takes another agent release to take effect.

## Implementation Workflow

1. Keep auth, tenant/org membership, site lookup, Flow enablement, role checks,
   request validation, and REST semantics in the REST handler.
2. Build the typed Flow protobuf request before calling the proxy.
3. Call `common.ExecuteFlowGRPC(ctx, siteTemporalClient, fullMethod, reqProto,
   respProtoOrNil, workflowID, conflictPolicy, siteIDSecretKey, secretFields...)`.
   Passing `secretFields` requires a non-empty `siteIDSecretKey`; the helper
   rejects the combination rather than send the fields unredacted.
4. Return `StatusGatewayTimeout` as it comes. Do not call
   `TerminateWorkflowOnTimeOut`. When Temporal reports the timeout the execution
   has already closed, and terminating a closed execution fails and reports a
   data desync that did not happen. When the caller stopped waiting instead, the
   execution may still be running, and terminating it still does not help: the
   activity does not heartbeat, so Temporal cannot deliver cancellation while
   its Flow RPC is in progress, and terminating only the workflow discards the
   result without stopping that RPC. For a deterministic ID with `USE_EXISTING`
   it also frees the ID, so a retried request starts a duplicate mutation
   instead of attaching to the call already in flight. An abandoned execution
   stays bounded by the 45s and 40s timeouts.
5. Return a curated REST response. Do not expose Flow protobufs or secret
   fields directly unless the API contract already does.
6. For a new public REST endpoint, register the route and update OpenAPI. For a
   migration from a bespoke workflow to the generic proxy, keep the REST
   contract and the parameter-derivation rules for the workflow ID unchanged.
   Use a temporary, non-colliding ID namespace while the bespoke and proxy
   workflows coexist, then restore the original ID when the bespoke workflow is
   retired.

## Rollout Requirement

A workflow type is known only to the workers that registered it. The cloud API
and each site's agent ship as separate Helm releases (`nico-rest` and
`nico-rest-site-agent`) and cannot be upgraded atomically, and Temporal accepts
a submission for a type no worker knows: the execution is created, no worker can
advance it, and the caller sees only a timeout after
`grpcproxy.WorkflowExecutionTimeout`.

So a handler may not start dispatching through `InvokeFlowGRPC` in the same
release that first registers it on the site agent. Register the proxy first,
wait for every site to run that agent, and switch handlers in a later release.
Retire the workflow a migration replaces in a third release, once no supported
cloud release still submits it. Let executions submitted by the migration
release reach their bounded completion before restoring the original workflow
IDs. During a rolling upgrade, old and new cloud replicas use the temporary and
original IDs respectively, so an identical request can execute once under each
name; both IDs dispatch the same generic proxy workflow at this stage.

When adding a Flow endpoint, reach for the proxy: there is no longer a bespoke
Flow workflow to copy, and adding one would reintroduce the per-method
registration this replaced.
