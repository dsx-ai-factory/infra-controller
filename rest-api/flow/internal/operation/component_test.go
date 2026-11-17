// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operation

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestComponentFilterEqualIgnoresOrder(t *testing.T) {
	compA := uuid.New()
	compB := uuid.New()

	typesAB := mustMarshalComponentFilter(t, &ComponentFilter{
		Kind:  ComponentFilterKindTypes,
		Types: []string{"COMPUTE", "POWERSHELF"},
	})
	typesBA := mustMarshalComponentFilter(t, &ComponentFilter{
		Kind:  ComponentFilterKindTypes,
		Types: []string{"POWERSHELF", "COMPUTE"},
	})
	compsAB := mustMarshalComponentFilter(t, &ComponentFilter{
		Kind:       ComponentFilterKindComponents,
		Components: []uuid.UUID{compA, compB},
	})
	compsBA := mustMarshalComponentFilter(t, &ComponentFilter{
		Kind:       ComponentFilterKindComponents,
		Components: []uuid.UUID{compB, compA},
	})

	equal, err := ComponentFilterEqual(typesAB, typesBA)
	require.NoError(t, err)
	assert.True(t, equal)

	equal, err = ComponentFilterEqual(compsAB, compsBA)
	require.NoError(t, err)
	assert.True(t, equal)
}

func TestComponentFilterValidateRejectsNilComponentUUID(t *testing.T) {
	filter := &ComponentFilter{
		Kind:       ComponentFilterKindComponents,
		Components: []uuid.UUID{uuid.Nil},
	}

	err := filter.Validate()

	require.ErrorContains(
		t,
		err,
		`component filter kind "components" components[0] is required`,
	)
}

func TestSliceSetEqual(t *testing.T) {
	cases := map[string]struct {
		a, b []string
		want bool
	}{
		"both empty":          {a: nil, b: nil, want: true},
		"same order":          {a: []string{"x", "y"}, b: []string{"x", "y"}, want: true},
		"different order":     {a: []string{"x", "y"}, b: []string{"y", "x"}, want: true},
		"different lengths":   {a: []string{"x", "y"}, b: []string{"x"}, want: false},
		"disjoint":            {a: []string{"x"}, b: []string{"y"}, want: false},
		"duplicate in a vs b": {a: []string{"x", "x"}, b: []string{"x", "y"}, want: false},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, tc.want, sliceSetEqual(tc.a, tc.b))
		})
	}
}

func TestComponentsByTypeNormalize(t *testing.T) {
	id1 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000001")
	id2 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000002")
	id3 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000003")

	components := ComponentsByType{
		devicetypes.ComponentTypePowerShelf: {id3, id3},
		devicetypes.ComponentTypeCompute:    {id2, id1, id2},
	}

	normalized, err := components.Normalize()
	require.NoError(t, err)

	require.Equal(t, ComponentsByType{
		devicetypes.ComponentTypeCompute:    {id2, id1},
		devicetypes.ComponentTypePowerShelf: {id3},
	}, normalized)
	require.Equal(t, []uuid.UUID{id2, id1, id2}, components[devicetypes.ComponentTypeCompute])
}

func TestComponentsByTypeValidateRejectsDuplicateComponentUUID(t *testing.T) {
	id1 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000001")

	components := ComponentsByType{
		devicetypes.ComponentTypeCompute: {id1, id1},
	}

	err := components.Validate()
	require.ErrorContains(t, err, "Compute duplicates component")

	normalized, err := components.Normalize()
	require.NoError(t, err)
	require.Equal(t, ComponentsByType{
		devicetypes.ComponentTypeCompute: {id1},
	}, normalized)
}

func TestComponentsByTypeRejectsInvalidInput(t *testing.T) {
	id1 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000001")

	cases := map[string]struct {
		components ComponentsByType
		wantErr    string
	}{
		"empty": {
			components: nil,
			wantErr:    "Non-empty ComponentsByType is required",
		},
		"unknown type": {
			components: ComponentsByType{
				devicetypes.ComponentTypeUnknown: {id1},
			},
			wantErr: "ComponentsByType contains unknown component type",
		},
		"empty component list": {
			components: ComponentsByType{
				devicetypes.ComponentTypeCompute: nil,
			},
			wantErr: "Compute must include at least one component",
		},
		"nil component UUID": {
			components: ComponentsByType{
				devicetypes.ComponentTypeCompute: {uuid.Nil},
			},
			wantErr: "Compute contains empty component UUID",
		},
		"component under multiple types": {
			components: ComponentsByType{
				devicetypes.ComponentTypeCompute:  {id1},
				devicetypes.ComponentTypeNVSwitch: {id1},
			},
			wantErr: "appears under both Compute and NVSwitch",
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			err := tc.components.Validate()
			require.ErrorContains(t, err, tc.wantErr)

			_, err = tc.components.Normalize()
			require.ErrorContains(t, err, tc.wantErr)
		})
	}
}

func TestComponentsByTypeMerge(t *testing.T) {
	id1 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000001")
	id2 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000002")
	id3 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000003")
	id4 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000004")
	id5 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000005")

	base := ComponentsByType{
		devicetypes.ComponentTypeCompute:    {id2, id1},
		devicetypes.ComponentTypePowerShelf: {id4},
	}
	next := ComponentsByType{
		devicetypes.ComponentTypeCompute:  {id3, id2},
		devicetypes.ComponentTypeNVSwitch: {id5},
	}

	merged, err := base.Merge(next)
	require.NoError(t, err)

	require.Equal(t, ComponentsByType{
		devicetypes.ComponentTypeCompute:    {id1, id2, id3},
		devicetypes.ComponentTypeNVSwitch:   {id5},
		devicetypes.ComponentTypePowerShelf: {id4},
	}, merged)
	require.Equal(t, []uuid.UUID{id2, id1}, base[devicetypes.ComponentTypeCompute])
	require.Equal(t, []uuid.UUID{id3, id2}, next[devicetypes.ComponentTypeCompute])
}

func TestComponentsByTypeMergeRejectsCrossTypeConflict(t *testing.T) {
	id1 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000001")

	_, err := ComponentsByType{
		devicetypes.ComponentTypeCompute: {id1},
	}.Merge(ComponentsByType{
		devicetypes.ComponentTypeNVSwitch: {id1},
	})

	require.ErrorContains(t, err, "appears under both Compute and NVSwitch")
}

func TestComponentsByTypeSubtract(t *testing.T) {
	id1 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000001")
	id2 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000002")
	id3 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000003")
	id4 := mustComponentUUID(t, "00000000-0000-0000-0000-000000000004")

	base := ComponentsByType{
		devicetypes.ComponentTypeCompute:    {id2, id1},
		devicetypes.ComponentTypeNVSwitch:   {id3},
		devicetypes.ComponentTypePowerShelf: {id4},
	}

	remaining := base.Subtract(ComponentsByType{
		devicetypes.ComponentTypeCompute:    {id2},
		devicetypes.ComponentTypePowerShelf: {id4},
	})

	require.Equal(t, ComponentsByType{
		devicetypes.ComponentTypeCompute:  {id1},
		devicetypes.ComponentTypeNVSwitch: {id3},
	}, remaining)
	require.Equal(t, []uuid.UUID{id2, id1}, base[devicetypes.ComponentTypeCompute])
}

func mustMarshalComponentFilter(
	t *testing.T,
	filter *ComponentFilter,
) []byte {
	t.Helper()

	raw, err := MarshalComponentFilter(filter)
	require.NoError(t, err)
	return raw
}

func mustComponentUUID(t *testing.T, value string) uuid.UUID {
	t.Helper()

	id, err := uuid.Parse(value)
	require.NoError(t, err)
	return id
}
