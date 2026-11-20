// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"encoding/json"
	"os"
	"testing"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/logcorrelation"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/stretchr/testify/require"
)

func TestPostgresStore_LogCorrelation(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("DB_PORT is required")
	}
	ctx := t.Context()
	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	pool, err := utils.UnitTestDB(ctx, t, config)
	require.NoError(t, err)
	store := NewPostgresStore(pool)
	template, err := json.Marshal(operationrun.Operation{Payload: &operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOn}})
	require.NoError(t, err)
	for _, tc := range []struct {
		name     string
		metadata *logcorrelation.Metadata
	}{
		{"legacy", nil},
		{"correlated", &logcorrelation.Metadata{TraceID: "1234567890abcdef1234567890abcdef"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			run := &operationrun.OperationRun{
				Name: tc.name, Metadata: tc.metadata, TotalPhases: 1,
				Selector: json.RawMessage(`{}`), Options: json.RawMessage(`{}`),
				OperationTemplate: template, OperationType: taskcommon.TaskTypePowerControl,
				OperationCode: "power_on",
			}
			id, err := store.Create(ctx, run)
			require.NoError(t, err)
			loaded, err := store.Get(ctx, id)
			require.NoError(t, err)
			require.Equal(t, tc.metadata, loaded.Metadata)
			op, err := loaded.DecodedOperation()
			require.NoError(t, err)
			req, err := operationrun.TaskRequestForTarget(op, &operationrun.OperationRunTarget{})
			require.NoError(t, err)
			if tc.metadata == nil {
				require.Empty(t, req.TraceID)
			} else {
				require.Equal(t, tc.metadata.TraceID, req.TraceID)
			}
		})
	}
}
