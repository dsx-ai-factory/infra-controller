// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sitemgr

import (
	"encoding/pem"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewCertificateClient(t *testing.T) {
	for _, tc := range []struct {
		name             string
		scheme           string
		serverName       string
		missingToken     bool
		missingCA        bool
		redirect         bool
		wantInitError    bool
		wantRequestError bool
		wantStatus       int
	}{
		{name: "verified TLS sends projected token", scheme: "https", wantStatus: http.StatusOK},
		{name: "wrong TLS identity", scheme: "https", serverName: "wrong.local", wantRequestError: true},
		{name: "plain HTTP refused", scheme: "http", wantInitError: true},
		{name: "missing token file", scheme: "https", missingToken: true, wantInitError: true},
		{name: "missing CA file", scheme: "https", missingCA: true, wantInitError: true},
		{name: "redirect does not forward token", scheme: "https", redirect: true, wantStatus: http.StatusTemporaryRedirect},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var calls atomic.Int32
			target := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				calls.Add(1)
				assert.Equal(t, "Bearer test-site-manager-token", r.Header.Get("Authorization"))
				if tc.redirect {
					http.Redirect(w, r, "/redirected", http.StatusTemporaryRedirect)
					return
				}
				w.WriteHeader(http.StatusOK)
			}))
			t.Cleanup(target.Close)
			dir := t.TempDir()
			caPath := filepath.Join(dir, "ca.crt")
			tokenPath := filepath.Join(dir, "token")
			require.NoError(t, os.WriteFile(caPath, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: target.Certificate().Raw}), 0600))
			require.NoError(t, os.WriteFile(tokenPath, []byte("test-site-manager-token"), 0600))
			if tc.missingToken {
				tokenPath += ".absent"
			}
			if tc.missingCA {
				caPath += ".absent"
			}
			endpoint := tc.scheme + target.URL[len("https"):]
			client, err := newCertificateClient(Options{credsMgrURL: endpoint, credsMgrTokenFile: tokenPath, credsMgrCAFile: caPath, credsMgrServerName: tc.serverName})
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
