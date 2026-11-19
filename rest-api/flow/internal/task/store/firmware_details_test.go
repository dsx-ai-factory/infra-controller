// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"bytes"
	"context"
	"encoding/json"
	stderrors "errors"
	"os"
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
	"github.com/uptrace/bun/extra/bunotel"
	"go.opentelemetry.io/otel/metric/noop"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	dbtestutil "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/testutil"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/migrations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// TestFirmwareTaskDetailsFromDAO proves the safe projection preserves stored
// selections and timestamps, while corrupt operation data fails without secrets.
func TestFirmwareTaskDetailsFromDAO(t *testing.T) {
	created := time.Date(2030, 1, 2, 3, 4, 5, 123456789, time.UTC)
	updated, started := created.Add(time.Minute), created.Add(time.Second)
	first, second, shelf := uuid.UUID{15: 1}, uuid.UUID{15: 2}, uuid.UUID{15: 3}
	for _, tc := range []struct {
		name        string
		typ         taskcommon.TaskType
		information string
		want        taskdef.FirmwareTaskDetails
		stage       string
	}{
		// Populated values, including encrypted authentication, must project
		// exactly without returning private operation/report fields.
		{name: "explicit fields and private payload", typ: taskcommon.TaskTypeFirmwareControl,
			information: `{"operation":1,` +
				`"target_version":"{\"Id\":\"fw-safe\",\"auth\":\"private-operation-data\"}",` +
				`"sub_targets":["bmc","dpu"],"override_readiness_check":true,` +
				`"override_version_check":true,"start_time":123,"end_time":456,` +
				`"authentication_data":{"version":1,"key_id":"private-operation-data",` +
				`"ciphertext":"cHJpdmF0ZS1vcGVyYXRpb24tZGF0YQ=="}}`,
			want: taskdef.FirmwareTaskDetails{
				Operation:              operations.FirmwareOperationUpgrade,
				RequestedTargets:       taskdef.FirmwareRequestedTargets{Shared: &taskdef.FirmwareTargetMetadata{Kind: taskdef.FirmwareTargetManifest, Value: "fw-safe"}},
				SubTargets:             []string{"bmc", "dpu"},
				OverrideReadinessCheck: true,
				OverrideVersionCheck:   true,
				StartTime:              123,
				EndTime:                456,
			}},
		// Omission retains false flags, no subtargets, and the default request.
		{name: "omitted flags and targets retain zero values", typ: taskcommon.TaskTypeFirmwareControl, information: `{"operation":3}`,
			want: taskdef.FirmwareTaskDetails{
				Operation:        operations.FirmwareOperationRollback,
				RequestedTargets: taskdef.FirmwareRequestedTargets{Shared: &taskdef.FirmwareTargetMetadata{Kind: taskdef.FirmwareTargetConfiguredDefault}},
			}},
		// Unavailable/absent per-type metadata must not remove task identity or
		// recorded selections. Unequal flags also detect swapped field mappings.
		{name: "partial metadata retains all selections and distinct flags", typ: taskcommon.TaskTypeFirmwareControl,
			information: `{"operation":2,"target_version":"{\"compute\":{` +
				`\"ProductName\":\"private-operation-data\",\"Milestones\":[]},` +
				`\"nvswitch\":{\"Id\":\"switch-fw\"}}",` +
				`"override_readiness_check":true,"override_version_check":false}`,
			want: taskdef.FirmwareTaskDetails{
				Operation: operations.FirmwareOperationDowngrade,
				RequestedTargets: taskdef.FirmwareRequestedTargets{ByType: map[devicetypes.ComponentType]taskdef.FirmwareTargetMetadata{
					devicetypes.ComponentTypeCompute:  {Kind: taskdef.FirmwareTargetUnavailable},
					devicetypes.ComponentTypeNVSwitch: {Kind: taskdef.FirmwareTargetManifest, Value: "switch-fw"},
				}},
				OverrideReadinessCheck: true,
			}},
		// This decoder error includes its numeric input, unlike an enum error.
		{name: "corrupt operation hides decoder input", typ: taskcommon.TaskTypeFirmwareControl, information: `{"operation":987654321098765432109876543210}`, stage: "operation_decode"},
		// FirmwareVersion is representable but is not a schedulable operation.
		{name: "unsupported version operation cannot become upgrade", typ: taskcommon.TaskTypeFirmwareControl, information: `{"operation":4}`, stage: "operation_validation"},
		// The private converter must not reinterpret another task payload.
		{name: "other task kind is rejected", typ: taskcommon.TaskTypePowerControl, information: `{"operation":1}`, stage: "operation_type"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var diagnostics bytes.Buffer
			previous := log.Logger
			log.Logger = zerolog.New(&diagnostics)
			t.Cleanup(func() { log.Logger = previous })
			row := model.Task{
				ID:          uuid.New(),
				RackID:      uuid.New(),
				Type:        tc.typ,
				Information: json.RawMessage(tc.information),
				Status:      taskcommon.TaskStatusRunning,
				CreatedAt:   created,
				UpdatedAt:   updated,
				StartedAt:   &started,
				Description: "private-operation-data",
				Report:      json.RawMessage(`{"private":"private-operation-data"}`),
				Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
					devicetypes.ComponentTypeCompute:    {second, first, second},
					devicetypes.ComponentTypePowerShelf: {shelf},
					devicetypes.ComponentTypeNVSwitch:   {},
				}},
			}
			if tc.stage == "operation_decode" {
				_, decodeErr := operations.New(tc.typ, row.Information)
				require.ErrorContains(t, decodeErr, "987654321098765432109876543210")
			}

			got, err := firmwareTaskDetailsFromDAO(row)

			assert.NotContains(t, diagnostics.String(), "private-operation-data")
			assert.NotContains(t, diagnostics.String(), "987654321098765432109876543210")
			if tc.stage != "" {
				require.Error(t, err)
				assert.Equal(t, codes.Internal, status.Code(err))
				assert.Contains(t, err.Error(), row.ID.String())
				assert.NotContains(t, err.Error(), "private-operation-data")
				assert.NotContains(t, err.Error(), "987654321098765432109876543210")
				assert.Equal(t, taskdef.FirmwareTaskDetails{}, got)
				var fields map[string]any
				require.NoError(t, json.Unmarshal(diagnostics.Bytes(), &fields))
				assert.Equal(t, row.ID.String(), fields["task_id"])
				assert.Equal(t, row.RackID.String(), fields["rack_id"])
				assert.Equal(t, tc.stage, fields["failure_stage"])
				return
			}
			require.NoError(t, err)
			assert.Empty(t, diagnostics.String())
			want := tc.want
			want.ID = row.ID
			want.RackID = row.RackID
			want.Attributes = taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypeCompute:    {first, second},
				devicetypes.ComponentTypePowerShelf: {shelf},
				devicetypes.ComponentTypeNVSwitch:   {},
			}}
			want.Status = row.Status
			want.CreatedAt = created
			want.UpdatedAt = updated
			want.StartedAt = &started
			assert.Equal(t, want, got)
			encoded, err := json.Marshal(got)
			require.NoError(t, err)
			assert.NotContains(t, string(encoded), "private-operation-data")
			assert.NotContains(t, string(encoded), "cHJpdmF0ZS1vcGVyYXRpb24tZGF0YQ==")
		})
	}
}

// TestPostgresStore_ListFirmwareTasksForRack protects the store failure
// boundary and proves the read sees uncommitted work in its caller transaction.
func TestPostgresStore_ListFirmwareTasksForRack(t *testing.T) {
	for _, tc := range []struct {
		name              string
		count             int
		invalidLast       bool
		invalidAttributes bool
		queryError        error
		cancel            bool
		overflow          bool
		stage             string
	}{
		// One query supplies safe intent without resolving inventory.
		{name: "one query returns safe details", count: 1},
		// Empty is a complete collection rather than an unavailable read.
		{name: "empty read is complete"},
		// A later invalid row must discard the earlier successfully converted row.
		{name: "later conversion failure discards earlier tasks", count: 2, invalidLast: true, stage: "operation_validation"},
		// This injected driver message tests store logs/response sanitization only.
		{name: "driver diagnostics omit messages", queryError: &pgconn.PgError{Code: "42703", Message: "private-store-data", Detail: "private-store-data"}, stage: "database_read"},
		// Real attribute decoding quotes its unknown key; exercise bunotel too.
		{name: "scan errors omit stored attributes", count: 1, invalidAttributes: true, stage: "database_read"},
		// Callers need the typed limit to distinguish overflow from an outage.
		{name: "overflow remains typed", count: 257, overflow: true},
		// Cancel after query entry so the driver returns a non-context error.
		{name: "driver cancellation preserves caller error", cancel: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var diagnostics bytes.Buffer
			previous := log.Logger
			log.Logger = zerolog.New(&diagnostics)
			t.Cleanup(func() { log.Logger = previous })
			ctx, cancel := context.WithCancel(t.Context())
			defer cancel()
			enteredQuery := false
			matcher := sqlmock.QueryMatcherFunc(func(expected, actual string) error {
				matchErr := sqlmock.QueryMatcherRegexp.Match(expected, actual)
				if matchErr != nil {
					return matchErr
				}
				enteredQuery = true
				if tc.cancel {
					cancel()
				}
				return nil
			})
			sqlDB, mock, err := sqlmock.New(sqlmock.QueryMatcherOption(matcher))
			require.NoError(t, err)
			db := bun.NewDB(sqlDB, pgdialect.New())
			var recorder *tracetest.SpanRecorder
			if tc.invalidAttributes {
				recorder = tracetest.NewSpanRecorder()
				provider := sdktrace.NewTracerProvider(sdktrace.WithSampler(sdktrace.AlwaysSample()), sdktrace.WithSpanProcessor(recorder))
				t.Cleanup(func() { require.NoError(t, provider.Shutdown(context.Background())) })
				db.AddQueryHook(bunotel.NewQueryHook(bunotel.WithTracerProvider(provider), bunotel.WithMeterProvider(noop.NewMeterProvider())))
			}
			t.Cleanup(func() {
				mock.ExpectClose()
				require.NoError(t, db.Close())
			})
			rackID, targetID := uuid.New(), uuid.New()
			created := time.Date(2030, 1, 2, 3, 4, 5, 0, time.UTC)
			expectation := mock.ExpectQuery(`FROM "task" AS "t".* LIMIT 257$`)
			if tc.cancel {
				expectation.WillReturnError(&pgconn.PgError{Code: "57014", Message: "canceling query due to user request"})
			} else if tc.queryError != nil {
				expectation.WillReturnError(tc.queryError)
			} else {
				rows := sqlmock.NewRows([]string{"id", "type", "rack_id", "information", "attributes", "status", "created_at", "updated_at", "started_at"})
				for i := range tc.count {
					information := `{"operation":1}`
					if tc.invalidLast && i == tc.count-1 {
						information = `{"operation":4}`
					}
					attributes := `{"components_by_type":{"Compute":["` + targetID.String() + `"]}}`
					if tc.invalidAttributes {
						attributes = `{"components_by_type":{"private-store-data":[]}}`
						// Prove the injected scan error can quote the private key.
						var decoded taskcommon.TaskAttributes
						decodeErr := json.Unmarshal([]byte(attributes), &decoded)
						require.ErrorContains(t, decodeErr, "private-store-data")
					}
					rows.AddRow(uuid.UUID{14: byte((i + 1) >> 8), 15: byte(i + 1)}, taskcommon.TaskTypeFirmwareControl, rackID,
						information, attributes, taskcommon.TaskStatusWaiting, created, created, nil)
				}
				expectation.WillReturnRows(rows).RowsWillBeClosed()
			}

			got, err := NewPostgres(&cdb.Session{DB: db}).ListFirmwareTasksForRack(ctx, rackID)

			assert.True(t, enteredQuery)
			if recorder != nil {
				spans := recorder.Ended()
				require.Len(t, spans, 1, "positive control: the production bunotel hook recorded the SELECT")
				assert.Equal(t, "SELECT", spans[0].Name())
				assert.NotContains(t, spans[0].Status().Description, "private-store-data")
				for _, payload := range []any{spans[0].Events(), spans[0].Attributes()} {
					encoded, encodeErr := json.Marshal(payload)
					require.NoError(t, encodeErr)
					assert.NotContains(t, string(encoded), "private-store-data")
				}
			}
			assert.NotContains(t, diagnostics.String(), "private-store-data")
			if tc.stage != "" {
				require.Error(t, err)
				assert.Equal(t, codes.Internal, status.Code(err))
				assert.NotContains(t, err.Error(), "private-store-data")
				assert.Nil(t, got)
				var fields map[string]any
				require.NoError(t, json.Unmarshal(diagnostics.Bytes(), &fields))
				assert.Equal(t, rackID.String(), fields["rack_id"])
				assert.Equal(t, tc.stage, fields["failure_stage"])
				assert.NotEmpty(t, fields["error_type"])
				if tc.invalidLast {
					assert.Equal(t, (uuid.UUID{15: 2}).String(), fields["task_id"])
				}
				if tc.queryError != nil {
					assert.Equal(t, "42703", fields["sqlstate"])
				}
			} else {
				assert.Empty(t, diagnostics.String())
				switch {
				case tc.cancel:
					require.ErrorIs(t, err, context.Canceled)
					assert.Nil(t, got)
				case tc.overflow:
					var limitErr *taskcommon.FirmwareTaskLimitError
					require.ErrorAs(t, err, &limitErr)
					assert.Equal(t, rackID, limitErr.RackID)
					assert.Nil(t, got)
				default:
					require.NoError(t, err)
					require.NotNil(t, got)
					require.Len(t, got, tc.count)
					if tc.count > 0 {
						assert.Equal(t, taskdef.FirmwareTaskDetails{
							ID:               uuid.UUID{15: 1},
							RackID:           rackID,
							Operation:        operations.FirmwareOperationUpgrade,
							RequestedTargets: taskdef.FirmwareRequestedTargets{Shared: &taskdef.FirmwareTargetMetadata{Kind: taskdef.FirmwareTargetConfiguredDefault}},
							Attributes:       taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypeCompute: {targetID}}},
							Status:           taskcommon.TaskStatusWaiting,
							CreatedAt:        created,
							UpdatedAt:        created,
						}, got[0])
					}
				}
			}
			require.NoError(t, mock.ExpectationsWereMet())
		})
	}

	// This separate setup proves transaction visibility and rollback, rather
	// than repeating the query/projection cases against PostgreSQL.
	t.Run("reads uncommitted task then rollback removes it", func(t *testing.T) {
		if os.Getenv("DB_PORT") == "" {
			t.Skip("Skipping PostgreSQL firmware transaction test: no DB environment specified")
		}
		config, err := cdb.ConfigFromEnv()
		require.NoError(t, err)
		// CreateTestDB returns configured connection failures; UnitTestDB skips them.
		session, err := dbtestutil.CreateTestDB(t.Context(), t, config)
		require.NoError(t, err)
		t.Cleanup(session.Close)
		require.NoError(t, migrations.MigrateWithDB(t.Context(), session.DB, migrations.MigrateOptions{}))
		store := NewPostgres(session)
		rackID, taskID := uuid.New(), uuid.New()
		rollback := stderrors.New("rollback fixture")
		err = store.RunInTransaction(t.Context(), func(ctx context.Context) error {
			row := model.Task{
				ID:           taskID,
				RackID:       rackID,
				Type:         taskcommon.TaskTypeFirmwareControl,
				Status:       taskcommon.TaskStatusWaiting,
				ExecutorType: taskcommon.ExecutorTypeTemporal,
				Information:  json.RawMessage(`{"operation":1}`),
			}
			createErr := row.Create(ctx, store.idb(ctx))
			require.NoError(t, createErr)

			got, readErr := store.ListFirmwareTasksForRack(ctx, rackID)

			require.NoError(t, readErr)
			require.Len(t, got, 1)
			assert.Equal(t, taskID, got[0].ID)
			assert.Empty(t, got[0].Attributes.ComponentsByType)
			return rollback
		})
		require.ErrorIs(t, err, rollback)
		got, err := store.ListFirmwareTasksForRack(t.Context(), rackID)
		require.NoError(t, err)
		assert.Empty(t, got)
	})
}
