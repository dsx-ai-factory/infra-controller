// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"google.golang.org/protobuf/proto"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestAPIPowerShelfDecommissionRequest_Validate(t *testing.T) {
	cases := []struct {
		name, siteID, componentID string
		valid                     bool
	}{
		{"valid", "11111111-1111-4111-8111-111111111111", "component-id", true},
		{"missing site", "", "component-id", false},
		{"invalid site", "invalid", "component-id", false},
		{"missing component", "11111111-1111-4111-8111-111111111111", "", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			request := APIPowerShelfDecommissionRequest{SiteID: tc.siteID, PowerShelfID: tc.componentID}
			if tc.valid {
				assert.NoError(t, request.Validate())
			} else {
				assert.Error(t, request.Validate())
			}
		})
	}
}

func TestAPIPowerShelfDecommissionRequest_ToProto(t *testing.T) {
	request := APIPowerShelfDecommissionRequest{SiteID: "11111111-1111-4111-8111-111111111111", PowerShelfID: "component-id"}
	expected := &corev1.DecommissionPowerShelfRequest{PowerShelfId: &corev1.PowerShelfId{Id: request.PowerShelfID}}
	assert.True(t, proto.Equal(expected, request.ToProto()))
}
