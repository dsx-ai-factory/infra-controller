// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model_test

import (
	"context"
	"encoding/json"
	"fmt"
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

// TestListPowerTasksForRack exercises PostgreSQL filtering, deterministic
// ordering, and the 256-record boundary so no generic pagination default can
// silently hide an outstanding operation.
func TestListPowerTasksForRack(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping PostgreSQL power task test: no DB environment specified")
	}
	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)

	// Configured PostgreSQL failures must fail this proof instead of skipping.
	session, err := dbtestutil.CreateTestDB(t.Context(), t, config)
	require.NoError(t, err)
	t.Cleanup(session.Close)
	err = migrations.MigrateWithDB(t.Context(), session.DB, migrations.MigrateOptions{})
	require.NoError(t, err)

	cases := []struct {
		name     string
		count    int
		mixed    bool
		overflow bool
	}{
		// No tasks is a complete, empty collection rather than a read failure.
		{name: "empty rack"},
		// Exercise rack/type/status filters and both sort keys in one fixture.
		{name: "outstanding power only in creation and UUID order", count: 3, mixed: true},
		// Pin the supported count independently of the production constant.
		{name: "exactly 256 tasks remain complete", count: 256},
		// One extra record must fail the entire read, not return the first 256.
		{name: "257 tasks exceed the supported limit", count: 257, overflow: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			rackID := uuid.New()
			createdAt := time.Date(2030, time.January, 2, 3, 4, 5, 0, time.UTC)
			updatedAt := createdAt.Add(10 * time.Minute)
			startedAt := createdAt.Add(time.Minute)
			rows := make([]model.Task, 0, tc.count)
			wantIDs := make([]uuid.UUID, 0, tc.count)
			for i := range tc.count {
				id := uuid.UUID{14: byte((i + 1) >> 8), 15: byte(i + 1)}
				wantIDs = append(wantIDs, id)
				rows = append(rows, model.Task{
					ID:           id,
					Type:         taskcommon.TaskTypePowerControl,
					RackID:       rackID,
					Status:       taskcommon.TaskStatusWaiting,
					ExecutorType: taskcommon.ExecutorTypeTemporal,
					Information:  json.RawMessage(`{"operation":1}`),
					CreatedAt:    createdAt.Add(time.Duration(i) * time.Second),
					UpdatedAt:    updatedAt,
					Description:  "not part of power details",
					Report:       json.RawMessage(`{"private":"not part of power details"}`),
				})
			}
			if tc.mixed {
				// IDs 1 and 2 tie on creation time; ID 3 is older than both.
				rows[0].CreatedAt = createdAt
				rows[1].CreatedAt = createdAt
				rows[1].Status = taskcommon.TaskStatusPending
				rows[2].CreatedAt = createdAt.Add(-time.Second)
				rows[2].Status = taskcommon.TaskStatusRunning
				rows[2].StartedAt = &startedAt

				// These recorded targets have no inventory rows. The query must
				// return their types, UUIDs, and original array membership.
				first, second, shelf := uuid.UUID{15: 11}, uuid.UUID{15: 12}, uuid.UUID{15: 13}
				rows[2].Attributes = taskcommon.TaskAttributes{
					ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
						devicetypes.ComponentTypeCompute:    {second, first, second},
						devicetypes.ComponentTypePowerShelf: {shelf},
					},
				}
				wantIDs = []uuid.UUID{rows[2].ID, rows[0].ID, rows[1].ID}

				// Insert tied IDs in reverse order to exercise the UUID sort key.
				rows[0], rows[1] = rows[1], rows[0]

				// Put excluded records before all included rows, so filtering
				// only by status cannot accidentally produce the expected list.
				for _, excludedFields := range []struct {
					rackID   uuid.UUID
					taskType taskcommon.TaskType
					status   taskcommon.TaskStatus
				}{
					// Another rack's outstanding power task is outside this read.
					{rackID: uuid.New(), taskType: taskcommon.TaskTypePowerControl, status: taskcommon.TaskStatusWaiting},
					// Non-power work is excluded even while it remains outstanding.
					{rackID: rackID, taskType: taskcommon.TaskTypeFirmwareControl, status: taskcommon.TaskStatusWaiting},
					// Completed tasks are history, not outstanding power work.
					{rackID: rackID, taskType: taskcommon.TaskTypePowerControl, status: taskcommon.TaskStatusCompleted},
					// Failed guards against treating every non-completed task as active.
					{rackID: rackID, taskType: taskcommon.TaskTypePowerControl, status: taskcommon.TaskStatusFailed},
				} {
					excluded := rows[0]
					excluded.ID = uuid.New()
					excluded.CreatedAt = createdAt.Add(-time.Hour)
					excluded.RackID = excludedFields.rackID
					excluded.Type = excludedFields.taskType
					excluded.Status = excludedFields.status
					rows = append(rows, excluded)
				}
			}
			wantByID := make(map[uuid.UUID]model.Task, len(rows))
			for _, row := range rows {
				wantByID[row.ID] = row
			}
			if len(rows) > 0 {
				_, err := session.DB.NewInsert().Model(&rows).Exec(t.Context())
				require.NoError(t, err)

				// Cases reuse fixed IDs. Clean up even after a fatal assertion,
				// using a fresh context because t.Context is cancelled first.
				t.Cleanup(func() {
					ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
					defer cancel()
					_, err := session.DB.NewDelete().Model(&rows).WherePK().Exec(ctx)
					require.NoError(t, err)
				})
			}

			got, err := model.ListPowerTasksForRack(t.Context(), session.DB, rackID)

			if tc.overflow {
				var limitErr *taskcommon.PowerTaskLimitError
				require.ErrorAs(t, err, &limitErr)
				assert.Equal(t, rackID, limitErr.RackID)
				assert.EqualError(t, err, fmt.Sprintf("rack %s exceeds the limit of 256 outstanding power tasks", rackID))
				assert.Nil(t, got, "overflow must not look like a complete prefix")
			} else {
				require.NoError(t, err)
				require.Len(t, got, len(wantIDs))
				gotIDs := make([]uuid.UUID, 0, len(got))
				for _, row := range got {
					want := wantByID[row.ID]
					gotIDs = append(gotIDs, row.ID)
					assert.Equal(t, rackID, row.RackID)
					assert.Equal(t, taskcommon.TaskTypePowerControl, row.Type)
					assert.JSONEq(t, `{"operation":1}`, string(row.Information))
					assert.Equal(t, want.Attributes, row.Attributes)
					assert.True(t, want.CreatedAt.Equal(row.CreatedAt), "creation time for %s", row.ID)
					assert.True(t, want.UpdatedAt.Equal(row.UpdatedAt), "update time for %s", row.ID)
					assert.Empty(t, row.Description)
					assert.Nil(t, row.Report)
				}
				assert.Equal(t, wantIDs, gotIDs)
				if tc.mixed {
					assert.Equal(t, []taskcommon.TaskStatus{
						taskcommon.TaskStatusRunning, taskcommon.TaskStatusWaiting, taskcommon.TaskStatusPending,
					}, []taskcommon.TaskStatus{got[0].Status, got[1].Status, got[2].Status})
					require.NotNil(t, got[0].StartedAt)
					assert.True(t, startedAt.Equal(*got[0].StartedAt))
					assert.Nil(t, got[1].StartedAt)
					assert.Nil(t, got[2].StartedAt)
				}
			}
		})
	}
}
