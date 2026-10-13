// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package matclient

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"encoding/pem"
	"net/http"
	"strings"
	"net/http/httptest"
	"os"
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

func TestNewClient_WithRootCA(t *testing.T) {
	// Start a TLS test server with a self-signed certificate.
	server := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()

	// Extract the server's self-signed CA cert.
	certPEM := server.TLS.Certificates[0].Certificate[0]
	cert, err := x509.ParseCertificate(certPEM)
	require.NoError(t, err)

	// Write the CA cert to a temp file using encoding/pem.
	caFile, err := os.CreateTemp(t.TempDir(), "ca-*.crt")
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(caFile.Name(),
		pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: certPEM}), 0o600))

	t.Run("rejects server cert without CA", func(t *testing.T) {
		// Default client uses the system pool, which won't trust the test cert.
		client, err := NewClient(server.URL)
		require.NoError(t, err)
		defer client.Close()
		_, err = client.GetMachinesStatus(context.Background())
		require.Error(t, err, "should reject self-signed cert without custom CA")
	})

	t.Run("accepts server cert with matching CA", func(t *testing.T) {
		client, err := NewClient(server.URL, WithRootCA(caFile.Name()))
		require.NoError(t, err)
		defer client.Close()

		// Verify the CA pool is set on the transport.
		transport, ok := client.httpClient.Transport.(*http.Transport)
		require.True(t, ok)
		require.NotNil(t, transport.TLSClientConfig)
		require.NotNil(t, transport.TLSClientConfig.RootCAs)
		assert.True(t, transport.TLSClientConfig.RootCAs.Equal(func() *x509.CertPool {
			p := x509.NewCertPool()
			p.AddCert(cert)
			return p
		}()))
	})

	t.Run("rejects missing CA file", func(t *testing.T) {
		_, err := NewClient(server.URL, WithRootCA("/nonexistent/ca.crt"))
		require.Error(t, err)
		assert.Contains(t, err.Error(), "loading root CA")
	})

	t.Run("rejects non-PEM CA file", func(t *testing.T) {
		bad, err := os.CreateTemp(t.TempDir(), "bad-*.crt")
		require.NoError(t, err)
		require.NoError(t, os.WriteFile(bad.Name(), []byte("not a cert"), 0o600))
		_, err = NewClient(server.URL, WithRootCA(bad.Name()))
		require.Error(t, err)
		assert.Contains(t, err.Error(), "no valid PEM certificates")
	})

	_ = cert // used above via AddCert
}

// Ensure tls is used (for the TLS server setup in TestNewClient_WithRootCA).
var _ = tls.Certificate{}

func TestClient_Close(t *testing.T) {
	client, err := NewClient("https://example.com")
	require.NoError(t, err)

	// Close should not error and should be idempotent
	require.NoError(t, client.Close())
	require.NoError(t, client.Close())
}
