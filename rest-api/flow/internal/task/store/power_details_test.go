// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"bytes"
	"context"
	"encoding/json"
	"testing"
	"time"

	"github.com/DATA-DOG/go-sqlmock"
	"github.com/google/uuid"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// TestPowerTaskDetailsFromDAO proves stored intent is validated before it can
// become a public action, and recorded target UUIDs and timestamps survive.
func TestPowerTaskDetailsFromDAO(t *testing.T) {
	taskID, rackID := uuid.New(), uuid.New()
	first, second, shelf := uuid.UUID{15: 1}, uuid.UUID{15: 2}, uuid.UUID{15: 3}
	createdAt := time.Date(2030, time.January, 2, 3, 4, 5, 0, time.UTC)
	updatedAt := createdAt.Add(time.Minute)
	startedAt := createdAt.Add(time.Second)

	cases := []struct {
		name        string
		taskType    taskcommon.TaskType
		information string
		wantError   string
		wantStage   string
		privateText string
	}{
		{
			// Preserve forced/readiness flags instead of recovering an action
			// from the task's human-readable description.
			name:        "typed intent and recorded targets",
			taskType:    taskcommon.TaskTypePowerControl,
			information: `{"operation":4,"forced":true,"override_readiness_check":true}`,
		},
		{
			// Numeric overflow includes the number in the decoder error. Neither
			// the public error nor the diagnostic may expose that stored value.
			name:        "malformed action hides stored text",
			taskType:    taskcommon.TaskTypePowerControl,
			information: `{"operation":987654321098765432109876543210}`,
			wantError:   "invalid operation data",
			wantStage:   "operation_decode",
			privateText: "987654321098765432109876543210",
		},
		{
			// CodeString defaults unknown actions to power_on, so Validate must
			// reject them before any converter can use that fallback.
			name:        "unknown action is not power on",
			taskType:    taskcommon.TaskTypePowerControl,
			information: `{"operation":999}`,
			wantError:   "invalid action",
			wantStage:   "operation_validation",
		},
		{
			// Do not reinterpret a different typed operation as power intent.
			name:        "non power operation rejected",
			taskType:    taskcommon.TaskTypeFirmwareControl,
			information: `{"operation":1}`,
			wantError:   "not a power task",
			wantStage:   "operation_type",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var diagnostics bytes.Buffer
			previousLogger := log.Logger
			log.Logger = zerolog.New(&diagnostics)
			t.Cleanup(func() { log.Logger = previousLogger })

			row := model.Task{
				ID: taskID, RackID: rackID, Type: tc.taskType,
				Information: json.RawMessage(tc.information),
				Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
					devicetypes.ComponentTypeCompute:    {second, first, second},
					devicetypes.ComponentTypePowerShelf: {shelf},
					devicetypes.ComponentTypeNVSwitch:   {},
				}},
				Status:    taskcommon.TaskStatusRunning,
				CreatedAt: createdAt, UpdatedAt: updatedAt, StartedAt: &startedAt,
				Description: "PowerOn (this display text is not authoritative)",
				Report:      json.RawMessage(`{"private":"private-operation-data"}`),
			}
			if tc.privateText != "" {
				// Establish that forwarding the decoder error would leak this
				// exact value; absence alone would not prove redaction.
				_, decodeErr := operations.New(tc.taskType, row.Information)
				require.ErrorContains(t, decodeErr, tc.privateText)
			}

			got, err := powerTaskDetailsFromDAO(row)

			assert.NotContains(t, diagnostics.String(), "private-operation-data")
			if tc.wantError != "" {
				require.Error(t, err)
				assert.Equal(t, codes.Internal, status.Code(err))
				assert.Contains(t, err.Error(), tc.wantError)
				assert.Contains(t, err.Error(), taskID.String())
				assert.NotContains(t, err.Error(), "private-operation-data")
				assert.Equal(t, taskdef.PowerTaskDetails{}, got)
				if tc.privateText != "" {
					assert.NotContains(t, err.Error(), tc.privateText)
					assert.NotContains(t, diagnostics.String(), tc.privateText)
				}
				var fields map[string]any
				require.NoError(t, json.Unmarshal(diagnostics.Bytes(), &fields))
				assert.Equal(t, rackID.String(), fields["rack_id"])
				assert.Equal(t, taskID.String(), fields["task_id"])
				assert.Equal(t, tc.wantStage, fields["failure_stage"])
				return
			}

			require.NoError(t, err)
			assert.Empty(t, diagnostics.String())
			assert.Equal(t, taskdef.PowerTaskDetails{
				ID: taskID, RackID: rackID,
				Operation: operations.PowerControlTaskInfo{
					Operation: operations.PowerOperationForcePowerOff,
					Forced:    true, OverrideReadinessCheck: true,
				},
				Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
					devicetypes.ComponentTypeCompute:    {first, second},
					devicetypes.ComponentTypePowerShelf: {shelf},
					devicetypes.ComponentTypeNVSwitch:   {},
				}},
				Status:    taskcommon.TaskStatusRunning,
				CreatedAt: createdAt, UpdatedAt: updatedAt, StartedAt: &startedAt,
			}, got)
		})
	}
}

// TestPostgresStore_ListPowerTasksForRack exercises the store boundary: one
// query supplies typed tasks, while failures never leave an apparently
// complete prefix or expose database payloads to callers.
func TestPostgresStore_ListPowerTasksForRack(t *testing.T) {
	rackID, targetID := uuid.New(), uuid.New()
	createdAt := time.Date(2030, time.January, 2, 3, 4, 5, 0, time.UTC)
	updatedAt := createdAt.Add(time.Minute)
	cases := []struct {
		name              string
		count             int
		invalidLast       bool
		invalidAttributes bool
		queryError        error
		cancel            bool
		wantCode          codes.Code
		overflow          bool
		wantStage         string
	}{
		// This boundary loads stored membership in one query without any
		// inventory lookup; PostgreSQL coverage proves the selected columns.
		{name: "one query returns complete stored details", count: 1},
		// No matches is a successful empty collection, not an unavailable read.
		{name: "empty collection succeeds"},
		// A valid earlier row must not escape when a later operation is invalid.
		{name: "conversion failure discards earlier tasks", count: 2, invalidLast: true, wantCode: codes.Internal, wantStage: "operation_validation"},
		// Preserve a useful SQLSTATE without logging the server's message/detail.
		{name: "database failure keeps only safe diagnostics", queryError: &pgconn.PgError{
			Code: "42703", Message: "private-operation-data", Detail: "private-operation-data",
		}, wantCode: codes.Internal, wantStage: "database_read"},
		// Attribute decoding can quote a stored key inside Bun's scan error.
		{name: "scan failure hides stored attributes", count: 1, invalidAttributes: true, wantCode: codes.Internal, wantStage: "database_read"},
		// The typed overflow must survive the store so the RPC can distinguish
		// unsupported collection size from a database outage.
		{name: "overflow stays typed", count: 257, overflow: true},
		// Preserve cancellation so an abandoned RPC does not become an outage.
		{name: "caller cancellation", cancel: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var diagnostics bytes.Buffer
			previousLogger := log.Logger
			log.Logger = zerolog.New(&diagnostics)
			t.Cleanup(func() { log.Logger = previousLogger })

			sqlDB, mock, err := sqlmock.New()
			require.NoError(t, err)
			db := bun.NewDB(sqlDB, pgdialect.New())
			t.Cleanup(func() {
				mock.ExpectClose()
				require.NoError(t, db.Close())
			})
			store := NewPostgres(&cdb.Session{DB: db})
			ctx, cancel := context.WithCancel(t.Context())
			defer cancel()

			if tc.cancel {
				cancel()
			} else {
				// Pin the fetched row bound separately from the production constant.
				expectation := mock.ExpectQuery(`FROM "task" AS "t".* LIMIT 257$`)
				if tc.queryError != nil {
					expectation.WillReturnError(tc.queryError)
				} else {
					rows := sqlmock.NewRows([]string{
						"id", "type", "rack_id", "information", "attributes", "status", "created_at", "updated_at", "started_at",
					})
					for i := range tc.count {
						information := `{"operation":1}`
						if tc.invalidLast && i == tc.count-1 {
							information = `{"operation":0}`
						}
						attributes := `{"components_by_type":{"Compute":["` + targetID.String() + `"]}}`
						if tc.invalidAttributes {
							attributes = `{"components_by_type":{"private-operation-data":[]}}`
							var decoded taskcommon.TaskAttributes
							decodeErr := json.Unmarshal([]byte(attributes), &decoded)
							require.ErrorContains(t, decodeErr, "private-operation-data")
						}
						rows.AddRow(
							uuid.UUID{14: byte((i + 1) >> 8), 15: byte(i + 1)},
							taskcommon.TaskTypePowerControl, rackID, information,
							attributes,
							taskcommon.TaskStatusWaiting, createdAt, updatedAt, nil,
						)
					}
					expectation.WillReturnRows(rows)
				}
			}

			got, err := store.ListPowerTasksForRack(ctx, rackID)

			assert.NotContains(t, diagnostics.String(), "private-operation-data")
			if tc.wantStage == "" {
				assert.Empty(t, diagnostics.String())
			} else {
				var fields map[string]any
				require.NoError(t, json.Unmarshal(diagnostics.Bytes(), &fields))
				assert.Equal(t, rackID.String(), fields["rack_id"])
				assert.Equal(t, tc.wantStage, fields["failure_stage"])
				assert.NotEmpty(t, fields["error_type"])
				if tc.invalidLast {
					assert.Equal(t, (uuid.UUID{15: 2}).String(), fields["task_id"])
				}
				if tc.queryError != nil {
					assert.Equal(t, "42703", fields["sqlstate"])
				}
			}
			switch {
			case tc.overflow:
				var limitErr *taskcommon.PowerTaskLimitError
				require.ErrorAs(t, err, &limitErr)
				assert.Equal(t, rackID, limitErr.RackID)
				assert.Nil(t, got)
			case tc.cancel:
				require.ErrorIs(t, err, context.Canceled)
				assert.Nil(t, got)
			case tc.wantCode != codes.OK:
				require.Error(t, err)
				assert.Equal(t, tc.wantCode, status.Code(err))
				assert.NotContains(t, err.Error(), "private-operation-data")
				assert.Nil(t, got)
			default:
				require.NoError(t, err)
				require.Len(t, got, tc.count)
				require.NotNil(t, got)
				if tc.count > 0 {
					assert.Equal(t, taskdef.PowerTaskDetails{
						ID: uuid.UUID{15: 1}, RackID: rackID,
						Operation: operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOn},
						Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
							devicetypes.ComponentTypeCompute: {targetID},
						}},
						Status:    taskcommon.TaskStatusWaiting,
						CreatedAt: createdAt, UpdatedAt: updatedAt, StartedAt: nil,
					}, got[0])
				}
			}
			require.NoError(t, mock.ExpectationsWereMet())
		})
	}
}
