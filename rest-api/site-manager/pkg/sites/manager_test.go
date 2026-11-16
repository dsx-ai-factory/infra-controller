// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sites

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"encoding/pem"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/site-manager/internal/certmanager"
	fakecrdclient "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/client/clientset/versioned/fake"
)

// TestManager starts Site Manager over TLS with a serving certificate from a
// Cert Manager stub. Then it creates and reads a Site through the routes.
func TestManager(t *testing.T) {
	s, err := TestManagerCreateSite()
	require.NoError(t, err)
	t.Cleanup(s.Teardown)
	assert.NotEmpty(t, s.UUID1OTP)
}

// TestNewSiteManager protects cancellation and certificate-client wiring.
func TestNewSiteManager(t *testing.T) {
	tests := []struct {
		name  string
		check func(*testing.T)
	}{
		{name: "stops retrying when canceled", check: func(t *testing.T) {
			caPath := filepath.Join(t.TempDir(), "ca.pem")
			require.NoError(t, os.WriteFile(caPath, []byte(testCACert), 0600))

			ctx, cancel := context.WithCancel(context.Background())
			cancel()

			o := Options{
				credsMgrURL:    "https://127.0.0.1:1",
				credsMgrCAPath: caPath,
				listenPort:     "0",
				namespace:      "site-manager",
				otpDuration:    time.Hour,
			}
			done := make(chan error, 1)
			go func() {
				_, err := NewSiteManager(ctx, o, fakecrdclient.NewSimpleClientset())
				done <- err
			}()

			select {
			case err := <-done:
				assert.ErrorIs(t, err, context.Canceled)
			case <-time.After(5 * time.Second):
				t.Fatal("NewSiteManager kept retrying after its context was canceled")
			}

		}},
		{name: "presents configured client identity during bootstrap", check: func(t *testing.T) {
			registerer := prometheus.DefaultRegisterer
			prometheus.DefaultRegisterer = prometheus.NewRegistry()
			t.Cleanup(func() { prometheus.DefaultRegisterer = registerer })
			var peers atomic.Int32
			var srv *httptest.Server
			srv = httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
				peers.Store(int32(len(req.TLS.PeerCertificates)))
				pair := srv.TLS.Certificates[0]
				keyDER, err := x509.MarshalPKCS8PrivateKey(pair.PrivateKey)
				if err != nil {
					http.Error(w, err.Error(), http.StatusInternalServerError)
					return
				}
				response := certmanager.Certificate{
					Certificate: string(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: pair.Certificate[0]})),
					Key:         string(pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: keyDER})),
				}
				err = json.NewEncoder(w).Encode(response)
				assert.NoError(t, err)
			}))
			srv.TLS = &tls.Config{MinVersion: tls.VersionTLS12, ClientAuth: tls.RequireAnyClientCert}
			srv.StartTLS()
			defer srv.Close()
			dir := t.TempDir()
			caPath := filepath.Join(dir, "ca.crt")
			certPath := filepath.Join(dir, "client.crt")
			keyPath := filepath.Join(dir, "client.key")
			certPEM := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: srv.Certificate().Raw})
			require.NoError(t, os.WriteFile(caPath, certPEM, 0600))
			require.NoError(t, os.WriteFile(certPath, certPEM, 0600))
			keyDER, err := x509.MarshalPKCS8PrivateKey(srv.TLS.Certificates[0].PrivateKey)
			require.NoError(t, err)
			require.NoError(t, os.WriteFile(keyPath, pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: keyDER}), 0600))
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			manager, err := NewSiteManager(ctx, Options{
				credsMgrURL:            srv.URL,
				credsMgrCAPath:         caPath,
				credsMgrClientCertPath: certPath,
				credsMgrClientKeyPath:  keyPath,
				ingressHost:            "test-host",
				namespace:              "site-manager",
				otpDuration:            time.Hour,
			}, fakecrdclient.NewSimpleClientset())
			require.NoError(t, err)
			require.NotNil(t, manager)
			assert.EqualValues(t, 1, peers.Load())

		}},
	}
	for _, test := range tests {
		t.Run(test.name, test.check)
	}
}
