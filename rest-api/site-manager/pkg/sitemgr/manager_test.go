// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sitemgr

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/pem"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestManager(t *testing.T) {
	s, err := TestManagerCreateSite()
	assert.Nil(t, err)
	assert.NotNil(t, s)
	err = s.TestManagerSiteTest()
	assert.NotNil(t, err)
	s.Teardown()
}

func TestCLI(t *testing.T) {
	cmd := NewCommand()
	assert.NotEqual(t, nil, cmd)
}

func TestNewCertManagerClient(t *testing.T) {
	for _, tc := range []struct {
		name             string
		scheme           string
		wrongHostname    bool
		caContent        string
		unsetCA          bool
		emptyCA          bool
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
		{name: "old chart without client identity", scheme: "https", noIdentity: true, wantStatus: http.StatusOK},
		{name: "partial client identity", scheme: "https", partialIdentity: true, wantInitError: true},
		{name: "wrong TLS identity", scheme: "https", wrongHostname: true, wantRequestError: true},
		{name: "plain HTTP refused", scheme: "http", wantInitError: true},
		{name: "missing client certificate", scheme: "https", missingIdentity: true, wantInitError: true},
		{name: "missing CA file", scheme: "https", missingCA: true, wantInitError: true},
		{name: "unrelated CA refused", scheme: "https", caContent: testCACert, wantRequestError: true},
		{name: "invalid CA bundle", scheme: "https", caContent: "not a certificate", wantInitError: true},
		{name: "empty CA bundle does not fall back to system roots", scheme: "https", emptyCA: true, wantInitError: true},
		{name: "unset CA path", scheme: "https", unsetCA: true, wantInitError: true},
		{name: "redirect does not forward identity", scheme: "https", redirect: true, wantStatus: http.StatusTemporaryRedirect},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var calls atomic.Int32
			target := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				calls.Add(1)
				if tc.noIdentity {
					assert.Empty(t, r.TLS.PeerCertificates)
				} else {
					require.Len(t, r.TLS.PeerCertificates, 1)
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
			if tc.caContent != "" || tc.emptyCA {
				require.NoError(t, os.WriteFile(caPath, []byte(tc.caContent), 0600))
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
			client, err := newCertManagerClient(ctx, Options{credsMgrURL: endpoint, credsMgrClientCertPath: certPath, credsMgrClientKeyPath: keyPath, credsMgrCAPath: caPath})
			if tc.wantInitError {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			t.Cleanup(client.CloseIdleConnections)
			response, err := client.Get(endpoint)
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
