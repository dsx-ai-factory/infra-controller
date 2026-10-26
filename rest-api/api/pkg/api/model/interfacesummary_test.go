// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestAPIInterfaceSummary_FromDBModel(t *testing.T) {
	vpcID := uuid.New()
	vpcPrefixID := uuid.New()

	tests := []struct {
		name            string
		ifc             cdbm.Interface
		wantVpcID       string
		wantVpcPrefixID *string
	}{
		{
			name:      "direct VPC selection with a reconciled VPC Prefix",
			ifc:       cdbm.Interface{VpcID: &vpcID, VpcPrefixID: &vpcPrefixID},
			wantVpcID: vpcID.String(),
		},
		{
			name: "explicit VPC Prefix selection derives its parent VPC",
			ifc: cdbm.Interface{
				VpcPrefixID: &vpcPrefixID,
				VpcPrefix:   &cdbm.VpcPrefix{ID: vpcPrefixID, VpcID: vpcID},
			},
			wantVpcID:       vpcID.String(),
			wantVpcPrefixID: cutil.GetPtr(vpcPrefixID.String()),
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			var summary APIInterfaceSummary
			summary.FromDBModel(&tc.ifc)
			assert.Equal(t, tc.wantVpcID, summary.VpcID)
			assert.Equal(t, tc.wantVpcPrefixID, summary.VpcPrefixID)
		})
	}
}

func TestAPIInterfaceSummaryGetAllRequest_Validate(t *testing.T) {
	validID := uuid.NewString()
	invalidID := "not-a-uuid"
	tests := []struct {
		name    string
		request APIInterfaceSummaryGetAllRequest
		wantErr bool
	}{
		{name: "no filters"},
		{name: "valid IDs", request: APIInterfaceSummaryGetAllRequest{VpcID: &validID, InstanceTenantID: &validID}},
		{name: "invalid VPC ID", request: APIInterfaceSummaryGetAllRequest{VpcID: &invalidID}, wantErr: true},
		{name: "invalid Instance Tenant ID", request: APIInterfaceSummaryGetAllRequest{InstanceTenantID: &invalidID}, wantErr: true},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.request.Validate()
			assert.Equal(t, tc.wantErr, err != nil)
		})
	}
}
