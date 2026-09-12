// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"os"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	commonutils "github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

func TestUniqueRackByName(t *testing.T) {
	tests := []struct {
		name      string
		racks     []model.Rack
		wantName  string
		wantCode  codes.Code
		wantError string
	}{
		{
			name:      "no match",
			wantCode:  codes.NotFound,
			wantError: "rack shared",
		},
		{
			name:     "one match",
			racks:    []model.Rack{{Name: "shared"}},
			wantName: "shared",
		},
		{
			name:      "ambiguous match",
			racks:     []model.Rack{{Name: "shared"}, {Name: "shared"}},
			wantCode:  codes.InvalidArgument,
			wantError: `rack name "shared" matches multiple racks; use rack id`,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got, err := uniqueRackByName(tc.racks, "shared")
			if tc.wantCode != codes.OK {
				require.Error(t, err)
				assert.Equal(t, tc.wantCode, status.Code(err))
				assert.ErrorContains(t, err, tc.wantError)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, got)
			assert.Equal(t, tc.wantName, got.Name)
		})
	}
}

func TestPostgresStore_GetRackByID(t *testing.T) {
	ctx := context.Background()
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}

	dbConf, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	pool, err := commonutils.UnitTestDB(ctx, t, dbConf)
	require.NoError(t, err)
	store := NewPostgres(pool)

	rackDAO := model.Rack{Name: "aggregate-rack", ExternalID: stringPtr("rack-01")}
	require.NoError(t, rackDAO.Create(ctx, pool.DB))
	components := []model.Component{
		{
			Name:   "ready",
			Type:   devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
			RackID: rackDAO.ID,
			Status: &types.ComponentOperationStatus{Phase: types.PhaseReady},
		},
		{
			Name:   "initializing",
			Type:   devicetypes.ComponentTypeToString(devicetypes.ComponentTypeNVSwitch),
			RackID: rackDAO.ID,
			Status: &types.ComponentOperationStatus{Phase: types.PhaseInitializing},
		},
	}
	for i := range components {
		require.NoError(t, components[i].Create(ctx, pool.DB))
	}

	byID, err := store.GetRackByID(ctx, rackDAO.ID, false)
	require.NoError(t, err)
	assert.Empty(t, byID.Components)
	assert.Equal(t, types.PhaseInitializing, byID.OperationStatus)

	byIDWithComponents, err := store.GetRackByID(ctx, rackDAO.ID, true)
	require.NoError(t, err)
	require.Len(t, byIDWithComponents.Components, 2)
	assert.Equal(t, byID.OperationStatus, byIDWithComponents.OperationStatus)

	listed, total, err := store.GetListOfRacks(
		ctx,
		dbquery.StringQueryInfo{},
		nil,
		nil,
		nil,
		nil,
		false,
	)
	require.NoError(t, err)
	require.Equal(t, int32(1), total)
	require.Len(t, listed, 1)
	assert.Empty(t, listed[0].Components)
	assert.Equal(t, byID.OperationStatus, listed[0].OperationStatus)
}

func stringPtr(value string) *string {
	return &value
}
