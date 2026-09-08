// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"os"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
)

type testRack struct {
	r *Rack
}

func newTestRack(r Rack) *testRack {
	return &testRack{r: &r}
}

func (tr *testRack) Rack() *Rack {
	return tr.r
}

func (tr *testRack) modifyName(name string) *testRack {
	tr.r.Name = name
	return tr
}

func (tr *testRack) modifyDescription(desc map[string]any) *testRack {
	tr.r.Description = desc
	return tr
}

func (tr *testRack) modifyLocation(location map[string]any) *testRack {
	tr.r.Location = location
	return tr
}

func TestRackBuildPatch(t *testing.T) {
	rackID := uuid.New()
	domainID := uuid.New()
	now := time.Now()

	shareRack := Rack{
		ID:           rackID,
		Name:         "Rack-01",
		Manufacturer: "NVIDIA",
		SerialNumber: "R12345",
		Description:  map[string]any{"type": "compute", "generation": "H100"},
		Location:     map[string]any{"datacenter": "DC1", "row": "A", "position": 1},
		Status:       RackStatusNew,
		NVLDomainID:  domainID,
		CreatedAt:    now,
		UpdatedAt:    now,
	}

	testCases := map[string]struct {
		cur      *Rack
		input    *Rack
		expected *Rack
	}{
		"nil input Rack returns nil": {
			cur:      newTestRack(shareRack).Rack(),
			input:    nil,
			expected: nil,
		},
		"nil current Rack returns nil": {
			cur:      nil,
			input:    newTestRack(shareRack).Rack(),
			expected: nil,
		},
		"both Racks nil returns nil": {
			cur:      nil,
			input:    nil,
			expected: nil,
		},
		"no changes returns nil": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).Rack(),
			expected: nil,
		},
		"name change": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyName("Rack-02").Rack(),
			expected: newTestRack(shareRack).modifyName("Rack-02").Rack(),
		},
		"empty name ignored": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyName("").Rack(),
			expected: nil,
		},
		"same name ignored": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyName("Rack-01").Rack(),
			expected: nil,
		},
		"description change": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyDescription(map[string]any{"type": "storage"}).Rack(), //nolint:gosec
			expected: newTestRack(shareRack).modifyDescription(map[string]any{"type": "storage"}).Rack(), //nolint:gosec
		},
		"description no change": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyDescription(map[string]any{"type": "compute", "generation": "H100"}).Rack(), //nolint:gosec
			expected: nil,
		},
		"description to nil": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyDescription(nil).Rack(),
			expected: nil,
		},
		"location change": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyLocation(map[string]any{"datacenter": "DC2"}).Rack(), //nolint:gosec
			expected: newTestRack(shareRack).modifyLocation(map[string]any{"datacenter": "DC2"}).Rack(), //nolint:gosec
		},
		"location no change": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyLocation(map[string]any{"datacenter": "DC1", "row": "A", "position": 1}).Rack(), //nolint:gosec
			expected: nil,
		},
		"location to nil": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyLocation(nil).Rack(),
			expected: nil,
		},
		"multiple changes": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyName("Rack-03").modifyDescription(map[string]any{"type": "mixed"}).modifyLocation(map[string]any{"datacenter": "DC3"}).Rack(), //nolint:gosec
			expected: newTestRack(shareRack).modifyName("Rack-03").modifyDescription(map[string]any{"type": "mixed"}).modifyLocation(map[string]any{"datacenter": "DC3"}).Rack(), //nolint:gosec
		},
		"mixed valid and invalid changes": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyName("").modifyDescription(map[string]any{"type": "updated"}).modifyLocation(nil).Rack(), //nolint:gosec
			expected: newTestRack(shareRack).modifyDescription(map[string]any{"type": "updated"}).Rack(),                                    //nolint:gosec
		},
		"complex description change": {
			cur: newTestRack(shareRack).Rack(),
			input: newTestRack(shareRack).modifyDescription(map[string]any{
				"type":         "compute",
				"generation":   "H200",
				"specs":        map[string]any{"gpus": 8, "memory": "640GB"},
				"capabilities": []string{"AI", "HPC", "inference"},
			}).Rack(),
			expected: newTestRack(shareRack).modifyDescription(map[string]any{
				"type":         "compute",
				"generation":   "H200",
				"specs":        map[string]any{"gpus": 8, "memory": "640GB"},
				"capabilities": []string{"AI", "HPC", "inference"},
			}).Rack(),
		},
		"complex location change": {
			cur: newTestRack(shareRack).Rack(),
			input: newTestRack(shareRack).modifyLocation(map[string]any{
				"datacenter":  "DC1",
				"building":    "B2",
				"floor":       3,
				"row":         "C",
				"position":    5,
				"coordinates": map[string]any{"lat": 37.7749, "lng": -122.4194},
			}).Rack(),
			expected: newTestRack(shareRack).modifyLocation(map[string]any{
				"datacenter":  "DC1",
				"building":    "B2",
				"floor":       3,
				"row":         "C",
				"position":    5,
				"coordinates": map[string]any{"lat": 37.7749, "lng": -122.4194},
			}).Rack(),
		},
		"name change with whitespace": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyName("  Rack-04  ").Rack(),
			expected: newTestRack(shareRack).modifyName("  Rack-04  ").Rack(),
		},
		"single character name change": {
			cur:      newTestRack(shareRack).Rack(),
			input:    newTestRack(shareRack).modifyName("X").Rack(),
			expected: newTestRack(shareRack).modifyName("X").Rack(),
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			result := tc.input.BuildPatch(tc.cur)
			assert.Equal(t, tc.expected, result)
		})
	}
}

func TestRackForceDelete_Idempotent(t *testing.T) {
	ctx := context.Background()

	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}

	dbConf, err := cdb.ConfigFromEnv()
	assert.Nil(t, err)

	pool, err := utils.UnitTestDB(ctx, t, dbConf)
	assert.Nil(t, err)

	rack := Rack{
		Name:         "fd-rack",
		Manufacturer: "TestMfg",
		SerialNumber: "fd-rack-serial",
	}
	err = rack.Create(ctx, pool.DB)
	assert.Nil(t, err)

	// First ForceDelete succeeds (row exists).
	err = rack.ForceDelete(ctx, pool.DB)
	assert.Nil(t, err)

	// Second ForceDelete on the same ID also succeeds (idempotent).
	err = rack.ForceDelete(ctx, pool.DB)
	assert.Nil(t, err)

	// ForceDelete on a UUID that never existed also succeeds.
	phantom := &Rack{ID: uuid.New()}
	err = phantom.ForceDelete(ctx, pool.DB)
	assert.Nil(t, err)
}

func TestGetRacksByIDsIncludingDeleted(t *testing.T) {
	ctx := context.Background()
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}

	dbConf, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	pool, err := utils.UnitTestDB(ctx, t, dbConf)
	require.NoError(t, err)

	racks := []Rack{
		{Name: "live-rack", Manufacturer: "TestMfg", SerialNumber: "live-rack-serial"},
		{Name: "deleted-rack", Manufacturer: "TestMfg", SerialNumber: "deleted-rack-serial"},
	}
	for i := range racks {
		require.NoError(t, racks[i].Create(ctx, pool.DB))
	}
	require.NoError(t, racks[1].Delete(ctx, pool.DB))

	ids := []uuid.UUID{racks[0].ID, racks[1].ID}
	liveOnly, err := GetRacksByIDs(ctx, pool.DB, ids, false)
	require.NoError(t, err)
	require.Len(t, liveOnly, 1)

	includingDeleted, err := GetRacksByIDsIncludingDeleted(ctx, pool.DB, ids, false)
	require.NoError(t, err)
	require.Len(t, includingDeleted, 2)
}
