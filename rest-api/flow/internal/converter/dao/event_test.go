// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dao

import (
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

func TestEventRoundTrip(t *testing.T) {
	now := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	local := time.FixedZone("PDT", -7*60*60)
	event := &eventrule.Event{
		ID:            uuid.New(),
		Key:           eventrule.EventKey{SourceName: "collector", SourceKey: "event-1"},
		Type:          "hardware.leak.detected",
		Resource:      eventrule.ResourceIdentity{Kind: eventrule.ResourceKindComponent, ID: uuid.New()},
		AppliedRuleID: uuid.New(),
		EffectivePolicy: eventrule.Policy{Actions: []eventrule.Action{
			{Name: "notify", Spec: &eventrule.Noop{Reason: "audit"}},
		}},
		Summary:        "Leak detected",
		Observations:   2,
		CreatedAt:      now,
		LastObservedAt: now.Add(time.Second),
	}

	for _, test := range []struct {
		name       string
		observedAt time.Time
	}{
		{name: "source observation timestamp", observedAt: now.Add(-time.Minute)},
		{name: "older record without observation timestamp"},
	} {
		t.Run(test.name, func(t *testing.T) {
			event.FirstObservedAt = test.observedAt
			persisted, err := EventTo(event)
			require.NoError(t, err)
			persisted.CreatedAt = persisted.CreatedAt.In(local)
			persisted.LastObservedAt = persisted.LastObservedAt.In(local)
			if test.observedAt.IsZero() {
				require.Nil(t, persisted.FirstObservedAt)
			} else {
				require.NotNil(t, persisted.FirstObservedAt)
				observedAt := persisted.FirstObservedAt.In(local)
				persisted.FirstObservedAt = &observedAt
			}

			roundTripped, err := EventFrom(persisted)
			require.NoError(t, err)
			require.Equal(t, event, roundTripped)
		})
	}
}

func TestEventFromRejectsInvalidPersistence(t *testing.T) {
	result, err := EventFrom(&dbmodel.Event{})

	require.ErrorIs(t, err, eventrule.ErrInvalidPersistedEvent)
	require.Nil(t, result)

	result, err = EventFrom(nil)

	require.NoError(t, err)
	require.Nil(t, result)
}
