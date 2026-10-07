// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import "errors"

// ErrTerminalProcessing identifies an event-processing failure that cannot
// succeed by delivering the same envelope again.
var ErrTerminalProcessing = errors.New("terminal event processing error")

// ErrBuiltInRuleImmutable identifies an attempted mutation of a code-defined
// rule exposed through the unified read API.
var ErrBuiltInRuleImmutable = errors.New("built-in event rule is immutable")

// ErrBindingConflict identifies a binding that conflicts with an occupied
// scope or with the owning rule's site-versus-rack hierarchy.
var ErrBindingConflict = errors.New("event rule binding conflicts with existing bindings")

// ErrBindingNotFound identifies an event type and scope with no persisted
// binding.
var ErrBindingNotFound = errors.New("event rule binding not found")

// ErrInvalidRuleInput identifies a rule or binding mutation rejected before
// persistence because the requested domain configuration is invalid.
var ErrInvalidRuleInput = errors.New("invalid event rule input")

// ErrRuleTargetNotFound identifies a concrete rack or component that cannot
// be resolved for effective-rule lookup.
var ErrRuleTargetNotFound = errors.New("event rule target not found")
