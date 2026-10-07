// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package core_test

import (
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"
)

func TestSitePrefixAuthority_ProtoJSONCompatibility(t *testing.T) {
	operatorManaged := corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_OPERATOR_MANAGED
	configured := corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_CONFIGURED //nolint:staticcheck // Verify the deprecated protobuf alias remains usable.
	require.Equal(t, int32(1), int32(operatorManaged))
	require.Equal(t, operatorManaged, configured)

	tests := []struct {
		name     string
		wireJSON string
	}{
		{
			name:     "canonical operator-managed spelling",
			wireJSON: `{"authority":"SITE_PREFIX_AUTHORITY_OPERATOR_MANAGED"}`,
		},
		{
			name:     "deprecated configured spelling",
			wireJSON: `{"authority":"SITE_PREFIX_AUTHORITY_CONFIGURED"}`,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var status corev1.SitePrefixStatus
			require.NoError(t, protojson.Unmarshal([]byte(test.wireJSON), &status))
			require.Equal(t, operatorManaged, status.GetAuthority())
			require.Equal(t, int32(1), int32(status.GetAuthority()))
		})
	}

	encoded, err := protojson.Marshal(&corev1.SitePrefixStatus{Authority: operatorManaged})
	require.NoError(t, err)
	require.JSONEq(
		t,
		`{"authority":"SITE_PREFIX_AUTHORITY_OPERATOR_MANAGED"}`,
		string(encoded),
	)
}
