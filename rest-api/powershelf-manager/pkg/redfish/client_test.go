// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package redfish

import (
	"net"
	"net/http"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestBuildEndpoint(t *testing.T) {
	testCases := map[string]struct {
		ip   string
		port string
	}{
		"IPv4":           {ip: "192.0.2.1", port: "443"},
		"IPv6":           {ip: "2001:db8::abcd", port: "443"},
		"local test PMC": {ip: "127.0.0.1", port: "8443"},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			endpoint := buildEndpoint(net.ParseIP(testCase.ip))
			request, err := http.NewRequestWithContext(t.Context(), http.MethodGet, endpoint, nil)
			require.NoError(t, err)
			assert.Equal(t, "https", request.URL.Scheme)
			assert.Equal(t, testCase.ip, request.URL.Hostname())
			port := request.URL.Port()
			if port == "" {
				port = "443"
			}
			assert.Equal(t, testCase.port, port)
		})
	}
}
