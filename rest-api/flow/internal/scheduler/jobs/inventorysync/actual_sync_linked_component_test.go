// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

type linkedInventoryTestClient struct {
	nicoapi.Client
	switches        []nicoapi.ObservedControllerDevice
	powerShelves    []nicoapi.ObservedControllerDevice
	switchErr       error
	powerShelfErr   error
	switchCalls     int
	powerShelfCalls int
}

func (c *linkedInventoryTestClient) GetSwitches(
	_ context.Context,
) ([]nicoapi.ObservedControllerDevice, error) {
	c.switchCalls++
	return c.switches, c.switchErr
}

func (c *linkedInventoryTestClient) GetPowerShelves(
	_ context.Context,
) ([]nicoapi.ObservedControllerDevice, error) {
	c.powerShelfCalls++
	return c.powerShelves, c.powerShelfErr
}

type linkedResourceTestCase struct {
	name          string
	componentType devicetypes.ComponentType
	runtimeID     string
	sync          func(context.Context, *cdb.Session, nicoapi.Client) (int, []model.ComponentDrift, bool)
	setLinked     func(*linkedInventoryTestClient, string, string)
	setError      func(*linkedInventoryTestClient, error)
	callCount     func(*linkedInventoryTestClient) int
}

func TestLinkedActualInventoryReconciliation(t *testing.T) {
	resources := []linkedResourceTestCase{
		{
			name:          "NVSwitch",
			componentType: devicetypes.ComponentTypeNVSwitch,
			runtimeID:     "core-switch-1",
			sync:          syncNVSwitchesNICo,
			setLinked: func(client *linkedInventoryTestClient, mac, runtimeID string) {
				client.switches = []nicoapi.ObservedControllerDevice{{BmcMac: mac, ID: runtimeID}}
			},
			setError:  func(client *linkedInventoryTestClient, err error) { client.switchErr = err },
			callCount: func(client *linkedInventoryTestClient) int { return client.switchCalls },
		},
		{
			name:          "PowerShelf",
			componentType: devicetypes.ComponentTypePowerShelf,
			runtimeID:     "core-power-shelf-1",
			sync:          syncPowershelvesNICo,
			setLinked: func(client *linkedInventoryTestClient, mac, runtimeID string) {
				client.powerShelves = []nicoapi.ObservedControllerDevice{{BmcMac: mac, ID: runtimeID}}
			},
			setError:  func(client *linkedInventoryTestClient, err error) { client.powerShelfErr = err },
			callCount: func(client *linkedInventoryTestClient) int { return client.powerShelfCalls },
		},
	}

	for _, resource := range resources {
		resource := resource
		t.Run(resource.name, func(t *testing.T) {
			t.Run("current match replaces stale external ID", func(t *testing.T) {
				ctx, pool := mirrorTestPool(t)
				component := createLinkedTestComponent(t, ctx, pool, resource.componentType, "aa:bb:cc:dd:ee:01", strPtr("stale-runtime-id"))
				client := &linkedInventoryTestClient{Client: nicoapi.NewMockClient()}
				resource.setLinked(client, "AA-BB-CC-DD-EE-01", resource.runtimeID)

				received, drifts, ok := resource.sync(ctx, pool, client)

				require.True(t, ok)
				assert.Equal(t, 1, received)
				assert.Empty(t, drifts)
				persisted := loadLinkedTestComponent(t, ctx, pool, component.ID)
				require.NotNil(t, persisted.ComponentID)
				assert.Equal(t, resource.runtimeID, *persisted.ComponentID)
			})

			t.Run("missing current match clears stale external ID and reports drift", func(t *testing.T) {
				ctx, pool := mirrorTestPool(t)
				component := createLinkedTestComponent(t, ctx, pool, resource.componentType, "aa:bb:cc:dd:ee:02", strPtr("stale-runtime-id"))
				client := &linkedInventoryTestClient{Client: nicoapi.NewMockClient()}

				received, drifts, ok := resource.sync(ctx, pool, client)

				require.True(t, ok)
				assert.Zero(t, received)
				require.Len(t, drifts, 1)
				assert.Equal(t, model.DriftTypeMissingInActual, drifts[0].DriftType)
				require.NotNil(t, drifts[0].ComponentID)
				assert.Equal(t, component.ID, *drifts[0].ComponentID)
				assert.Nil(t, loadLinkedTestComponent(t, ctx, pool, component.ID).ComponentID)
			})

			t.Run("empty Flow expected inventory still reports observed actual", func(t *testing.T) {
				ctx, pool := mirrorTestPool(t)
				client := &linkedInventoryTestClient{Client: nicoapi.NewMockClient()}
				resource.setLinked(client, "aa:bb:cc:dd:ee:03", resource.runtimeID)

				received, drifts, ok := resource.sync(ctx, pool, client)

				require.True(t, ok)
				assert.Equal(t, 1, received)
				assert.Equal(t, 1, resource.callCount(client))
				require.Len(t, drifts, 1)
				assert.Equal(t, model.DriftTypeMissingInExpected, drifts[0].DriftType)
				assert.Nil(t, drifts[0].ComponentID)
				require.NotNil(t, drifts[0].ExternalID)
				assert.Equal(t, resource.runtimeID, *drifts[0].ExternalID)
			})

			t.Run("one Host controller plus an auxiliary BMC matches normally", func(t *testing.T) {
				ctx, pool := mirrorTestPool(t)
				component := createLinkedTestComponent(t, ctx, pool, resource.componentType, "aa:bb:cc:dd:ee:04", nil)
				insertLinkedTestBMC(t, ctx, pool, component.ID, "aa:bb:cc:dd:ee:14", devicetypes.BMCTypeDPU)
				client := &linkedInventoryTestClient{Client: nicoapi.NewMockClient()}
				resource.setLinked(client, "aa:bb:cc:dd:ee:04", resource.runtimeID)

				_, drifts, ok := resource.sync(ctx, pool, client)

				require.True(t, ok)
				assert.Empty(t, drifts)
				persisted := loadLinkedTestComponent(t, ctx, pool, component.ID)
				require.NotNil(t, persisted.ComponentID)
				assert.Equal(t, resource.runtimeID, *persisted.ComponentID)
			})

			t.Run("actual snapshot RPC failure preserves stale external ID", func(t *testing.T) {
				ctx, pool := mirrorTestPool(t)
				component := createLinkedTestComponent(t, ctx, pool, resource.componentType, "aa:bb:cc:dd:ee:05", strPtr("stale-runtime-id"))
				client := &linkedInventoryTestClient{Client: nicoapi.NewMockClient()}
				resource.setError(client, errors.New("injected linked snapshot failure"))

				_, drifts, ok := resource.sync(ctx, pool, client)

				assert.False(t, ok)
				assert.Empty(t, drifts)
				persisted := loadLinkedTestComponent(t, ctx, pool, component.ID)
				require.NotNil(t, persisted.ComponentID)
				assert.Equal(t, "stale-runtime-id", *persisted.ComponentID)
			})
		})
	}
}

func TestReconcileLinkedActualComponentsPersistenceFailureDoesNotMutateMatchState(t *testing.T) {
	ctx, pool := mirrorTestPool(t)
	expected := []model.Component{{
		ID:   uuid.New(),
		Type: devicetypes.ComponentTypeToString(devicetypes.ComponentTypeNVSwitch),
		BMCs: []model.BMC{{
			MacAddress: "aa:bb:cc:dd:ee:20",
			Type:       devicetypes.BMCTypeToString(devicetypes.BMCTypeHost),
		}},
	}}

	received, matched, drifts, ok := reconcileActualControllerDevices(
		ctx,
		pool,
		"NVSwitch",
		expected,
		[]actualControllerDevice{{controllerMAC: "aa:bb:cc:dd:ee:20", externalID: "core-switch-missing-row"}},
	)

	assert.False(t, ok)
	assert.Equal(t, 1, received)
	assert.Nil(t, matched)
	assert.Nil(t, drifts)
	assert.Nil(t, expected[0].ComponentID)
}

func TestReconcileLinkedActualComponentsMixedUnknownActual(t *testing.T) {
	ctx, pool := mirrorTestPool(t)
	component := createLinkedTestComponent(
		t,
		ctx,
		pool,
		devicetypes.ComponentTypeNVSwitch,
		"aa:bb:cc:dd:ee:30",
		nil,
	)
	expected, err := model.GetComponentsByType(ctx, pool.DB, devicetypes.ComponentTypeNVSwitch)
	require.NoError(t, err)

	received, matched, drifts, ok := reconcileActualControllerDevices(
		ctx,
		pool,
		"NVSwitch",
		expected,
		[]actualControllerDevice{
			{controllerMAC: "aa:bb:cc:dd:ee:30", externalID: "core-switch-known"},
			{controllerMAC: "aa:bb:cc:dd:ee:31", externalID: "core-switch-unknown"},
		},
	)

	require.True(t, ok)
	assert.Equal(t, 2, received)
	require.Contains(t, matched, "core-switch-known")
	assert.Equal(t, component.ID, matched["core-switch-known"].ID)
	require.Len(t, drifts, 1)
	assert.Equal(t, model.DriftTypeMissingInExpected, drifts[0].DriftType)
	require.NotNil(t, drifts[0].ExternalID)
	assert.Equal(t, "core-switch-unknown", *drifts[0].ExternalID)
}

func TestReconcileLinkedActualComponentsSwapsExternalIDs(t *testing.T) {
	ctx, pool := mirrorTestPool(t)
	first := createLinkedTestComponent(
		t,
		ctx,
		pool,
		devicetypes.ComponentTypeNVSwitch,
		"aa:bb:cc:dd:ee:32",
		strPtr("core-switch-first"),
	)
	second := createLinkedTestComponent(
		t,
		ctx,
		pool,
		devicetypes.ComponentTypeNVSwitch,
		"aa:bb:cc:dd:ee:33",
		strPtr("core-switch-second"),
	)
	expected, err := model.GetComponentsByType(ctx, pool.DB, devicetypes.ComponentTypeNVSwitch)
	require.NoError(t, err)

	_, matched, drifts, ok := reconcileActualControllerDevices(
		ctx,
		pool,
		"NVSwitch",
		expected,
		[]actualControllerDevice{
			{controllerMAC: "aa:bb:cc:dd:ee:32", externalID: "core-switch-second"},
			{controllerMAC: "aa:bb:cc:dd:ee:33", externalID: "core-switch-first"},
		},
	)

	require.True(t, ok)
	assert.Empty(t, drifts)
	assert.Equal(t, first.ID, matched["core-switch-second"].ID)
	assert.Equal(t, second.ID, matched["core-switch-first"].ID)
	assert.Equal(t, "core-switch-second", *loadLinkedTestComponent(t, ctx, pool, first.ID).ComponentID)
	assert.Equal(t, "core-switch-first", *loadLinkedTestComponent(t, ctx, pool, second.ID).ComponentID)
}

func TestReconcileLinkedActualComponentsRejectsConflictingObservedIdentity(t *testing.T) {
	testCases := []struct {
		name     string
		observed []actualControllerDevice
	}{
		{
			name: "duplicate controller MAC",
			observed: []actualControllerDevice{
				{controllerMAC: "aa:bb:cc:dd:ee:34", externalID: "core-switch-new"},
				{controllerMAC: "AA-BB-CC-DD-EE-34", externalID: "core-switch-conflict"},
			},
		},
		{
			name: "runtime ID mapped to multiple controller MACs",
			observed: []actualControllerDevice{
				{controllerMAC: "aa:bb:cc:dd:ee:34", externalID: "core-switch-new"},
				{controllerMAC: "aa:bb:cc:dd:ee:35", externalID: "core-switch-new"},
			},
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			ctx, pool := mirrorTestPool(t)
			component := createLinkedTestComponent(
				t,
				ctx,
				pool,
				devicetypes.ComponentTypeNVSwitch,
				"aa:bb:cc:dd:ee:34",
				strPtr("core-switch-old"),
			)
			expected, err := model.GetComponentsByType(ctx, pool.DB, devicetypes.ComponentTypeNVSwitch)
			require.NoError(t, err)

			_, matched, drifts, ok := reconcileActualControllerDevices(
				ctx,
				pool,
				"NVSwitch",
				expected,
				tc.observed,
			)

			assert.False(t, ok)
			assert.Nil(t, matched)
			assert.Nil(t, drifts)
			persisted := loadLinkedTestComponent(t, ctx, pool, component.ID)
			require.NotNil(t, persisted.ComponentID)
			assert.Equal(t, "core-switch-old", *persisted.ComponentID)
		})
	}
}

func TestReconcileLinkedActualComponentsReportsAmbiguousExpectedIdentity(t *testing.T) {
	ctx, pool := mirrorTestPool(t)
	first := createLinkedTestComponent(
		t,
		ctx,
		pool,
		devicetypes.ComponentTypeNVSwitch,
		"aa:bb:cc:dd:ee:36",
		strPtr("core-switch-first"),
	)
	second := createLinkedTestComponent(
		t,
		ctx,
		pool,
		devicetypes.ComponentTypeNVSwitch,
		"AA-BB-CC-DD-EE-36",
		strPtr("core-switch-second"),
	)
	expected, err := model.GetComponentsByType(ctx, pool.DB, devicetypes.ComponentTypeNVSwitch)
	require.NoError(t, err)

	_, matched, drifts, ok := reconcileActualControllerDevices(
		ctx,
		pool,
		"NVSwitch",
		expected,
		[]actualControllerDevice{{controllerMAC: "aa:bb:cc:dd:ee:36", externalID: "core-switch-observed"}},
	)

	require.True(t, ok)
	assert.Empty(t, matched)
	require.Len(t, drifts, 3)
	assert.Equal(t, model.DriftTypeMissingInActual, drifts[0].DriftType)
	assert.Equal(t, model.DriftTypeMissingInActual, drifts[1].DriftType)
	assert.Equal(t, model.DriftTypeMissingInExpected, drifts[2].DriftType)
	assert.Nil(t, loadLinkedTestComponent(t, ctx, pool, first.ID).ComponentID)
	assert.Nil(t, loadLinkedTestComponent(t, ctx, pool, second.ID).ComponentID)
}

func TestReconcileLinkedActualComponentsInvalidMixedSnapshotIsNotApplied(t *testing.T) {
	ctx, pool := mirrorTestPool(t)
	component := createLinkedTestComponent(
		t,
		ctx,
		pool,
		devicetypes.ComponentTypeNVSwitch,
		"aa:bb:cc:dd:ee:40",
		strPtr("old-switch-id"),
	)
	expected, err := model.GetComponentsByType(ctx, pool.DB, devicetypes.ComponentTypeNVSwitch)
	require.NoError(t, err)

	received, matched, drifts, ok := reconcileActualControllerDevices(
		ctx,
		pool,
		"NVSwitch",
		expected,
		[]actualControllerDevice{
			{controllerMAC: "aa:bb:cc:dd:ee:40", externalID: "new-switch-id"},
			{controllerMAC: "not-a-mac", externalID: "invalid-switch-id"},
		},
	)

	assert.False(t, ok)
	assert.Zero(t, received)
	assert.Nil(t, matched)
	assert.Nil(t, drifts)
	persisted := loadLinkedTestComponent(t, ctx, pool, component.ID)
	require.NotNil(t, persisted.ComponentID)
	assert.Equal(t, "old-switch-id", *persisted.ComponentID)
}

func TestReconcileLinkedActualComponentsInvalidHostControllerCountsAreActionable(t *testing.T) {
	testCases := []struct {
		name string
		bmcs []model.BMC
	}{
		{
			name: "no Host controller",
			bmcs: []model.BMC{{
				MacAddress: "aa:bb:cc:dd:ee:50",
				Type:       devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU),
			}},
		},
		{
			name: "multiple Host controllers",
			bmcs: []model.BMC{
				{MacAddress: "aa:bb:cc:dd:ee:51", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeHost)},
				{MacAddress: "aa:bb:cc:dd:ee:52", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeHost)},
			},
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			ctx, pool := mirrorTestPool(t)
			component := model.Component{
				Type:         devicetypes.ComponentTypeToString(devicetypes.ComponentTypeNVSwitch),
				Manufacturer: "TestMfg",
				SerialNumber: uuid.NewString(),
				ComponentID:  strPtr("stale-switch-id"),
			}
			require.NoError(t, component.Create(ctx, pool.DB))
			for _, bmc := range tc.bmcs {
				bmc.ComponentID = component.ID
				_, err := pool.DB.NewInsert().Model(&bmc).Exec(ctx)
				require.NoError(t, err)
			}
			expected, err := model.GetComponentsByType(ctx, pool.DB, devicetypes.ComponentTypeNVSwitch)
			require.NoError(t, err)

			_, matched, drifts, ok := reconcileActualControllerDevices(
				ctx,
				pool,
				"NVSwitch",
				expected,
				nil,
			)

			require.True(t, ok)
			assert.Empty(t, matched)
			require.Len(t, drifts, 1)
			assert.Equal(t, model.DriftTypeMissingInActual, drifts[0].DriftType)
			assert.Nil(t, loadLinkedTestComponent(t, ctx, pool, component.ID).ComponentID)
		})
	}
}

func createLinkedTestComponent(
	t *testing.T,
	ctx context.Context,
	pool *cdb.Session,
	componentType devicetypes.ComponentType,
	controllerMAC string,
	externalID *string,
) model.Component {
	t.Helper()
	component := model.Component{
		Type:         devicetypes.ComponentTypeToString(componentType),
		Manufacturer: "TestMfg",
		SerialNumber: uuid.NewString(),
		ComponentID:  externalID,
	}
	require.NoError(t, component.Create(ctx, pool.DB))
	insertLinkedTestBMC(t, ctx, pool, component.ID, controllerMAC, devicetypes.BMCTypeHost)
	return component
}

func insertLinkedTestBMC(
	t *testing.T,
	ctx context.Context,
	pool *cdb.Session,
	componentID uuid.UUID,
	mac string,
	bmcType devicetypes.BMCType,
) {
	t.Helper()
	bmc := model.BMC{
		MacAddress:  mac,
		Type:        devicetypes.BMCTypeToString(bmcType),
		ComponentID: componentID,
	}
	_, err := pool.DB.NewInsert().Model(&bmc).Exec(ctx)
	require.NoError(t, err)
}

func loadLinkedTestComponent(
	t *testing.T,
	ctx context.Context,
	pool *cdb.Session,
	componentID uuid.UUID,
) model.Component {
	t.Helper()
	var component model.Component
	require.NoError(t, pool.DB.NewSelect().Model(&component).Where("id = ?", componentID).Scan(ctx))
	return component
}
