// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage"
	eventscheduler "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/scheduler"
	eventstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/postgres"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/storetest"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestManager_PostgresRuleHierarchy(t *testing.T) {
	ctx := context.Background()
	session := storetest.NewPostgresTestSession(t)
	rackID := uuid.New()
	inventory := newPostgresIntegrationInventory(rackID)
	config := testManagerConfig()
	store := eventstore.New(session)
	config.Store = store
	config.Inventory = inventory

	manager, err := New(config)
	require.NoError(t, err)

	envelope := eventrule.Envelope{
		Key: eventrule.EventKey{
			SourceName: "manager_postgres_test",
			SourceKey:  "built-in",
		},
		Type: leakage.TypeHardwareLeakDetected,
		Resource: eventrule.Resource{
			Kind: eventrule.ResourceKindRack,
			ID:   rackID,
		},
	}

	require.NoError(t, manager.Process(ctx, envelope))

	site, err := manager.Create(
		ctx,
		testRuleCreate(leakage.TypeHardwareLeakDetected, "site"),
	)
	require.NoError(t, err)
	_, err = manager.Bind(ctx, site.ID, eventrule.Scope{Type: eventrule.ScopeTypeSite})
	require.NoError(t, err)
	require.NoError(t, manager.SetEnabled(ctx, site.ID, true))

	envelope.Key.SourceKey = "site"
	require.NoError(t, manager.Process(ctx, envelope))

	rackRule, err := manager.Create(
		ctx,
		testRuleCreate(leakage.TypeHardwareLeakDetected, "rack"),
	)
	require.NoError(t, err)
	_, err = manager.Bind(ctx, rackRule.ID, eventrule.Scope{
		Type: eventrule.ScopeTypeRack,
		ID:   rackID,
	})
	require.NoError(t, err)
	require.NoError(t, manager.SetEnabled(ctx, rackRule.ID, true))

	envelope.Key.SourceKey = "rack"
	require.NoError(t, manager.Process(ctx, envelope))

	events, err := store.Events(ctx)
	require.NoError(t, err)
	require.Len(t, events, 3)

	applied := make(map[string]uuid.UUID, len(events))
	for _, event := range events {
		applied[event.Key.SourceKey] = event.AppliedRuleID
	}

	require.Equal(t, leakage.DefaultRule().ID, applied["built-in"])
	require.Equal(t, site.ID, applied["site"])
	require.Equal(t, rackRule.ID, applied["rack"])
}

func TestManager_PostgresDurableRetryWithoutRedelivery(t *testing.T) {
	ctx := context.Background()
	session := storetest.NewPostgresTestSession(t)
	rackID := uuid.New()
	inventory := newPostgresIntegrationInventory(rackID)
	tasks := &postgresRetryTaskManager{failFirst: true}
	config := testManagerConfig()
	store := eventstore.New(session)
	config.Store = store
	config.Inventory = inventory
	config.TaskManager = tasks
	config.Scheduler.Runtime.PollInterval = 5 * time.Millisecond
	config.Scheduler.Policy = eventscheduler.PolicyConfig{
		MaxAttempts:  2,
		InitialDelay: 200 * time.Millisecond,
		MaxDelay:     200 * time.Millisecond,
	}

	manager, err := New(config)
	require.NoError(t, err)

	require.NoError(t, manager.Start(ctx))
	t.Cleanup(func() {
		require.NoError(t, manager.Stop())
	})

	envelope := eventrule.Envelope{
		Key: eventrule.EventKey{
			SourceName: "manager_postgres_test",
			SourceKey:  "durable-retry",
		},
		Type: leakage.TypeHardwareLeakDetected,
		Resource: eventrule.Resource{
			Kind: eventrule.ResourceKindComponent,
			ID:   inventory.component.Info.ID,
		},
	}

	require.NoError(t, manager.Process(ctx, envelope))

	require.Eventually(t, func() bool {
		executions, err := store.Executions(ctx)

		return err == nil &&
			len(executions) == 1 &&
			executions[0].Status == eventrule.ExecutionStatusDeferred &&
			executions[0].Attempts == 1
	}, time.Second, 5*time.Millisecond)

	require.Eventually(t, func() bool {
		executions, err := store.Executions(ctx)

		return err == nil &&
			len(executions) == 1 &&
			executions[0].Status == eventrule.ExecutionStatusCompleted &&
			executions[0].Attempts == 2
	}, 3*time.Second, 10*time.Millisecond)
	require.Equal(t, 2, tasks.callCount())

	require.NoError(t, manager.Process(ctx, envelope))

	events, err := store.Events(ctx)
	require.NoError(t, err)
	require.Len(t, events, 1)
	require.Equal(t, 2, events[0].Observations)

	executions, err := store.Executions(ctx)
	require.NoError(t, err)
	require.Len(t, executions, 1)
	require.Equal(t, eventrule.ExecutionStatusCompleted, executions[0].Status)
	require.Equal(t, 2, executions[0].Attempts)
	require.Equal(t, 2, tasks.callCount())

}

func TestManager_PostgresExpiredClaimRecovery(t *testing.T) {
	ctx := context.Background()
	session := storetest.NewPostgresTestSession(t)
	rackID := uuid.New()
	inventory := newPostgresIntegrationInventory(rackID)
	tasks := &postgresRetryTaskManager{}
	config := testManagerConfig()
	store := eventstore.New(session)
	config.Store = store
	config.Inventory = inventory
	config.TaskManager = tasks
	config.Scheduler.Runtime.PollInterval = 5 * time.Millisecond
	config.Scheduler.Runtime.ClaimDuration = 120 * time.Millisecond
	config.Scheduler.Policy.MaxAttempts = 2

	manager, err := New(config)
	require.NoError(t, err)

	envelope := eventrule.Envelope{
		Key: eventrule.EventKey{
			SourceName: "manager_postgres_test",
			SourceKey:  "expired-claim-recovery",
		},
		Type: leakage.TypeHardwareLeakDetected,
		Resource: eventrule.Resource{
			Kind: eventrule.ResourceKindComponent,
			ID:   inventory.component.Info.ID,
		},
	}
	require.NoError(t, manager.Process(ctx, envelope))

	batch, err := store.ClaimPendingExecutions(ctx, eventrule.ExecutionClaimRequest{
		Owner:         "crashed-scheduler",
		Limit:         1,
		ClaimDuration: 50 * time.Millisecond,
		MaxAttempts:   2,
	})
	require.NoError(t, err)
	require.Len(t, batch.Claims, 1)
	claim := batch.Claims[0]

	require.Eventually(t, func() bool {
		var expired bool
		err := session.DB.NewSelect().
			TableExpr("event_action_executions").
			ColumnExpr("claim_expires_at <= transaction_timestamp()").
			Where("id = ?", claim.Execution.ID).
			Scan(ctx, &expired)

		return err == nil && expired
	}, time.Second, 5*time.Millisecond)

	require.NoError(t, manager.Start(ctx))
	t.Cleanup(func() {
		require.NoError(t, manager.Stop())
	})

	require.Eventually(t, func() bool {
		executions, err := store.Executions(ctx)

		return err == nil &&
			len(executions) == 1 &&
			executions[0].Status == eventrule.ExecutionStatusCompleted &&
			executions[0].Attempts == 2
	}, time.Second, 5*time.Millisecond)
	require.Equal(t, 1, tasks.callCount())

	err = store.TransitionClaimedExecution(
		ctx,
		claim.Execution.ID,
		claim.Token,
		eventrule.CompletedExecutionResult(),
	)
	require.ErrorIs(t, err, eventrule.ErrExecutionClaimLost)
}

type postgresIntegrationInventory struct {
	rack      *rack.Rack
	component *component.Component
}

func newPostgresIntegrationInventory(rackID uuid.UUID) *postgresIntegrationInventory {
	resolvedComponent := component.Component{
		Type:        devicetypes.ComponentTypeCompute,
		Info:        deviceinfo.DeviceInfo{ID: uuid.New()},
		Position:    component.InRackPosition{SlotID: 1},
		ComponentID: "postgres-component",
		RackID:      rackID,
	}
	resolvedRack := &rack.Rack{
		Info:       deviceinfo.DeviceInfo{ID: rackID},
		Components: []component.Component{resolvedComponent},
	}

	return &postgresIntegrationInventory{
		rack:      resolvedRack,
		component: &resolvedComponent,
	}
}

func (i *postgresIntegrationInventory) GetComponentByID(
	_ context.Context,
	id uuid.UUID,
) (*component.Component, error) {
	if id != i.component.Info.ID {
		return nil, errors.New("component not found")
	}

	cloned := *i.component

	return &cloned, nil
}

func (i *postgresIntegrationInventory) GetComponentsByExternalIDs(
	_ context.Context,
	externalIDs []string,
) ([]*component.Component, error) {
	if len(externalIDs) != 1 || externalIDs[0] != i.component.ComponentID {
		return nil, errors.New("component not found")
	}

	cloned := *i.component

	return []*component.Component{&cloned}, nil
}

func (i *postgresIntegrationInventory) GetRackByIdentifier(
	_ context.Context,
	_ identifier.Identifier,
	_ bool,
) (*rack.Rack, error) {
	cloned := *i.rack
	cloned.Components = append([]component.Component(nil), i.rack.Components...)

	return &cloned, nil
}

type postgresRetryTaskManager struct {
	mu        sync.Mutex
	calls     int
	failFirst bool
}

func (m *postgresRetryTaskManager) SubmitTask(
	_ context.Context,
	_ *operation.Request,
) ([]uuid.UUID, error) {
	m.mu.Lock()
	defer m.mu.Unlock()

	m.calls++
	if m.failFirst && m.calls == 1 {
		return nil, errors.New("temporary task service failure")
	}

	return []uuid.UUID{uuid.New()}, nil
}

func (m *postgresRetryTaskManager) callCount() int {
	m.mu.Lock()
	defer m.mu.Unlock()

	return m.calls
}
