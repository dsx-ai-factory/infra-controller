// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pmcregistry

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestStringToDSType(t *testing.T) {
	testCases := map[string]struct {
		in       string
		wantType PmcRegisterType
		wantOK   bool
	}{
		"valid Postgres": {
			in:       "Postgres",
			wantType: RegisterTypePostgres,
			wantOK:   true,
		},
		"valid InMemory": {
			in:       "InMemory",
			wantType: RegisterTypeInMemory,
			wantOK:   true,
		},
		"unknown type": {
			in:       "Unknown",
			wantType: "",
			wantOK:   false,
		},
		"empty string": {
			in:       "",
			wantType: "",
			wantOK:   false,
		},
		"wrong case (postgres)": {
			in:       "postgres",
			wantType: "",
			wantOK:   false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			gotType, ok := StringToDSType(tc.in)
			assert.Equal(t, tc.wantType, gotType)
			assert.Equal(t, tc.wantOK, ok)
		})
	}
}

func TestDSTypeStringRoundTrip(t *testing.T) {
	testCases := map[string]struct {
		inType PmcRegisterType
	}{
		"round-trip Postgres": {inType: RegisterTypePostgres},
		"round-trip InMemory": {inType: RegisterTypeInMemory},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			s := string(tc.inType)
			gotType, ok := StringToDSType(s)
			assert.True(t, ok)
			assert.Equal(t, tc.inType, gotType)
		})
	}
}
