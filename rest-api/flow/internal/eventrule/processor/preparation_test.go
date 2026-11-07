// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processor

import (
	"context"
	"errors"
	"fmt"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/location"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestPrepare(t *testing.T) {
	rackID := uuid.New()
	rule := &eventrule.Rule{ID: uuid.New()}
	storeErr := errors.New("rule store unavailable")
	tests := map[string]struct {
		envelope     eventrule.Envelope
		rule         *eventrule.Rule
		ruleErr      error
		wantErr      error
		wantMessage  string
		wantTerminal bool
		wantResolved bool
	}{
		"effective rule found": {
			rule:         rule,
			wantResolved: true,
		},
		"effective rule absent": {
			wantResolved: true,
		},
		"store failure is retryable": {
			ruleErr:      storeErr,
			wantErr:      storeErr,
			wantResolved: true,
		},
		"invalid persisted rule is terminal": {
			ruleErr: fmt.Errorf(
				"decode persisted override: %w",
				eventrule.ErrInvalidPersistedRule,
			),
			wantErr:      eventrule.ErrInvalidPersistedRule,
			wantTerminal: true,
			wantResolved: true,
		},
		"invalid envelope is terminal": {
			envelope:     eventrule.Envelope{},
			wantErr:      ErrTerminal,
			wantMessage:  "event source name is empty",
			wantTerminal: true,
			wantResolved: false,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			envelope := test.envelope
			if test.wantResolved {
				envelope = eventrule.Envelope{
					Key:      eventrule.EventKey{SourceName: "test", SourceKey: "event-1"},
					Type:     "test.event",
					Resource: eventrule.Resource{Kind: eventrule.ResourceKindRack, ID: rackID},
				}
			}

			resolverCalled := false
			resolver := ruleResolverFunc(func(
				_ context.Context,
				eventType eventrule.Type,
				resolvedRackID uuid.UUID,
			) (*eventrule.Rule, error) {
				resolverCalled = true

				require.Equal(t, eventrule.Type("test.event"), eventType)
				require.Equal(t, rackID, resolvedRackID)

				return test.rule, test.ruleErr
			})

			processor := newRackProcessor(t, rackID, resolver)

			result, err := processor.prepare(
				context.Background(),
				envelope,
			)

			require.Equal(t, test.wantResolved, resolverCalled)

			if test.wantErr == nil {
				require.NoError(t, err)

				if test.rule == nil {
					require.Nil(t, result)

					return
				}

				require.NotNil(t, result)
				require.Equal(t, rackID, result.Resource.ID)
				require.Equal(t, rackID, result.Resource.RackID)
				require.Equal(t, eventrule.ResourceKindRack, result.Resource.Kind)
				require.Equal(t, rackID, result.Event.Resource.ID)
				require.Equal(t, eventrule.ResourceKindRack, result.Event.Resource.Kind)
				require.Equal(t, test.rule.ID, result.Event.AppliedRuleID)
				require.Equal(t, uuid.Nil, result.Event.ID)
				require.Zero(t, result.Event.Observations)
				require.True(t, result.Event.CreatedAt.IsZero())
				return
			}

			require.Nil(t, result)
			require.ErrorIs(t, err, test.wantErr)

			if test.wantMessage != "" {
				require.ErrorContains(t, err, test.wantMessage)
			}
			if test.wantTerminal {
				require.ErrorIs(t, err, ErrTerminal)
			} else {
				require.NotErrorIs(t, err, ErrTerminal)
			}
		})
	}
}

func newRackProcessor(
	t *testing.T,
	rackID uuid.UUID,
	rules RuleResolver,
) *Processor {
	t.Helper()

	return newTestProcessor(
		t,
		&processorInventory{
			rack: rack.New(deviceinfo.DeviceInfo{ID: rackID}, location.Location{}),
		},
		rules,
	)
}

type ruleResolverFunc func(
	context.Context,
	eventrule.Type,
	uuid.UUID,
) (*eventrule.Rule, error)

func (f ruleResolverFunc) GetEffective(
	ctx context.Context,
	eventType eventrule.Type,
	rackID uuid.UUID,
) (*eventrule.Rule, error) {
	return f(ctx, eventType, rackID)
}
