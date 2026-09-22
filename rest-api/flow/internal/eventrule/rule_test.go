// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestPolicy_Clone(t *testing.T) {
	policy := Policy{Actions: []Action{{Name: "noop", Spec: &Noop{Reason: "test"}}}}
	cloned := policy.Clone()
	require.Equal(t, policy, cloned)
	require.NotSame(t, policy.Actions[0].Spec, cloned.Actions[0].Spec)
}

func TestScopeHasID(t *testing.T) {
	require.False(t, Scope{Type: ScopeTypeSite}.HasID())
	require.True(t, Scope{Type: ScopeTypeRack, ID: uuid.New()}.HasID())
}

func TestBindingValidateRequiresEventType(t *testing.T) {
	binding := Binding{
		ID:     uuid.New(),
		RuleID: uuid.New(),
		Scope:  Scope{Type: ScopeTypeSite},
	}

	require.ErrorContains(t, binding.Validate(), "event rule binding event type")

	binding.EventType = "test.event"
	require.NoError(t, binding.Validate())
}
