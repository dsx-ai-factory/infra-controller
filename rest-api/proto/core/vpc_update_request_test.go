// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package core_test

import (
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"
)

func TestVpcUpdateRequestPowerResourceGroupJSON(t *testing.T) {
	tests := []struct {
		name    string
		payload string
		want    *string
	}{
		{
			name:    "set camel case",
			payload: `{"powerResourceGroup":"power-group"}`,
			want:    stringPtr("power-group"),
		},
		{
			name:    "set snake case",
			payload: `{"power_resource_group":"power-group"}`,
			want:    stringPtr("power-group"),
		},
		{
			name:    "clear",
			payload: `{"powerResourceGroup":""}`,
			want:    stringPtr(""),
		},
		{
			name:    "omitted",
			payload: `{}`,
		},
		{
			name:    "null",
			payload: `{"powerResourceGroup":null}`,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			request := &corev1.VpcUpdateRequest{}
			require.NoError(t, protojson.Unmarshal([]byte(test.payload), request))
			require.Equal(t, test.want, request.PowerResourceGroup)
		})
	}
}

func stringPtr(value string) *string {
	return &value
}
