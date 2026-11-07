// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package rule contains the shared rule envelope used by Flow rule families.
//
// The package owns lifecycle metadata, selectors, bindings, and kind validator
// registration. Rule-kind semantics live in child packages such as
// internal/rule/event or internal/rule/operation so the shared framework stays
// independent from event and operation execution details.
//
// Main concepts:
//
//   - Rule is the generic persisted envelope. It owns lifecycle metadata and
//     carries the kind-specific Definition JSON.
//   - Selector carries coarse, JSON-serializable lookup fields. Rule-kind
//     packages define the supported keys and semantics.
//   - Binding attaches a rule to a supported scope, optionally with selector
//     refinements.
//   - Registry stores KindValidator implementations and validates rules by
//     checking the shared envelope first, then delegating to the registered
//     kind-specific validator.
package rule
