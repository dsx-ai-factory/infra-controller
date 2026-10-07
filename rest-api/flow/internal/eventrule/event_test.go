// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"encoding/json"
	"strings"
	"testing"
	"time"

	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestNewEvent(t *testing.T) {
	now := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	definition := validEventDefinition()

	event, err := NewEvent(definition, now)

	require.NoError(t, err)
	require.NotEqual(t, uuid.Nil, event.ID)
	require.Equal(t, 1, event.Observations)
	require.Equal(t, now, event.CreatedAt)
	require.Equal(t, now, event.LastObservedAt)
	require.NoError(t, event.Validate())
}

func TestEventValidateDefinition(t *testing.T) {
	tests := map[string]struct {
		mutate  func(*Event)
		wantErr string
	}{
		"valid": {},
		"empty effective policy is valid": {
			mutate: func(event *Event) { event.EffectivePolicy = Policy{} },
		},
		"missing resource": {
			mutate:  func(event *Event) { event.Resource.ID = uuid.Nil },
			wantErr: "event resource id is required",
		},
		"missing applied rule": {
			mutate:  func(event *Event) { event.AppliedRuleID = uuid.Nil },
			wantErr: "applied event rule id is required",
		},
		"empty summary": {
			mutate:  func(event *Event) { event.Summary = "" },
			wantErr: "event summary is empty",
		},
		"summary too long": {
			mutate:  func(event *Event) { event.Summary = strings.Repeat("界", maxEventSummaryRunes+1) },
			wantErr: "exceeds 1024 characters",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			event := validEventDefinition()

			if test.mutate != nil {
				test.mutate(&event)
			}

			err := event.ValidateDefinition()

			if test.wantErr == "" {
				require.NoError(t, err)

				return
			}

			require.ErrorContains(t, err, test.wantErr)
		})
	}
}

func TestParseSeverity(t *testing.T) {
	tests := map[string]struct {
		value    string
		expected Severity
		wantErr  bool
	}{
		"unspecified": {expected: SeverityUnspecified},
		"info":        {value: "info", expected: SeverityInfo},
		"warning":     {value: "warning", expected: SeverityWarning},
		"critical":    {value: "critical", expected: SeverityCritical},
		"invalid":     {value: "urgent", wantErr: true},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			actual, err := ParseSeverity(test.value)

			if test.wantErr {
				require.Error(t, err)
				require.Equal(t, SeverityUnspecified, actual)

				return
			}

			require.NoError(t, err)
			require.Equal(t, test.expected, actual)
		})
	}
}

func TestEventKey_Validate(t *testing.T) {
	tests := map[string]struct {
		key     EventKey
		wantErr string
	}{
		"valid non-UUID source key": {
			key: EventKey{SourceName: "leakage", SourceKey: "rack/r1:occurrence-42"},
		},
		"missing source name": {
			key: EventKey{SourceKey: "event-1"}, wantErr: "event source name is empty",
		},
		"invalid source name": {
			key:     EventKey{SourceName: "Leakage", SourceKey: "event-1"},
			wantErr: "event source name",
		},
		"missing source key": {
			key: EventKey{SourceName: "leakage"}, wantErr: "event source key is empty",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := test.key.Validate()

			if test.wantErr == "" {
				require.NoError(t, err)

				return
			}

			require.ErrorContains(t, err, test.wantErr)
		})
	}
}

func TestEnvelopeValidatePayload(t *testing.T) {
	tests := map[string]struct {
		payload json.RawMessage
		wantErr string
	}{
		"valid payload": {
			payload: json.RawMessage(`{"value":42}`),
		},
		"invalid payload": {
			payload: json.RawMessage(`{"value":`),
			wantErr: "payload must be valid JSON",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			envelope := Envelope{
				Key:      EventKey{SourceName: "test", SourceKey: "event-1"},
				Type:     "test.event",
				Resource: Resource{Kind: ResourceKindRack},
				Payload:  test.payload,
			}

			err := envelope.Validate()

			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestEnvelopeAllowsUnspecifiedSeverity(t *testing.T) {
	envelope := Envelope{
		Key:      EventKey{SourceName: "test", SourceKey: "event-1"},
		Type:     "test.event",
		Severity: SeverityUnspecified,
		Resource: Resource{Kind: ResourceKindRack},
	}

	require.NoError(t, envelope.Validate())
}

func TestEnvelope_Clone(t *testing.T) {
	original := Envelope{Payload: json.RawMessage(`{"value":42}`)}

	cloned := original.Clone()
	cloned.Payload[2] = 'x'

	require.NotEqual(t, original.Payload, cloned.Payload)
}

func TestResourceIDMayBeUnresolved(t *testing.T) {
	resource := Resource{Kind: ResourceKindRack}

	require.Equal(t, uuid.Nil, resource.ID)
	require.NoError(t, resource.Validate())

	resource.ID = uuid.New()

	require.NoError(t, resource.Validate())
}

func TestResolvedResource_Validate(t *testing.T) {
	componentID := uuid.New()
	rackID := uuid.New()

	tests := []struct {
		name     string
		resource ResolvedResource
		wantErr  string
	}{
		{
			name: "component",
			resource: ResolvedResource{
				Kind:          ResourceKindComponent,
				ID:            componentID,
				RackID:        rackID,
				ComponentType: flowtypes.ComponentTypeCompute,
			},
		},
		{
			name: "rack",
			resource: ResolvedResource{
				Kind:   ResourceKindRack,
				ID:     rackID,
				RackID: rackID,
			},
		},
		{
			name: "rack ignores component type",
			resource: ResolvedResource{
				Kind:          ResourceKindRack,
				ID:            rackID,
				RackID:        rackID,
				ComponentType: flowtypes.ComponentTypeCompute,
			},
		},
		{
			name:     "kind required",
			resource: ResolvedResource{ID: componentID, RackID: rackID},
			wantErr:  "unknown resource kind",
		},
		{
			name:     "id required",
			resource: ResolvedResource{Kind: ResourceKindComponent, RackID: rackID},
			wantErr:  "resolved resource id is required",
		},
		{
			name:     "rack id required",
			resource: ResolvedResource{Kind: ResourceKindComponent, ID: componentID},
			wantErr:  "resolved resource rack id is required",
		},
		{
			name: "component type required",
			resource: ResolvedResource{
				Kind:   ResourceKindComponent,
				ID:     componentID,
				RackID: rackID,
			},
			wantErr: "resolved resource component type",
		},
		{
			name: "component type must be supported",
			resource: ResolvedResource{
				Kind:          ResourceKindComponent,
				ID:            componentID,
				RackID:        rackID,
				ComponentType: flowtypes.ComponentType("INVALID"),
			},
			wantErr: "unknown component type",
		},
		{
			name: "rack identities must match",
			resource: ResolvedResource{
				Kind:   ResourceKindRack,
				ID:     uuid.New(),
				RackID: rackID,
			},
			wantErr: "resolved rack resource id must equal rack id",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := test.resource.Validate()

			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)

				return
			}

			require.NoError(t, err)
		})
	}
}

func validEventDefinition() Event {
	return Event{
		Key:           EventKey{SourceName: "collector", SourceKey: "event-1"},
		Type:          "hardware.leak.detected",
		Resource:      ResourceIdentity{Kind: ResourceKindRack, ID: uuid.New()},
		AppliedRuleID: uuid.New(),
		EffectivePolicy: Policy{Actions: []Action{
			{Name: "noop", Spec: &Noop{}},
		}},
		Summary: "Leak detected",
	}
}
