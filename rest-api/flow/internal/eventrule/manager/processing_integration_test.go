// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/memory"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/location"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestManager_Start(t *testing.T) {
	ctx := context.Background()
	eventType := leakage.TypeHardwareLeakDetected
	rackID := uuid.New()
	config := testManagerConfig()
	config.Scheduler.Runtime.PollInterval = time.Hour
	config.Inventory = &processingInventory{
		rack: rack.New(deviceinfo.DeviceInfo{ID: rackID}, location.Location{}),
	}

	manager, err := New(config)
	require.NoError(t, err)

	store, ok := manager.store.(*memory.Store)
	require.True(t, ok)

	rule, err := manager.Create(ctx, testRuleCreate(eventType, "scheduled"))
	require.NoError(t, err)
	_, err = manager.Bind(ctx, rule.ID, eventrule.Scope{
		Type: eventrule.ScopeTypeRack,
		ID:   rackID,
	})
	require.NoError(t, err)
	require.NoError(t, manager.SetEnabled(ctx, rule.ID, true))

	runCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	require.NoError(t, manager.Start(runCtx))

	require.NoError(t, manager.Process(ctx, eventrule.Envelope{
		Key:      eventrule.EventKey{SourceName: "test", SourceKey: "scheduled-event"},
		Type:     eventType,
		Resource: eventrule.Resource{Kind: eventrule.ResourceKindRack, ID: rackID},
	}))

	require.Eventually(t, func() bool {
		executions, err := store.Executions()
		return err == nil &&
			len(executions) == 1 &&
			executions[0].Status == eventrule.ExecutionStatusCompleted
	}, time.Second, time.Millisecond)

	require.EqualError(
		t,
		manager.Start(context.Background()),
		"scheduler can only be started once",
	)
	require.NoError(t, manager.Stop())
}

func TestManager_Stop(t *testing.T) {
	tests := map[string]struct {
		prepare func(*testing.T, *Manager)
		wantErr string
	}{
		"before start": {
			wantErr: "scheduler cannot be stopped before it is started",
		},
		"after start": {
			prepare: func(t *testing.T, manager *Manager) {
				require.NoError(t, manager.Start(context.Background()))
			},
		},
		"second stop": {
			prepare: func(t *testing.T, manager *Manager) {
				require.NoError(t, manager.Start(context.Background()))
				require.NoError(t, manager.Stop())
			},
			wantErr: "scheduler can only be stopped once",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			manager, err := New(testManagerConfig())
			require.NoError(t, err)

			if test.prepare != nil {
				test.prepare(t, manager)
			}

			err = manager.Stop()
			if test.wantErr != "" {
				require.EqualError(t, err, test.wantErr)

				return
			}

			require.NoError(t, err)
		})
	}
}

func TestManager_ProcessIntegration(t *testing.T) {
	ctx := context.Background()
	eventType := leakage.TypeHardwareLeakDetected
	rackID := uuid.New()
	builtIn := leakage.DefaultRule()
	config := testManagerConfig()
	config.Inventory = &processingInventory{
		rack: rack.New(deviceinfo.DeviceInfo{ID: rackID}, location.Location{}),
	}

	manager, err := New(config)
	require.NoError(t, err)

	store, ok := manager.store.(*memory.Store)
	require.True(t, ok)

	rackRule, err := manager.Create(ctx, testRuleCreate(eventType, "rack"))
	require.NoError(t, err)

	_, err = manager.Bind(ctx, rackRule.ID, eventrule.Scope{
		Type: eventrule.ScopeTypeRack,
		ID:   rackID,
	})
	require.NoError(t, err)

	envelope := eventrule.Envelope{
		Key:      eventrule.EventKey{SourceName: "test", SourceKey: "event-1"},
		Type:     eventType,
		Resource: eventrule.Resource{Kind: eventrule.ResourceKindRack, ID: rackID},
	}

	err = manager.Process(ctx, envelope)
	require.NoError(t, err)

	processed := storedEvent(t, store, envelope.Key.SourceKey)
	require.Equal(t, rackID, processed.Resource.ID)
	require.Equal(t, builtIn.ID, processed.AppliedRuleID)

	require.NoError(t, manager.SetEnabled(ctx, rackRule.ID, true))

	envelope.Key.SourceKey = "event-2"
	err = manager.Process(ctx, envelope)
	require.NoError(t, err)

	processed = storedEvent(t, store, envelope.Key.SourceKey)
	require.Equal(t, rackRule.ID, processed.AppliedRuleID)
}

func storedEvent(
	t *testing.T,
	store *memory.Store,
	sourceKey string,
) eventrule.Event {
	t.Helper()

	events, err := store.Events()
	require.NoError(t, err)

	for _, event := range events {
		if event.Key.SourceKey == sourceKey {
			return event
		}
	}

	require.FailNow(t, "stored event not found", "source key: %s", sourceKey)

	return eventrule.Event{}
}

type processingInventory struct {
	rack *rack.Rack
}

func (*processingInventory) GetComponentByID(
	context.Context,
	uuid.UUID,
) (*component.Component, error) {
	return nil, nil
}

func (*processingInventory) GetComponentsByExternalIDs(
	context.Context,
	[]string,
) ([]*component.Component, error) {
	return nil, nil
}

func (i *processingInventory) GetRackByIdentifier(
	context.Context,
	identifier.Identifier,
	bool,
) (*rack.Rack, error) {
	return i.rack, nil
}
