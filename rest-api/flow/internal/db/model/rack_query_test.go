// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model_test

import (
	"os"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// TestGetListOfRacks verifies model ordering and pagination with duplicate and
// absent model values, plus ordinary-column sorting and total counts.
func TestGetListOfRacks(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}
	ctx := t.Context()
	dbConf, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	pool, err := utils.UnitTestDB(ctx, t, dbConf)
	require.NoError(t, err)

	racks := []model.Rack{
		{ID: uuid.UUID{15: 1}, Name: "rack-c", Manufacturer: "C", Description: map[string]any{"model": "GB300"}},
		{ID: uuid.UUID{15: 2}, Name: "rack-a", Manufacturer: "A", Description: map[string]any{"model": "GB200"}},
		{ID: uuid.UUID{15: 3}, Name: "rack-b", Manufacturer: "B", Description: map[string]any{"model": "GB200"}},
		{ID: uuid.UUID{15: 4}, Name: "rack-d", Manufacturer: "D", Description: map[string]any{}},
		{ID: uuid.UUID{15: 5}, Name: "rack-e", Manufacturer: "E", Description: map[string]any{"model": nil}},
		{ID: uuid.UUID{15: 6}, Name: "rack-f", Manufacturer: "F", Description: nil},
	}
	for i := range racks {
		err = racks[i].Create(ctx, pool.DB)
		require.NoError(t, err)
	}

	cases := []struct {
		name      string
		field     pb.RackOrderByField
		direction string
		pageSize  int
		want      []int
	}{
		{
			name:  "model ascending with ties and absent values",
			field: pb.RackOrderByField_RACK_ORDER_BY_FIELD_MODEL, direction: "ASC",
			pageSize: 2, want: []int{1, 2, 0, 3, 4, 5},
		},
		{
			name:  "model descending with ties and absent values",
			field: pb.RackOrderByField_RACK_ORDER_BY_FIELD_MODEL, direction: "DESC",
			pageSize: 2, want: []int{3, 4, 5, 0, 1, 2},
		},
		{
			name:  "name ascending",
			field: pb.RackOrderByField_RACK_ORDER_BY_FIELD_NAME, direction: "ASC",
			pageSize: 100, want: []int{1, 2, 0, 3, 4, 5},
		},
		{
			name:  "manufacturer descending",
			field: pb.RackOrderByField_RACK_ORDER_BY_FIELD_MANUFACTURER, direction: "DESC",
			pageSize: 100, want: []int{5, 4, 3, 0, 2, 1},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			orderBy, err := protobuf.RackOrderByFrom(&pb.OrderBy{
				Field:     &pb.OrderBy_RackField{RackField: tc.field},
				Direction: tc.direction,
			})
			require.NoError(t, err)
			require.NotNil(t, orderBy)
			var gotIDs []uuid.UUID
			for page := range (len(racks) + tc.pageSize - 1) / tc.pageSize {
				got, total, err := model.GetListOfRacks(
					ctx, pool.DB, dbquery.StringQueryInfo{}, nil, nil,
					&dbquery.Pagination{Offset: page * tc.pageSize, Limit: tc.pageSize},
					orderBy, false, false,
				)
				require.NoError(t, err)
				assert.EqualValues(t, len(racks), total)
				for _, rack := range got {
					gotIDs = append(gotIDs, rack.ID)
				}
			}
			wantIDs := make([]uuid.UUID, len(tc.want))
			for i, index := range tc.want {
				wantIDs[i] = racks[index].ID
			}
			assert.Equal(t, wantIDs, gotIDs)
		})
	}
}
