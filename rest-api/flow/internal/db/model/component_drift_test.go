// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model_test

import (
	"context"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/storetest"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestReplaceDriftsByComponentType(t *testing.T) {
	ctx := context.Background()
	session := storetest.NewPostgresTestSession(t)

	computeType := devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute)
	switchType := devicetypes.ComponentTypeToString(devicetypes.ComponentTypeNVSwitch)
	compute := createDriftTestComponent(t, ctx, session.DB, computeType)
	switchComponent := createDriftTestComponent(t, ctx, session.DB, switchType)
	legacyOrphanID := "legacy-orphan"
	drifts := []model.ComponentDrift{
		{ComponentID: &compute.ID, DriftType: model.DriftTypeMissingInActual, Diffs: []model.FieldDiff{}, CheckedAt: time.Now()},
		{ComponentID: &switchComponent.ID, DriftType: model.DriftTypeMissingInActual, Diffs: []model.FieldDiff{}, CheckedAt: time.Now()},
		{ExternalID: &legacyOrphanID, DriftType: model.DriftTypeMissingInExpected, Diffs: []model.FieldDiff{}, CheckedAt: time.Now()},
		{ComponentType: &computeType, DriftType: model.DriftTypeMissingInActual, Diffs: []model.FieldDiff{}, CheckedAt: time.Now()},
		{ComponentType: &switchType, DriftType: model.DriftTypeMissingInActual, Diffs: []model.FieldDiff{}, CheckedAt: time.Now()},
	}
	_, err := session.DB.NewInsert().Model(&drifts).Exec(ctx)
	require.NoError(t, err)

	newComputeID := "new-compute"
	err = session.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		return model.ReplaceDriftsByComponentType(ctx, tx, computeType, []model.ComponentDrift{{
			ExternalID: &newComputeID,
			DriftType:  model.DriftTypeMissingInExpected,
			Diffs:      []model.FieldDiff{},
			CheckedAt:  time.Now(),
		}})
	})
	require.NoError(t, err)

	got, err := model.GetAllDrifts(ctx, session.DB)
	require.NoError(t, err)
	assertDriftExternalIDsByType(t, got, map[string][]string{
		"":          {legacyOrphanID, ""},
		computeType: {newComputeID},
		switchType:  {""},
	})

	err = session.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		return model.ReplaceDriftsByComponentType(ctx, tx, switchType, nil)
	})
	require.NoError(t, err)
	got, err = model.GetAllDrifts(ctx, session.DB)
	require.NoError(t, err)
	assertDriftExternalIDsByType(t, got, map[string][]string{
		"":          {legacyOrphanID},
		computeType: {newComputeID},
	})

	require.NoError(t, model.DeleteLegacyUnscopedDrifts(ctx, session.DB))
	got, err = model.GetAllDrifts(ctx, session.DB)
	require.NoError(t, err)
	assertDriftExternalIDsByType(t, got, map[string][]string{computeType: {newComputeID}})
}

func createDriftTestComponent(t *testing.T, ctx context.Context, db bun.IDB, componentType string) model.Component {
	t.Helper()
	component := model.Component{
		Type:         componentType,
		Manufacturer: "test",
		SerialNumber: uuid.NewString(),
		RackID:       uuid.New(),
	}
	require.NoError(t, component.Create(ctx, db))
	return component
}

func assertDriftExternalIDsByType(t *testing.T, drifts []model.ComponentDrift, expected map[string][]string) {
	t.Helper()
	actual := make(map[string][]string)
	for _, drift := range drifts {
		componentType := ""
		if drift.ComponentType != nil {
			componentType = *drift.ComponentType
		}
		externalID := ""
		if drift.ExternalID != nil {
			externalID = *drift.ExternalID
		}
		actual[componentType] = append(actual[componentType], externalID)
	}
	require.Len(t, actual, len(expected))
	for componentType, externalIDs := range expected {
		assert.ElementsMatch(t, externalIDs, actual[componentType], componentType)
	}
}
