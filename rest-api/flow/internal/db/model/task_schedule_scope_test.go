// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"encoding/json"
	"os"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
)

// fixed UUIDs for deterministic test output.
var (
	compA = uuid.MustParse("aaaaaaaa-0000-0000-0000-000000000001")
	compB = uuid.MustParse("bbbbbbbb-0000-0000-0000-000000000002")
	compC = uuid.MustParse("cccccccc-0000-0000-0000-000000000003")
)

func mustMarshalFilter(t *testing.T, cf *ComponentFilter) json.RawMessage {
	t.Helper()
	raw, err := MarshalComponentFilter(cf)
	require.NoError(t, err)
	return raw
}

// ── MarshalComponentFilter ────────────────────────────────────────────────────

func TestMarshalComponentFilter(t *testing.T) {
	cases := map[string]struct {
		input   *ComponentFilter
		wantNil bool
		want    string
		wantErr bool
	}{
		"nil returns nil": {
			input:   nil,
			wantNil: true,
		},
		"types filter": {
			input: &ComponentFilter{
				Kind:  ComponentFilterKindTypes,
				Types: []string{"COMPUTE", "POWERSHELF"},
			},
			want: `{"kind":"types","types":["COMPUTE","POWERSHELF"]}`,
		},
		"components filter": {
			input: &ComponentFilter{
				Kind:       ComponentFilterKindComponents,
				Components: []uuid.UUID{compA, compB},
			},
			want: `{"kind":"components","components":["aaaaaaaa-0000-0000-0000-000000000001","bbbbbbbb-0000-0000-0000-000000000002"]}`,
		},

		// Validation error cases.
		"unknown kind": {
			input:   &ComponentFilter{Kind: "unknown", Types: []string{"COMPUTE"}},
			wantErr: true,
		},
		"types: empty types slice": {
			input:   &ComponentFilter{Kind: ComponentFilterKindTypes},
			wantErr: true,
		},
		"types: components also set": {
			input: &ComponentFilter{
				Kind:       ComponentFilterKindTypes,
				Types:      []string{"COMPUTE"},
				Components: []uuid.UUID{compA},
			},
			wantErr: true,
		},
		"components: empty components slice": {
			input:   &ComponentFilter{Kind: ComponentFilterKindComponents},
			wantErr: true,
		},
		"components: types also set": {
			input: &ComponentFilter{
				Kind:       ComponentFilterKindComponents,
				Components: []uuid.UUID{compA},
				Types:      []string{"COMPUTE"},
			},
			wantErr: true,
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			raw, err := MarshalComponentFilter(tc.input)
			if tc.wantErr {
				require.Error(t, err)
				assert.Nil(t, raw)
				return
			}
			require.NoError(t, err)
			if tc.wantNil {
				assert.Nil(t, raw)
			} else {
				assert.JSONEq(t, tc.want, string(raw))
			}
		})
	}
}

// ── UnmarshalComponentFilter ──────────────────────────────────────────────────

func TestUnmarshalComponentFilter(t *testing.T) {
	cases := map[string]struct {
		input   json.RawMessage
		wantNil bool
		want    *ComponentFilter
		wantErr bool
	}{
		"nil returns nil": {
			input:   nil,
			wantNil: true,
		},
		"empty returns nil": {
			input:   json.RawMessage{},
			wantNil: true,
		},
		"json null returns nil": {
			input:   json.RawMessage(`null`),
			wantNil: true,
		},
		"types filter": {
			input: json.RawMessage(`{"kind":"types","types":["COMPUTE","POWERSHELF"]}`),
			want: &ComponentFilter{
				Kind:  ComponentFilterKindTypes,
				Types: []string{"COMPUTE", "POWERSHELF"},
			},
		},
		"components filter": {
			input: json.RawMessage(`{"kind":"components","components":["aaaaaaaa-0000-0000-0000-000000000001","bbbbbbbb-0000-0000-0000-000000000002"]}`),
			want: &ComponentFilter{
				Kind:       ComponentFilterKindComponents,
				Components: []uuid.UUID{compA, compB},
			},
		},
		"invalid JSON returns error": {
			input:   json.RawMessage(`not-json`),
			wantErr: true,
		},

		// Validation error cases: valid JSON but invalid discriminated-union state.
		"unknown kind": {
			input:   json.RawMessage(`{"kind":"unknown","types":["COMPUTE"]}`),
			wantErr: true,
		},
		"types: empty types array": {
			input:   json.RawMessage(`{"kind":"types","types":[]}`),
			wantErr: true,
		},
		"types: components also set": {
			input:   json.RawMessage(`{"kind":"types","types":["COMPUTE"],"components":["aaaaaaaa-0000-0000-0000-000000000001"]}`),
			wantErr: true,
		},
		"components: empty components array": {
			input:   json.RawMessage(`{"kind":"components","components":[]}`),
			wantErr: true,
		},
		"components: types also set": {
			input:   json.RawMessage(`{"kind":"components","components":["aaaaaaaa-0000-0000-0000-000000000001"],"types":["COMPUTE"]}`),
			wantErr: true,
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := UnmarshalComponentFilter(tc.input)
			if tc.wantErr {
				require.Error(t, err)
				assert.Nil(t, got)
				return
			}
			require.NoError(t, err)
			if tc.wantNil {
				assert.Nil(t, got)
			} else {
				assert.Equal(t, tc.want, got)
			}
		})
	}
}

// ── ComponentFilterEqual ──────────────────────────────────────────────────────

func TestComponentFilterEqual(t *testing.T) {
	typesAB := mustMarshalFilter(t, &ComponentFilter{Kind: ComponentFilterKindTypes, Types: []string{"COMPUTE", "POWERSHELF"}})
	typesBA := mustMarshalFilter(t, &ComponentFilter{Kind: ComponentFilterKindTypes, Types: []string{"POWERSHELF", "COMPUTE"}})
	typesA := mustMarshalFilter(t, &ComponentFilter{Kind: ComponentFilterKindTypes, Types: []string{"COMPUTE"}})
	typesC := mustMarshalFilter(t, &ComponentFilter{Kind: ComponentFilterKindTypes, Types: []string{"NVSwitch"}})

	compsAB := mustMarshalFilter(t, &ComponentFilter{Kind: ComponentFilterKindComponents, Components: []uuid.UUID{compA, compB}})
	compsBA := mustMarshalFilter(t, &ComponentFilter{Kind: ComponentFilterKindComponents, Components: []uuid.UUID{compB, compA}})
	compsA := mustMarshalFilter(t, &ComponentFilter{Kind: ComponentFilterKindComponents, Components: []uuid.UUID{compA}})
	compsC := mustMarshalFilter(t, &ComponentFilter{Kind: ComponentFilterKindComponents, Components: []uuid.UUID{compC}})

	cases := map[string]struct {
		a, b    json.RawMessage
		want    bool
		wantErr bool
	}{
		// Quick-path: nil / empty checks (no unmarshaling).
		"both nil":       {a: nil, b: nil, want: true},
		"both empty":     {a: json.RawMessage{}, b: json.RawMessage{}, want: true},
		"nil vs empty":   {a: nil, b: json.RawMessage{}, want: true},
		"nil vs non-nil": {a: nil, b: typesAB, want: false},
		"non-nil vs nil": {a: typesAB, b: nil, want: false},
		"byte-identical": {a: typesAB, b: typesAB, want: true},

		// Types filter.
		"types: same order":      {a: typesAB, b: typesAB, want: true},
		"types: different order": {a: typesAB, b: typesBA, want: true},
		"types: subset":          {a: typesAB, b: typesA, want: false},
		"types: disjoint":        {a: typesA, b: typesC, want: false},

		// Components filter.
		"components: same order":      {a: compsAB, b: compsAB, want: true},
		"components: different order": {a: compsAB, b: compsBA, want: true},
		"components: subset":          {a: compsAB, b: compsA, want: false},
		"components: disjoint":        {a: compsA, b: compsC, want: false},

		// Mixed kinds.
		"types vs components": {a: typesA, b: compsA, want: false},

		// Unknown kind — rejected by UnmarshalComponentFilter validation.
		"unknown kind": {
			a:       json.RawMessage(`{"kind":"unknown","types":["A"]}`),
			b:       json.RawMessage(`{"kind":"unknown","types":["B"]}`),
			wantErr: true,
		},

		// Invalid JSON.
		"invalid a": {a: json.RawMessage(`bad`), b: typesAB, wantErr: true},
		"invalid b": {a: typesAB, b: json.RawMessage(`bad`), wantErr: true},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := ComponentFilterEqual(tc.a, tc.b)
			if tc.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.want, got)
		})
	}
}

// ── Bun nullzero serialization ────────────────────────────────────────────────

// TestNilComponentFilterIsNULL verifies that a nil ComponentFilter (meaning
// "target all components in the rack") is persisted as SQL NULL by PostgreSQL,
// not as the JSON null literal 'null'.
//
// With the nullzero tag, bun emits DEFAULT for a nil json.RawMessage column.
// PostgreSQL resolves DEFAULT to NULL for a nullable column with no explicit
// column default, so the round-trip value is SQL NULL (Go nil), not 'null'.
//
// This is an integration test that requires a real PostgreSQL instance.
// Set DB_PORT (and optionally DB_HOST, DB_NAME, DB_USER, DB_PASSWORD) to enable it.
func TestNilComponentFilterIsNULL(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}

	ctx := context.Background()

	dbConf, err := cdb.ConfigFromEnv()
	require.NoError(t, err)

	pool, err := utils.UnitTestDB(ctx, t, dbConf)
	require.NoError(t, err)

	// Insert a minimal rack to satisfy the FK on task_schedule_scope.rack_id.
	rack := &Rack{
		Name:         "nullzero-test-rack",
		Manufacturer: "test-mfg",
		SerialNumber: uuid.New().String(), // unique per run
	}
	err = rack.Create(ctx, pool.DB)
	require.NoError(t, err)

	// Insert a minimal task_schedule to satisfy the FK on task_schedule_scope.schedule_id.
	sched := &TaskSchedule{
		Name:              "nullzero-test-schedule-" + uuid.New().String(),
		SpecType:          SpecTypeInterval,
		Spec:              "1h",
		Timezone:          "UTC",
		OperationTemplate: json.RawMessage(`{"type":"power_on"}`),
		OverlapPolicy:     OverlapPolicySkip,
		Enabled:           true,
	}
	_, err = pool.DB.NewInsert().Model(sched).Returning("id").Exec(ctx, &sched.ID)
	require.NoError(t, err)

	// Insert the scope with a nil ComponentFilter.
	scope := &TaskScheduleScope{
		ScheduleID:      sched.ID,
		RackID:          rack.ID,
		ComponentFilter: nil,
	}
	_, err = pool.DB.NewInsert().Model(scope).Returning("id").Exec(ctx, &scope.ID)
	require.NoError(t, err)

	// Read back the raw component_filter value via the pg driver.
	// If the nullzero tag is working correctly, PostgreSQL stored SQL NULL,
	// so Scan into *[]byte will leave it nil.
	var raw []byte
	err = pool.DB.QueryRowContext(ctx,
		"SELECT component_filter FROM task_schedule_scope WHERE id = ?", scope.ID,
	).Scan(&raw)
	require.NoError(t, err)

	assert.Nil(t, raw, "component_filter should be SQL NULL, not the JSON null literal")
}
