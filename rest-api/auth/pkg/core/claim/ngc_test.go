// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package claim

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestNgcClaims_ValidateOrg(t *testing.T) {
	type fields struct {
		Access []NgcAccessClaim
	}
	type args struct {
		orgName string
	}

	orgName := "test-org"

	ngcOrgClaim := NgcAccessClaim{
		Type:    "group/ngc-stg",
		Name:    orgName,
		Actions: []string{},
	}

	tests := []struct {
		name   string
		fields fields
		args   args
		want   bool
	}{
		{
			name: "validate and accept org in claim",
			fields: fields{
				Access: []NgcAccessClaim{
					ngcOrgClaim,
				},
			},
			args: args{
				orgName: orgName,
			},
			want: true,
		},
		{
			name: "validate and reject org in claim",
			fields: fields{
				Access: []NgcAccessClaim{
					ngcOrgClaim,
				},
			},
			args: args{
				orgName: "invalid-org",
			},
			want: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nc := &NgcKasClaims{
				Access: tt.fields.Access,
			}

			assert.Equal(t, tt.want, nc.ValidateOrg(tt.args.orgName))
		})
	}
}
