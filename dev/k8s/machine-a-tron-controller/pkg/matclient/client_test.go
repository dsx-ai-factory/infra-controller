// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package matclient

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestClient_GetMachinesStatus(t *testing.T) {
	tests := []struct {
		name           string
		serverResponse *MachinesStatusResponse
		serverStatus   int
		wantErr        bool
		errContains    string
	}{
		{
			name: "empty machines list",
			serverResponse: &MachinesStatusResponse{
				Machines: []MachineStatus{},
			},
			serverStatus: http.StatusOK,
		},
		{
			name: "single host with DPU",
			serverResponse: &MachinesStatusResponse{
				Machines: []MachineStatus{
					{
						MatID:      "host-uuid-1",
						MachineID:  ptr("machine-123"),
						APIState:   "Ready",
						PowerState: "On",
						BMC: BMCStatus{
							IP: ptr("192.168.1.100"),
							Redfish: EndpointStatus{
								ReachablePort: 443,
								ListenPort:    8443,
							},
						},
						DPUs: []MachineStatus{
							{
								MatID:      "dpu-uuid-1",
								MachineID:  ptr("dpu-456"),
								APIState:   "Ready",
								PowerState: "On",
								BMC: BMCStatus{
									IP: ptr("192.168.1.101"),
									Redfish: EndpointStatus{
										ReachablePort: 443,
										ListenPort:    8444,
									},
									SSH: &EndpointStatus{
										ReachablePort: 32022,
										ListenPort:    32022,
									},
								},
							},
						},
					},
				},
			},
			serverStatus: http.StatusOK,
		},
		{
			name:         "server error",
			serverStatus: http.StatusInternalServerError,
			wantErr:      true,
			errContains:  "unexpected status 500",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				assert.Equal(t, "/machines/status", r.URL.Path)
				assert.Equal(t, http.MethodGet, r.Method)
				assert.Equal(t, "application/json", r.Header.Get("Accept"))

				w.WriteHeader(tt.serverStatus)
				if tt.serverResponse != nil {
					err := json.NewEncoder(w).Encode(tt.serverResponse)
					require.NoError(t, err)
				}
			}))
			defer server.Close()

			client, err := NewClient(server.URL, WithLogger(zerolog.Nop()))
			require.NoError(t, err)

			resp, err := client.GetMachinesStatus(context.Background())

			if tt.wantErr {
				require.Error(t, err)
				if tt.errContains != "" {
					assert.Contains(t, err.Error(), tt.errContains)
				}
				return
			}

			require.NoError(t, err)
			assert.Equal(t, tt.serverResponse, resp)
		})
	}
}

func TestNewClient_InvalidURL(t *testing.T) {
	tests := []struct {
		name        string
		url         string
		errContains string
	}{
		{
			name:        "invalid syntax",
			url:         "://invalid",
			errContains: "invalid base URL",
		},
		{
			name:        "userinfo rejected",
			url:         "https://user:password@example.com",
			errContains: "userinfo not allowed",
		},
		{
			name:        "query rejected",
			url:         "https://example.com?token=secret",
			errContains: "query string not allowed",
		},
		{
			name:        "fragment rejected",
			url:         "https://example.com#fragment",
			errContains: "fragment not allowed",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := NewClient(tt.url)
			require.Error(t, err)
			assert.Contains(t, err.Error(), tt.errContains)
		})
	}
}

func TestClient_GetMachinesStatus_InvalidBodies(t *testing.T) {
	tests := []struct {
		name        string
		body        string
		errContains string
	}{
		{
			name:        "oversized response",
			body:        strings.Repeat(" ", maxResponseSize+1),
			errContains: "response exceeds maximum size",
		},
		{
			name:        "trailing JSON rejected",
			body:        `{"machines":[]} {"machines":[]}`,
			errContains: "decoding response",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.WriteHeader(http.StatusOK)
				_, err := w.Write([]byte(tt.body))
				require.NoError(t, err)
			}))
			defer server.Close()

			client, err := NewClient(server.URL)
			require.NoError(t, err)

			_, err = client.GetMachinesStatus(context.Background())
			require.Error(t, err)
			assert.Contains(t, err.Error(), tt.errContains)
		})
	}
}

func ptr[T any](v T) *T {
	return &v
}

func TestClient_Close(t *testing.T) {
	client, err := NewClient("https://example.com")
	require.NoError(t, err)

	// Close should not error and should be idempotent
	require.NoError(t, client.Close())
	require.NoError(t, client.Close())
}
