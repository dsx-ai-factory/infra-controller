// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestConfigurationServerURL(t *testing.T) {
	cfg := NewConfiguration()

	tests := []struct {
		name      string
		index     int
		variables map[string]string
		expected  string
	}{
		{
			name:     "default in-cluster HTTP Service",
			index:    0,
			expected: "http://nico-rest-api.nico-rest.svc.cluster.local:8388",
		},
		{
			name:      "in-cluster Service namespace override",
			index:     0,
			variables: map[string]string{"namespace": "custom-namespace"},
			expected:  "http://nico-rest-api.custom-namespace.svc.cluster.local:8388",
		},
		{
			name:     "default external TLS endpoint",
			index:    1,
			expected: "https://rest-api.example.com",
		},
		{
			name:      "external TLS hostname override",
			index:     1,
			variables: map[string]string{"hostname": "rest-api.mysite.example.com"},
			expected:  "https://rest-api.mysite.example.com",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			serverURL, err := cfg.ServerURL(tt.index, tt.variables)
			require.NoError(t, err)
			assert.Equal(t, tt.expected, serverURL)
		})
	}
}
