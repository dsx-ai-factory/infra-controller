// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package memory_test

import (
	"context"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/manager"
	eventscheduler "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/scheduler"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/memory"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestManagerIntegration(t *testing.T) {
	ctx := context.Background()
	eventType := leakage.TypeHardwareLeakDetected
	builtIn := leakage.DefaultRule()
	ruleManager, err := manager.New(manager.Config{
		Store: memory.New(),
		Scheduler: manager.SchedulerConfig{
			InstanceID: "memory-manager-integration-test",
			Runtime:    eventscheduler.DefaultRuntimeConfig(),
			Policy:     eventscheduler.DefaultPolicyConfig(),
		},
		Inventory:   integrationInventory{},
		TaskManager: integrationTaskManager{},
	})
	require.NoError(t, err)

	loadedBuiltIn, err := ruleManager.GetByID(ctx, builtIn.ID)
	require.NoError(t, err)
	require.Equal(t, builtIn, loadedBuiltIn)

	site, err := ruleManager.Create(ctx, integrationCreate(eventType, "site"))
	require.NoError(t, err)
	_, err = ruleManager.Bind(ctx, site.ID, eventrule.Scope{Type: eventrule.ScopeTypeSite})
	require.NoError(t, err)
	require.NoError(t, ruleManager.SetEnabled(ctx, site.ID, true))

	loadedSite, err := ruleManager.GetByID(ctx, site.ID)
	require.NoError(t, err)
	require.True(t, loadedSite.Enabled)

	rackID := uuid.New()
	rack, err := ruleManager.Create(ctx, integrationCreate(eventType, "rack"))
	require.NoError(t, err)
	_, err = ruleManager.Bind(ctx, rack.ID, eventrule.Scope{
		Type: eventrule.ScopeTypeRack,
		ID:   rackID,
	})
	require.NoError(t, err)
	require.NoError(t, ruleManager.SetEnabled(ctx, rack.ID, true))

	require.NoError(t, ruleManager.SetEnabled(ctx, rack.ID, false))

	loadedRack, err := ruleManager.GetByID(ctx, rack.ID)
	require.NoError(t, err)
	require.False(t, loadedRack.Enabled)

	rules, err := ruleManager.List(ctx, eventrule.RuleListRequest{Limit: 100})
	require.NoError(t, err)
	require.Equal(t, 3, rules.Total)
	require.Len(t, rules.Rules, 3)
}

type integrationInventory struct{}

func (integrationInventory) GetComponentByID(
	context.Context,
	uuid.UUID,
) (*component.Component, error) {
	return nil, nil
}

type integrationTaskManager struct{}

func (integrationTaskManager) SubmitTask(
	context.Context,
	*operation.Request,
) ([]uuid.UUID, error) {
	return nil, nil
}

func (integrationInventory) GetComponentsByExternalIDs(
	context.Context,
	[]string,
) ([]*component.Component, error) {
	return nil, nil
}

func (integrationInventory) GetRackByIdentifier(
	_ context.Context,
	ref identifier.Identifier,
	_ bool,
) (*rack.Rack, error) {
	return &rack.Rack{Info: deviceinfo.DeviceInfo{ID: ref.ID}}, nil
}

func integrationCreate(eventType eventrule.Type, name string) eventrule.RuleCreate {
	return eventrule.RuleCreate{
		Metadata:  eventrule.RuleMetadata{Name: name},
		EventType: eventType,
		Policy: eventrule.Policy{Actions: []eventrule.Action{
			{Name: "noop", Spec: &eventrule.Noop{}},
		}},
	}
}
