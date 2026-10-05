// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/db/model"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/pkg/types"
)

type failRackHealthClient struct {
	nicoapi.Client
}

func (c *failRackHealthClient) FindRackHealthReports(_ context.Context, _ []string) (map[string]*types.HealthReport, error) {
	return nil, errors.New("boom")
}

func TestPersistComponentHealthSnapshots(t *testing.T) {
	ctx, pool := mirrorTestPool(t)
	externalID := "fm100-test"
	oldHealth := &types.HealthReport{Source: "old", Successes: []types.HealthProbeSuccess{}, Alerts: []types.HealthProbeAlert{}}
	component := model.Component{
		Type:        devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
		ComponentID: &externalID,
		Health:      oldHealth,
	}
	require.NoError(t, component.Create(ctx, pool.DB))

	newHealth := &types.HealthReport{Source: "new", Successes: []types.HealthProbeSuccess{{ID: "PowerSupply"}}, Alerts: []types.HealthProbeAlert{}}
	persistComponentHealthSnapshots(ctx, pool, map[string]*types.HealthReport{externalID: newHealth}, map[string]*model.Component{externalID: &component})

	stored, err := (&model.Component{ID: component.ID}).Get(ctx, pool.DB)
	require.NoError(t, err)
	assert.Equal(t, newHealth, stored.Health)

	persistComponentHealthSnapshots(ctx, pool, map[string]*types.HealthReport{}, map[string]*model.Component{externalID: stored})
	stored, err = (&model.Component{ID: component.ID}).Get(ctx, pool.DB)
	require.NoError(t, err)
	assert.Equal(t, newHealth, stored.Health, "a missing Core snapshot must not clear the last known health")

	persistComponentHealthSnapshots(ctx, pool, map[string]*types.HealthReport{externalID: nil}, map[string]*model.Component{externalID: stored})
	stored, err = (&model.Component{ID: component.ID}).Get(ctx, pool.DB)
	require.NoError(t, err)
	assert.Nil(t, stored.Health, "an authoritative empty Core report must clear stale health")
}

func TestSyncRackHealth(t *testing.T) {
	ctx, pool := mirrorTestPool(t)
	externalID := "rack-01"
	oldHealth := &types.HealthReport{Source: "old", Successes: []types.HealthProbeSuccess{}, Alerts: []types.HealthProbeAlert{}}
	rack := model.Rack{Name: "rack", ExternalID: &externalID, Health: oldHealth}
	require.NoError(t, rack.Create(ctx, pool.DB))

	client := nicoapi.NewMockClient()
	newHealth := &types.HealthReport{Source: "new", Successes: []types.HealthProbeSuccess{}, Alerts: []types.HealthProbeAlert{{ID: "RackAlert", Message: "fault"}}}
	client.SetRackHealth(externalID, newHealth)
	syncRackHealth(ctx, pool, client)

	stored, err := (&model.Rack{ID: rack.ID}).Get(ctx, pool.DB, false)
	require.NoError(t, err)
	assert.Equal(t, newHealth, stored.Health)

	syncRackHealth(ctx, pool, &failRackHealthClient{Client: client})
	stored, err = (&model.Rack{ID: rack.ID}).Get(ctx, pool.DB, false)
	require.NoError(t, err)
	assert.Equal(t, newHealth, stored.Health, "a failed Core refresh must preserve the last known health")

	client.SetRackHealth(externalID, nil)
	syncRackHealth(ctx, pool, client)
	stored, err = (&model.Rack{ID: rack.ID}).Get(ctx, pool.DB, false)
	require.NoError(t, err)
	assert.Nil(t, stored.Health, "an authoritative empty Core report must clear stale health")
}
