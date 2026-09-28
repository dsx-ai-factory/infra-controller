// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package readiness

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/testutil"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/migrations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

func TestDBReader_GetStatusesByManagementMACs(t *testing.T) {
	for _, tc := range []struct {
		name      string
		kind      devicetypes.ComponentType
		coreID    bool
		mutation  string
		wantError string
	}{
		{"compute without Core ID", devicetypes.ComponentTypeCompute, false, "", ""},
		{"compute with Core ID", devicetypes.ComponentTypeCompute, true, "", ""},
		{"switch checks rack host without Core ID", devicetypes.ComponentTypeNVSwitch, false, "", ""},
		{"shelf checks rack host", devicetypes.ComponentTypePowerShelf, false, "", ""},
		{"deleted rack is not empty rack", devicetypes.ComponentTypeNVSwitch, false, "deleted rack", "missing rack"},
		{"mixed missing target", devicetypes.ComponentTypeCompute, false, "missing", "not found"},
		{"DPU MAC is not management identity", devicetypes.ComponentTypeCompute, false, "dpu", "not found"},
		{"deleted target", devicetypes.ComponentTypeCompute, false, "deleted", "not found"},
		{"ambiguous case variants", devicetypes.ComponentTypeCompute, false, "ambiguous", "ambiguous"},
		{"wrong component type", devicetypes.ComponentTypeCompute, false, "wrong type", "unexpected component type"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			// A configured but unreachable database must fail this regression test,
			// not silently skip the query that it is intended to exercise.
			config, err := cdb.ConfigFromEnv()
			require.NoError(t, err)
			session, err := testutil.CreateTestDB(ctx, t, config)
			require.NoError(t, err)
			t.Cleanup(session.Close)
			db := session.DB
			require.NoError(t, migrations.MigrateWithDB(ctx, db, migrations.MigrateOptions{}))
			rack, owner, host := uuid.New(), uuid.New(), uuid.New()
			_, err = db.ExecContext(ctx, "INSERT INTO rack (id, name) VALUES (?, ?)", rack, rack.String())
			require.NoError(t, err)
			status, err := json.Marshal(inUseStatus())
			require.NoError(t, err)
			var externalID any
			var targetStatus any
			if tc.kind == devicetypes.ComponentTypeCompute {
				targetStatus = string(status)
			}
			if tc.coreID {
				externalID = "core-1"
			}
			_, err = db.ExecContext(ctx, "INSERT INTO component (id, type, rack_id, external_id, status) VALUES (?, ?, ?, ?, ?::jsonb), (?, 'Compute', ?, NULL, ?::jsonb)", owner, devicetypes.ComponentTypeToString(tc.kind), rack, externalID, targetStatus, host, rack, string(status))
			require.NoError(t, err)
			_, err = db.ExecContext(ctx, "INSERT INTO bmc (mac_address, type, component_id) VALUES ('AA:BB:CC:DD:EE:01', 'Host', ?)", owner)
			require.NoError(t, err)
			macs := []string{"aa:bb:cc:dd:ee:01"}
			switch tc.mutation {
			case "deleted rack":
				_, err = db.ExecContext(ctx, "UPDATE rack SET deleted_at = now() WHERE id = ?", rack)
			case "missing":
				macs = append(macs, "aa:bb:cc:dd:ee:02")
			case "dpu":
				_, err = db.ExecContext(ctx, "UPDATE bmc SET type = 'DPU'")
			case "deleted":
				_, err = db.ExecContext(ctx, "UPDATE component SET deleted_at = now() WHERE id = ?", owner)
			case "ambiguous":
				_, err = db.ExecContext(ctx, "INSERT INTO bmc (mac_address, type, component_id) VALUES ('aa:bb:cc:dd:ee:01', 'Host', ?)", host)
			case "wrong type":
				_, err = db.ExecContext(ctx, "UPDATE component SET type = 'PowerShelf' WHERE id = ?", owner)
			}
			require.NoError(t, err)
			reader := NewDBReader(db)
			got, err := reader.GetStatusesByManagementMACs(ctx, tc.kind, macs)
			if tc.wantError != "" {
				require.ErrorContains(t, err, tc.wantError)
				return
			}
			require.NoError(t, err)
			require.NotEmpty(t, got[macs[0]])
			if tc.coreID {
				byID, err := reader.GetStatusesByExternalIDs(ctx, []string{"core-1"})
				require.NoError(t, err)
				require.Equal(t, byID["core-1"], got[macs[0]][0])
			}
			for _, status := range got[macs[0]] {
				require.True(t, status.Blocks(types.OperationTypePowerControl))
			}
			// Clearing status must be observed on the next lookup, without needing
			// a Core ID or a new workflow payload.
			_, err = db.ExecContext(ctx, "UPDATE component SET status = NULL")
			require.NoError(t, err)
			got, err = reader.GetStatusesByManagementMACs(ctx, tc.kind, macs)
			require.NoError(t, err)
			for _, status := range got[macs[0]] {
				require.Nil(t, status)
			}
		})
	}
}

func TestDBGate_WaitForManagementMACsReady(t *testing.T) {
	t.Run("polling", testMACPolling)
	for _, tc := range []struct {
		name    string
		status  *types.ComponentOperationStatus
		blocked bool
	}{
		{"blocked", inUseStatus(), true}, {"ready", readyStatus(), false}, {"unknown status preserves fail open", nil, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			reader := NewMemReader()
			reader.SetStatus("mac-1", tc.status)
			gate := NewDBGate(reader, time.Millisecond, time.Millisecond)
			err := gate.WaitForManagementMACsReady(context.Background(), devicetypes.ComponentTypeCompute, []string{"mac-1"}, types.OperationTypePowerControl)
			if tc.blocked {
				require.ErrorContains(t, err, "timed out")
				return
			}
			require.NoError(t, err)
		})
	}
}

type changingMACReader struct {
	*MemReader
	calls int
	err   error
}

func (r *changingMACReader) GetStatusesByManagementMACs(_ context.Context, _ devicetypes.ComponentType, macs []string) (map[string][]*types.ComponentOperationStatus, error) {
	r.calls++
	if r.err != nil {
		return nil, r.err
	}
	status := inUseStatus()
	if r.calls > 1 {
		status = readyStatus()
	}
	return map[string][]*types.ComponentOperationStatus{macs[0]: {status}}, nil
}

func testMACPolling(t *testing.T) {
	for _, tc := range []struct {
		name      string
		err       error
		wantCalls int
	}{
		{"refreshes status after blocked poll", nil, 2},
		{"lookup failure never becomes ready", errors.New("database unavailable"), 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			reader := &changingMACReader{MemReader: NewMemReader(), err: tc.err}
			gate := NewDBGate(reader, time.Second, time.Millisecond)
			err := gate.WaitForManagementMACsReady(context.Background(), devicetypes.ComponentTypeCompute, []string{"mac-1"}, types.OperationTypePowerControl)
			if tc.err != nil {
				require.ErrorIs(t, err, tc.err)
			} else {
				require.NoError(t, err)
			}
			require.Equal(t, tc.wantCalls, reader.calls)
		})
	}
}
