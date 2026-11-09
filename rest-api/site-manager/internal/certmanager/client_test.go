// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certmanager

import (
	"bytes"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/json"
	"encoding/pem"
	"fmt"
	"io"
	"math/big"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// received is what a Cert Manager stub received.
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
	client, err := NewClient(t.Context(), Options{URL: srv.URL, CAPath: writePEM(t, srv.Certificate().Raw)})
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
			client, err := NewClient(t.Context(), Options{URL: srv.URL, CAPath: tt.caPath})
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
	t.Run("TLS startup diagnostics use service JSON logging", func(t *testing.T) {
		var logs bytes.Buffer
		original := log.Logger
		log.Logger = zerolog.New(&logs).With().Str("component", "site-manager").Logger()
		t.Cleanup(func() { log.Logger = original })
		caPath := writePEM(t, srv.Certificate().Raw)
		ca, err := os.ReadFile(caPath)
		require.NoError(t, err)
		ca = append(ca, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: []byte("invalid DER")})...)
		require.NoError(t, os.WriteFile(caPath, ca, 0600))
		certPath := writePEM(t, srv.Certificate().Raw)
		// Identity loading fails synchronously after the CA diagnostic, so no poller starts.
		_, err = NewClient(t.Context(), Options{URL: srv.URL, CAPath: caPath, ClientCertPath: certPath, ClientKeyPath: filepath.Join(t.TempDir(), "missing.key")})
		require.Error(t, err)
		var record map[string]any
		require.NoError(t, json.Unmarshal(logs.Bytes(), &record))
		assert.Equal(t, "error", record[zerolog.LevelFieldName])
		assert.Contains(t, record[zerolog.MessageFieldName], "using parseable certificates")
		assert.Equal(t, "site-manager", record["component"])
		assert.Equal(t, caPath, record["path"])
	})
	for _, tc := range []struct {
		name             string
		scheme           string
		wrongHostname    bool
		inheritedProxy   bool
		unsetCA          bool
		missingIdentity  bool
		noIdentity       bool
		partialIdentity  bool
		missingCA        bool
		redirect         bool
		wantInitError    bool
		wantRequestError bool
		wantStatus       int
	}{
		{name: "verified TLS sends client certificate", scheme: "https", wantStatus: http.StatusOK},
		{name: "ignores inherited proxy for in-cluster issuance", scheme: "https", inheritedProxy: true, wantStatus: http.StatusOK},
		{name: "old chart without client identity", scheme: "https", noIdentity: true, wantStatus: http.StatusOK},
		{name: "partial client identity", scheme: "https", partialIdentity: true, wantInitError: true},
		{name: "wrong TLS identity", scheme: "https", wrongHostname: true, wantRequestError: true},
		{name: "plain HTTP refused", scheme: "http", wantInitError: true},
		{name: "missing client certificate", scheme: "https", missingIdentity: true, wantInitError: true},
		{name: "missing CA file", scheme: "https", missingCA: true, wantInitError: true},
		{name: "unset CA path", scheme: "https", unsetCA: true, wantInitError: true},
		{name: "redirect does not forward identity", scheme: "https", redirect: true, wantStatus: http.StatusTemporaryRedirect},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if tc.inheritedProxy {
				original := http.DefaultTransport
				transport := original.(*http.Transport).Clone()
				transport.Proxy = func(_ *http.Request) (*url.URL, error) {
					return nil, fmt.Errorf("in-cluster issuance must bypass proxy")
				}
				http.DefaultTransport = transport
				t.Cleanup(func() { http.DefaultTransport = original })
			}
			var calls atomic.Int32
			target := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				calls.Add(1)
				if tc.noIdentity {
					assert.Empty(t, r.TLS.PeerCertificates)
				} else {
					assert.Len(t, r.TLS.PeerCertificates, 1)
				}
				assert.Empty(t, r.Header.Get("Authorization"))
				if tc.redirect {
					http.Redirect(w, r, "/redirected", http.StatusTemporaryRedirect)
					return
				}
				w.WriteHeader(http.StatusOK)
			}))
			target.TLS = &tls.Config{MinVersion: tls.VersionTLS12, ClientAuth: tls.RequestClientCert}
			target.StartTLS()
			t.Cleanup(target.Close)
			dir := t.TempDir()
			caPath := filepath.Join(dir, "ca.crt")
			certPath := filepath.Join(dir, "client.crt")
			keyPath := filepath.Join(dir, "client.key")
			require.NoError(t, os.WriteFile(caPath, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: target.Certificate().Raw}), 0600))
			require.NoError(t, os.WriteFile(certPath, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: target.Certificate().Raw}), 0600))
			keyDER, err := x509.MarshalPKCS8PrivateKey(target.TLS.Certificates[0].PrivateKey)
			require.NoError(t, err)
			require.NoError(t, os.WriteFile(keyPath, pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: keyDER}), 0600))
			if tc.noIdentity {
				certPath, keyPath = "", ""
			}
			if tc.partialIdentity {
				keyPath = ""
			}
			if tc.missingIdentity {
				certPath += ".absent"
			}
			if tc.missingCA {
				caPath += ".absent"
			}
			if tc.unsetCA {
				caPath = ""
			}
			endpoint := tc.scheme + target.URL[len("https"):]
			if tc.wrongHostname {
				endpoint = strings.Replace(endpoint, "127.0.0.1", "localhost", 1)
			}
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			client, err := NewClient(ctx, Options{URL: endpoint, ClientCertPath: certPath, ClientKeyPath: keyPath, CAPath: caPath})
			if tc.wantInitError {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			t.Cleanup(client.httpClient.CloseIdleConnections)
			response, err := client.httpClient.Get(endpoint)
			if tc.wantRequestError {
				require.Error(t, err)
				assert.Zero(t, calls.Load())
				return
			}
			require.NoError(t, err)
			defer response.Body.Close()
			_, err = io.Copy(io.Discard, response.Body)
			require.NoError(t, err)
			assert.Equal(t, tc.wantStatus, response.StatusCode)
			assert.EqualValues(t, 1, calls.Load())
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
