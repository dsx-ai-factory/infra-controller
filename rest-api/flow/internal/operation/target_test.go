// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operation

import (
	"testing"

	"github.com/stretchr/testify/require"

	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
)

func TestTargetSpecValidateTargetKinds(t *testing.T) {
	testCases := map[string]struct {
		targetSpec *TargetSpec
		wantErr    string
	}{
		"nil": {
			wantErr: "target spec is nil",
		},
		"empty": {
			targetSpec: &TargetSpec{},
			wantErr:    "must have exactly one",
		},
		"rack": {
			targetSpec: &TargetSpec{
				Racks: []RackTarget{{Identifier: identifier.Identifier{Name: "rack-1"}}},
			},
		},
		"NVLink domain": {
			targetSpec: &TargetSpec{
				NVLDomains: []NVLDomainTarget{
					{Identifier: identifier.Identifier{Name: "domain-1"}},
				},
			},
		},
		"multiple target kinds": {
			targetSpec: &TargetSpec{
				Racks: []RackTarget{{Identifier: identifier.Identifier{Name: "rack-1"}}},
				NVLDomains: []NVLDomainTarget{
					{Identifier: identifier.Identifier{Name: "domain-1"}},
				},
			},
			wantErr: "must have exactly one",
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			err := testCase.targetSpec.Validate()
			if testCase.wantErr != "" {
				require.ErrorContains(t, err, testCase.wantErr)
				return
			}
			require.NoError(t, err)
		})
	}
}
