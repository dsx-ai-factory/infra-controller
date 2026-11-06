// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certmanager

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"io"
	"math/big"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// received is what a REST Cert Manager stub received.
type received struct {
	method, path, body string
}

// newStub serves status and body for every request. It reports each request
// it receives on the returned channel.
func newStub(t *testing.T, status int, body string) (*httptest.Server, <-chan received) {
	requests := make(chan received, 1)
	srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requestBody, err := io.ReadAll(r.Body)
		assert.NoError(t, err)
		requests <- received{method: r.Method, path: r.URL.Path, body: string(requestBody)}
		w.WriteHeader(status)
		_, _ = w.Write([]byte(body))
	}))
	t.Cleanup(srv.Close)
	return srv, requests
}

// writePEM writes certificate DER as a PEM file in a temporary directory.
func writePEM(t *testing.T, der []byte) string {
	path := filepath.Join(t.TempDir(), "ca.pem")
	require.NoError(t, os.WriteFile(path, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}), 0600))
	return path
}

// newTestClient returns a client for srv that trusts srv's certificate.
func newTestClient(t *testing.T, srv *httptest.Server) *Client {
	client, err := NewClient(srv.URL, writePEM(t, srv.Certificate().Raw))
	require.NoError(t, err)
	return client
}

// TestNewClient checks that the client trusts only the configured CA. A
// bundle without certificates fails at startup instead of falling back to an
// unverified connection.
func TestNewClient(t *testing.T) {
	srv, _ := newStub(t, http.StatusOK, "")

	// Every httptest server shares one certificate, so the unrelated CA is
	// generated here.
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	require.NoError(t, err)
	template := &x509.Certificate{
		SerialNumber:          big.NewInt(1),
		Subject:               pkix.Name{CommonName: "unrelated CA"},
		NotBefore:             time.Now().Add(-time.Hour),
		NotAfter:              time.Now().Add(time.Hour),
		IsCA:                  true,
		BasicConstraintsValid: true,
		KeyUsage:              x509.KeyUsageCertSign,
	}
	unrelatedDER, err := x509.CreateCertificate(rand.Reader, template, template, &key.PublicKey, key)
	require.NoError(t, err)

	emptyPath := filepath.Join(t.TempDir(), "empty.pem")
	require.NoError(t, os.WriteFile(emptyPath, []byte("not a certificate"), 0600))

	tests := []struct {
		name           string
		caPath         string
		wantBuildErr   bool
		wantRequestErr bool
	}{
		{
			name:   "the CA that signed the endpoint is accepted",
			caPath: writePEM(t, srv.Certificate().Raw),
		},
		{
			name:           "an unrelated CA is refused",
			caPath:         writePEM(t, unrelatedDER),
			wantRequestErr: true,
		},
		{
			name:         "a bundle without certificates fails at construction",
			caPath:       emptyPath,
			wantBuildErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			client, err := NewClient(srv.URL, tt.caPath)
			if tt.wantBuildErr {
				assert.Error(t, err)
				assert.Nil(t, client)
				return
			}
			require.NoError(t, err)

			_, err = client.GetCA(context.Background())
			if tt.wantRequestErr {
				assert.ErrorContains(t, err, "certificate")
				return
			}
			assert.NoError(t, err)
		})
	}
}

func TestClient_IssueCertificate(t *testing.T) {
	tests := []struct {
		name    string
		status  int
		body    string
		want    *Certificate
		wantErr string
	}{
		{
			name:   "posts name, app and ttl and decodes the issued pair",
			status: http.StatusOK,
			body:   `{"key":"KEY PEM","certificate":"CERT PEM"}`,
			want:   &Certificate{Key: "KEY PEM", Certificate: "CERT PEM"},
		},
		{
			name:    "a status outside 2xx is an error with the status",
			status:  http.StatusInternalServerError,
			body:    "{}",
			wantErr: "500 Internal Server Error",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			srv, requests := newStub(t, tt.status, tt.body)

			got, err := newTestClient(t, srv).IssueCertificate(context.Background(), "client", "site-uuid", 2160)
			if tt.wantErr != "" {
				assert.ErrorContains(t, err, tt.wantErr)
				assert.Nil(t, got)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.want, got)
			assert.Equal(t, received{
				method: http.MethodPost,
				path:   "/v1/pki/cloud-cert",
				body:   `{"name":"client","app":"site-uuid","ttl":2160}` + "\n",
			}, <-requests)
		})
	}
}

func TestClient_GetCA(t *testing.T) {
	const caPEM = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n"
	srv, requests := newStub(t, http.StatusOK, caPEM)

	got, err := newTestClient(t, srv).GetCA(context.Background())
	require.NoError(t, err)
	assert.Equal(t, caPEM, got)
	assert.Equal(t, received{method: http.MethodGet, path: "/v1/pki/ca/pem"}, <-requests)
}
