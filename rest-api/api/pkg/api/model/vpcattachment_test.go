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

func TestAPIVpcAttachment_FromDBModel_VpcPrefixID(t *testing.T) {
	vpcID := uuid.New()
	vpcPrefixID := uuid.New()

	tests := []struct {
		name string
		ifc  cdbm.Interface
		want *string
	}{
		{
			name: "direct VPC selection with a reconciled VPC Prefix",
			ifc:  cdbm.Interface{VpcID: &vpcID, VpcPrefixID: &vpcPrefixID},
		},
		{
			name: "explicit VPC Prefix selection",
			ifc:  cdbm.Interface{VpcPrefixID: &vpcPrefixID},
			want: cutil.GetPtr(vpcPrefixID.String()),
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			var attachment APIVpcAttachment
			attachment.FromDBModel(&tc.ifc, vpcID)
			assert.Equal(t, tc.want, attachment.VpcPrefixID)
		})
	}
}
