// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package testutil

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestTestDatabaseName(t *testing.T) {
	for _, tc := range []struct {
		name     string
		dbName   string
		testName string
		want     string
	}{
		{
			name:     "short name",
			dbName:   "nicotest",
			testName: "TestName/subtest",
			want:     "nicotest_test_testname_subtest",
		},
		{
			name:     "identifier limit",
			dbName:   "nicotest",
			testName: strings.Repeat("t", 49),
			want:     "nicotest_test_" + strings.Repeat("t", 49),
		},
		{
			name:     "just over identifier limit",
			dbName:   "nicotest",
			testName: strings.Repeat("t", 50),
			want:     "nicotest_test_tttttttttttttttttttttttttttttttt_33de77d0da35848e",
		},
		{
			name:     "set expected description",
			dbName:   "nicotest",
			testName: "TestUpdateMirroredComponent_PreservesRuntimeWriteAfterReconciliationRead/set_expected_description",
			want:     "nicotest_test_testupdatemirroredcomponent_pres_0ce43f6a412a0337",
		},
		{
			name:     "clear expected description",
			dbName:   "nicotest",
			testName: "TestUpdateMirroredComponent_PreservesRuntimeWriteAfterReconciliationRead/clear_expected_description",
			want:     "nicotest_test_testupdatemirroredcomponent_pres_551bbf5e397347ee",
		},
		{
			name:     "long database prefix",
			dbName:   "a" + strings.Repeat("é", 30),
			testName: "T",
			want:     "aéééééééééééééééééééééé_55fca24612198170",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got := testDatabaseName(tc.dbName, tc.testName)
			assert.Equal(t, tc.want, got)
		})
	}
}
