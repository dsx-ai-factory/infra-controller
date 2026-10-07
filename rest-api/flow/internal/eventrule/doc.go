// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package eventrule defines the in-memory domain contracts for policy-driven
// event handling. It deliberately contains no event transport, database
// representation, inventory lookup, store implementation, or action executor.
// Those boundaries convert their own representations into these types and
// implement the store capabilities declared by this package.
//
// # Events
//
// Envelope is the normalized event accepted by processing. Its EventKey
// combines the registered source name with the source-provided or
// collector-derived event identity. The resulting key identifies one event
// across delivery retries and is the mandatory deduplication identity.
// Resource is the caller-supplied reference to the Flow rack or component
// concerned by the event and may contain only an ExternalID.
// ResolvedResource separately contains the canonical ID, rack ID, and component
// type established by processing so enrichment never mutates the envelope.
//
// Envelope.Payload is opaque JSON whose schema is selected by Envelope.Type.
// The generic domain validates only that the payload is valid JSON. The child
// package that owns an event type defines its typed payload and strict
// encode/decode helpers when that event carries type-specific information. An
// event that needs only the common envelope fields leaves Payload nil.
//
// # Rules and policies
//
// Rule represents either a configurable persisted rule associated with scopes
// such as site or rack, or an immutable code-defined fallback. Rule.Origin
// distinguishes those sources; both use stable UUID identities and the same
// Policy, so processing executes them identically.
//
// EventType belongs to Rule because it controls rule selection. Policy contains
// only the actions considered for an accepted event. A resolver is expected to
// select one effective rule before
// its policy is evaluated, for example rack override, then site rule, then
// built-in fallback.
//
// Persisted rule creation accepts RuleCreate, containing caller-owned metadata,
// event type, and policy. The manager generates the rule identity, fixes its
// origin to persisted, and creates it disabled. Disabled-by-default creation
// prevents a partially configured rule from becoming effective while callers
// establish its bindings; activation requires an explicit SetEnabled
// operation. Persistence supplies timestamps and returns the canonical stored
// rule.
//
// # Bindings and stores
//
// Binding associates a persisted rule and its immutable event type with a
// Scope. A site scope has no ID; a rack scope carries the rack UUID. Bindings
// are separate from rules so one persisted policy can be managed independently
// of where it applies.
//
// A persisted rule without bindings is inactive. A rule may have multiple rack
// bindings, but a site-bound rule cannot have any other bindings. At most one
// binding applies to an event type at a resolved site or rack scope. Disabled
// persisted rules are ignored during resolution while their bindings remain in
// place. Built-in rules have no persisted bindings and act as implicit
// site-wide fallbacks. Effective resolution proceeds from rack to site to
// built-in.
//
// The store interfaces are divided by capability. RuleReader provides common
// reads. BuiltInRuleReader adds unique built-in lookup by event type.
// RuleStore adds persisted-rule lifecycle operations. Rule identity and event
// type remain stable; metadata, actions, and enabled state are updated
// independently. BindingStore separately manages bindings and scope
// lookup. A resolver composes the rule and binding stores to select rack, site,
// and built-in precedence. Implementations own persistence,
// concurrency, and transaction semantics.
//
// # Actions
//
// Each Action has a stable name within its policy, an optional Condition, and a
// concrete ActionSpec. Conditions intentionally support only demonstrated event
// properties: severity and component type. Values within one condition field
// use OR semantics, while different fields use AND semantics. An empty
// condition applies to every event.
//
// Task actions use a named TargetStrategy rather than an arbitrary inventory
// query; actions without targets use TargetStrategyNone. Target resolution and
// side effects occur outside this package. If a target strategy resolves no
// resources, the processor should record the action as skipped and must not
// submit a task.
//
// # Validation boundaries
//
// Collectors validate Envelope before handing it to processing. Store
// implementations and APIs validate Rule and Binding when converting from
// their persistence or transport representations. Event-family code strictly
// validates any typed payload, and executors validate runtime requirements that
// depend on inventory or external services.
package eventrule
