// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// TestActivities_All_ContainsAllActivities verifies that All() returns every
// expected activity name with a non-nil function value.
func TestActivities_All_ContainsAllActivities(t *testing.T) {
	acts := New(nil, nil, nil, nil)
	all := acts.All()

	expectedNames := []string{
		NameInjectExpectation,
		NamePowerControl,
		NameGetPowerStatus,
		NameUpdateTaskStatus,
		NameUpdateTaskReport,
		NameFirmwareControl,
		NameGetFirmwareStatus,
		NameBringUpControl,
		NameGetBringUpStatus,
		NameVerifyFirmwareConsistency,
		NameDecommissionControl,
		NameGetDecommissionStatus,
	}
	require.Len(t, all, len(expectedNames), "unexpected number of activities")

	for _, name := range expectedNames {
		assert.Contains(t, all, name, "expected activity %q to be present", name)
		assert.NotNil(t, all[name], "expected function for activity %q to be non-nil", name)
	}
}

// TestActivities_All_ReturnsCopy verifies that mutating the returned map does
// not affect subsequent calls — each call produces an independent map.
func TestActivities_All_ReturnsCopy(t *testing.T) {
	acts := New(nil, nil, nil, nil)
	first := acts.All()
	firstLen := len(first)

	first["should-not-persist"] = func() {}

	second := acts.All()
	assert.Equal(t, firstLen, len(second), "registry size should be unchanged after mutating the returned map")
	assert.NotContains(t, second, "should-not-persist")
}

// TestActivities_Isolation verifies that two Activities instances do not share
// state: mutations to one instance's map must not affect the other.
func TestActivities_Isolation(t *testing.T) {
	a1 := New(nil, nil, nil, nil)
	a2 := New(nil, nil, nil, nil)

	m1 := a1.All()
	m1["isolation-sentinel"] = func() {}

	m2 := a2.All()
	assert.NotContains(t, m2, "isolation-sentinel", "mutations to one instance's map must not bleed into another instance's map")
}
