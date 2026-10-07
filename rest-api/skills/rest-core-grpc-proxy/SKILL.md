---
name: rest-core-grpc-proxy
description: Build or migrate infra-controller REST API endpoints that call on-site NICo Core through the generic Core gRPC proxy. Use when working on REST-to-Core operations, ExecuteCoreGRPC, grpcproxy, forge.Forge methods, creating new proxied REST endpoints, or migrating bespoke workflows to the gRPC proxy.
---

# REST Core gRPC Proxy Skill

Use this guidance when building or converting `infra-controller` REST API endpoints that need to call on-site NICo Core through the generic Core gRPC proxy.

## Current Proxy Contract

- Cloud helper: `rest-api/api/pkg/api/handler/util/common/grpcproxy.go`, `ExecuteCoreGRPC`.
- Shared contract: `rest-api/common/pkg/grpcproxy/grpcproxy.go`, with `grpcproxy.Request` and `grpcproxy.Response`. Core and Flow share every layer of the proxy; the backend is named by `grpcproxy.Core`, and each layer keeps a per-backend wrapper only because Temporal dispatches on the registered workflow and activity names.
- Site workflow/activity: `InvokeCoreGRPC` and `InvokeCoreGRPCOnSite`.
- Site Core invocation: `CoreGrpcClient.InvokeJSON`.
- Temporal transport payload: protojson, so non-secret request fields and responses remain readable in Temporal UI.
- Secret transport payload: selected top-level protojson fields are redacted from `RequestJSON` and carried separately in `EncryptedSecrets`. `grpcproxy.RedactSecrets` / `grpcproxy.MergeSecrets` do the splitting and are shared with the Flow proxy.
- Final site-to-Core call: normal binary gRPC. The JSON step is only the generic Temporal payload representation.

## Before Coding

Confirm these details before editing:

- REST operation path, method, auth role, org/site scoping, request model, response model, and expected status code.
- Target Core method, usually `/forge.Forge/<Method>`, and whether it is unary. The proxy does not support streaming methods.
- Core internal RBAC authorizes `SiteAgent` for the target method. The final
  Core call is made by the on-site `elektra-site-agent` service, not the cloud
  user.
- Typed protobuf request and optional typed protobuf response.
- Secret fields that must not appear in Temporal history. These must be top-level protojson field names such as `password`.
- Whether the REST operation maps to one Core call or must compose multiple
  calls to `ExecuteCoreGRPC`. A single REST handler may invoke the proxy helper
  more than once when the API operation requires multiple Core gRPC calls.
- Whether each Core operation is non-idempotent. The shared proxy workflow
  intentionally runs each activity once with no automatic retry.

## Implementation Workflow

1. Inspect the nearest existing REST handler and model patterns before editing.
2. Add or update the API model with validation, REST-to-proto mapping, and response shaping. Keep API compatibility and OpenAPI required/nullable semantics aligned with server validation.
3. Keep auth, tenant/org membership, site lookup, role checks, request validation, and REST semantics in the REST handler. The proxy is only the cloud-to-site transport.
4. Build the typed protobuf request before calling the proxy. Prefer generated protobuf types over maps or ad hoc JSON.
5. Call `common.ExecuteCoreGRPC(ctx, siteTemporalClient, fullMethod, reqProto, respProtoOrNil, siteIDSecretKey, secretFields...)`. Passing `secretFields` requires a non-empty `siteIDSecretKey`; the helper rejects the combination rather than send the fields unredacted.
6. Pass the site ID string as the secret key when redacting fields for a site-scoped call; the site-agent decrypts with the same site key.
7. Never log full request bodies when they can contain secrets. Log method, kind, site ID, or other non-secret metadata only.
8. Return a curated REST response. Do not expose Core protobufs or secret fields directly unless the API contract already does.
9. For a new public REST endpoint, register the route, update
   `rest-api/openapi/spec.yaml`, and regenerate SDK files if the repo workflow
   requires it. For an existing REST endpoint being migrated from a bespoke
   workflow to the generic proxy, keep the REST contract unchanged and do not
   touch OpenAPI or generated SDK files unless the public API actually changes.

## Tests To Add

- Model validation for required fields, enums, conditional requirements, and REST-to-proto mapping.
- Handler tests for auth/site validation, request normalization, `ExecuteCoreGRPC` invocation arguments, secret field names, status code, and password-free response.
- Route registration tests.
- OpenAPI/schema checks and generated SDK checks only for new endpoints or
  public API changes. Pure migrations from bespoke workflows to the generic
  proxy should not change the REST contract, OpenAPI, or generated SDK files.
- Proxy package tests only if changing shared proxy behavior, not for each new endpoint.

## Binary Protobuf Question

If someone asks why the proxy uses protojson instead of protobuf bytes:

- A binary payload is possible because the cloud handler already has a typed proto request and the site side resolves the method descriptor.
- The merged design chose protojson for the Temporal payload so non-secret fields remain readable during debugging, while selected secret fields are encrypted separately.
- Switching to bytes would reduce encode/decode overhead and lean harder on protobuf wire compatibility, but it would make Temporal history opaque and require a different redaction strategy for secret fields.
- Treat binary transport as a follow-up design change to the shared proxy, not as part of adding a normal REST endpoint through the existing proxy.

## Known Example

Use the BMC credential endpoint from PR 2477 as the reference pattern:

- Handler: `rest-api/api/pkg/api/handler/bmccredential.go`.
- Model: `rest-api/api/pkg/api/model/bmccredential.go`.
- Core method: `/forge.Forge/CreateCredential`.
- Secret redaction: `password`.
- Response omits password and returns only accepted metadata.
