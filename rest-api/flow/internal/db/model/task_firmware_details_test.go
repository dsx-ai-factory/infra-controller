// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model_test

import (
	"context"
	"encoding/json"
	"os"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	dbtestutil "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/testutil"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/migrations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// TestListFirmwareTasksForRack proves the real query's filters, sort keys and
// complete list boundary. Its targets deliberately have no inventory rows.
func TestListFirmwareTasksForRack(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping PostgreSQL firmware task test: no DB environment specified")
	}
	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	// Use CreateTestDB directly: utils.UnitTestDB skips connection failures.
	// A configured database failure must fail this proof instead.
	session, err := dbtestutil.CreateTestDB(t.Context(), t, config)
	require.NoError(t, err)
	t.Cleanup(session.Close)
	require.NoError(t, migrations.MigrateWithDB(t.Context(), session.DB, migrations.MigrateOptions{}))

	for _, tc := range []struct {
		name  string
		count int
		mixed bool
	}{
		// No rows is a successful, nonnil collection.
		{name: "empty rack"},
		// One fixture proves query wiring, both sort keys, and unresolved targets.
		{name: "firmware filtering and creation UUID order", count: 3, mixed: true},
		// Pin the supported count independently of the production constant.
		{name: "exactly 256 tasks remain complete", count: 256},
		// One more matching record must reject the entire result.
		{name: "257 tasks return no prefix", count: 257},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rackID := uuid.New()
			created := time.Date(2030, 1, 2, 3, 4, 5, 123456000, time.UTC)
			updated, started := created.Add(time.Minute), created.Add(time.Second)
			rows := make([]model.Task, 0, tc.count+5)
			wantIDs := make([]uuid.UUID, 0, tc.count)
			for i := range tc.count {
				id := uuid.UUID{14: byte((i + 1) >> 8), 15: byte(i + 1)}
				wantIDs = append(wantIDs, id)
				rows = append(rows, model.Task{
					ID:           id,
					RackID:       rackID,
					Type:         taskcommon.TaskTypeFirmwareControl,
					Status:       taskcommon.TaskStatusWaiting,
					ExecutorType: taskcommon.ExecutorTypeTemporal,
					Information:  json.RawMessage(`{"operation":1}`),
					CreatedAt:    created.Add(time.Duration(i) * time.Second),
					UpdatedAt:    updated,
					Description:  "private description",
					Report:       json.RawMessage(`{"private":"report"}`),
				})
			}
			if tc.mixed {
				// Reverse the tied IDs and make the third task older: neither
				// insertion order nor UUID order alone can produce the right list.
				rows[0].ID, rows[1].ID = rows[1].ID, rows[0].ID
				rows[1].CreatedAt, rows[1].Status = created, taskcommon.TaskStatusPending
				rows[2].CreatedAt, rows[2].Status = created.Add(-time.Second), taskcommon.TaskStatusRunning
				rows[2].StartedAt = &started
				rows[2].Attributes = taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
					devicetypes.ComponentTypeCompute:    {uuid.UUID{15: 12}, uuid.UUID{15: 11}, uuid.UUID{15: 12}},
					devicetypes.ComponentTypeNVSwitch:   {uuid.UUID{15: 13}},
					devicetypes.ComponentTypePowerShelf: {uuid.UUID{15: 14}},
				}}
				wantIDs = []uuid.UUID{rows[2].ID, rows[1].ID, rows[0].ID}
				for _, excluded := range []struct {
					rack   uuid.UUID
					typ    taskcommon.TaskType
					status taskcommon.TaskStatus
				}{
					{uuid.New(), taskcommon.TaskTypeFirmwareControl, taskcommon.TaskStatusWaiting},
					{rackID, taskcommon.TaskTypePowerControl, taskcommon.TaskStatusWaiting},
					{rackID, taskcommon.TaskTypeFirmwareControl, taskcommon.TaskStatusCompleted},
					{rackID, taskcommon.TaskTypeFirmwareControl, taskcommon.TaskStatusFailed},
				} {
					row := rows[0]
					row.ID, row.RackID, row.Type, row.Status = uuid.New(), excluded.rack, excluded.typ, excluded.status
					row.CreatedAt = created.Add(-time.Hour)
					rows = append(rows, row)
				}
			}
			wantByID := make(map[uuid.UUID]model.Task, len(rows))
			for _, row := range rows {
				wantByID[row.ID] = row
			}
			if len(rows) > 0 {
				_, err := session.DB.NewInsert().Model(&rows).Exec(t.Context())
				require.NoError(t, err)
				t.Cleanup(func() {
					ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
					defer cancel()
					_, err := session.DB.NewDelete().Model(&rows).WherePK().Exec(ctx)
					require.NoError(t, err)
				})
			}

			got, err := model.ListFirmwareTasksForRack(t.Context(), session.DB, rackID)

			if tc.count == 257 {
				var limitErr *taskcommon.FirmwareTaskLimitError
				require.ErrorAs(t, err, &limitErr)
				assert.Equal(t, rackID, limitErr.RackID)
				assert.Contains(t, err.Error(), "limit of 256 outstanding firmware tasks")
				assert.Nil(t, got)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, got)
			require.Len(t, got, len(wantIDs))
			for i, row := range got {
				want := wantByID[wantIDs[i]]
				assert.Equal(t, want.ID, row.ID)
				assert.Equal(t, want.RackID, row.RackID)
				assert.Equal(t, want.Type, row.Type)
				assert.Equal(t, want.Status, row.Status)
				assert.Equal(t, want.Attributes, row.Attributes)
				assert.JSONEq(t, string(want.Information), string(row.Information))
				assert.True(t, want.CreatedAt.Equal(row.CreatedAt))
				assert.True(t, want.UpdatedAt.Equal(row.UpdatedAt))
				if want.StartedAt == nil {
					assert.Nil(t, row.StartedAt)
				} else {
					require.NotNil(t, row.StartedAt)
					assert.True(t, want.StartedAt.Equal(*row.StartedAt))
				}
				assert.Empty(t, row.Description)
				assert.Nil(t, row.Report)
			}
		})
	}
}
