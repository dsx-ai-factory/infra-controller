// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"encoding/pem"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/site-manager/internal/certmanager"
	crdsv1 "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/crds/v1"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/types"
)

const testCAPEM = "CA PEM"

// newCertManager returns a client for a Cert Manager stub that issues a fixed
// key and certificate and serves testCAPEM.
func newCertManager(t *testing.T) *certmanager.Client {
	mux := http.NewServeMux()
	mux.HandleFunc("POST /v1/pki/cloud-cert", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte(`{"key":"KEY PEM","certificate":"CERT PEM"}`))
	})
	mux.HandleFunc("GET /v1/pki/ca/pem", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte(testCAPEM))
	})
	srv := httptest.NewTLSServer(mux)
	t.Cleanup(srv.Close)

	caPath := filepath.Join(t.TempDir(), "ca.pem")
	caPEM := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: srv.Certificate().Raw})
	require.NoError(t, os.WriteFile(caPath, caPEM, 0600))
	client, err := certmanager.NewClient(srv.URL, caPath)
	require.NoError(t, err)
	return client
}

// TestCreateSiteCredsHandler checks that only a Site waiting for its
// handshake with a matching, unexpired OTP gets credentials. Site Agent
// treats every rejection as a failure, so each one is `500`.
func TestCreateSiteCredsHandler(t *testing.T) {
	certManager := newCertManager(t)
	valid := time.Now().Add(time.Hour)
	request := `{"siteuuid":"site-uuid","otp":"otp"}`

	tests := []struct {
		name        string
		site        *crdsv1.Site
		wantStatus  int
		wantMessage string
		wantState   string
	}{
		{
			name:       "a matching OTP gets credentials and completes the handshake",
			site:       newSite(crdsv1.SiteAwaitHandshake, "otp", valid),
			wantStatus: http.StatusOK,
			wantState:  crdsv1.SiteHandshakeComplete,
		},
		{
			name:        "a wrong OTP is rejected",
			site:        newSite(crdsv1.SiteAwaitHandshake, "other", valid),
			wantStatus:  http.StatusInternalServerError,
			wantMessage: "Bad OTP",
			wantState:   crdsv1.SiteAwaitHandshake,
		},
		{
			name:        "a used OTP is rejected",
			site:        newSite(crdsv1.SiteHandshakeComplete, "otp", valid),
			wantStatus:  http.StatusInternalServerError,
			wantMessage: "OTP already used",
			wantState:   crdsv1.SiteHandshakeComplete,
		},
		{
			name:        "an expired OTP is rejected",
			site:        newSite(crdsv1.SiteAwaitHandshake, "otp", time.Now().Add(-time.Minute)),
			wantStatus:  http.StatusInternalServerError,
			wantMessage: "OTP expired",
			wantState:   crdsv1.SiteAwaitHandshake,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			sites := newSites(tt.site)
			rec := serve(t, NewCreateSiteCredsHandler(sites, certManager), request, "")
			assert.Equal(t, tt.wantStatus, rec.Code, rec.Body.String())
			assert.Equal(t, tt.wantState, getSite(t, sites).Status.BootstrapState)

			if tt.wantStatus == http.StatusOK {
				got := types.SiteCredsResponse{}
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &got))
				assert.Equal(t, types.SiteCredsResponse{Key: "KEY PEM", Certificate: "CERT PEM", CACertificate: testCAPEM}, got)
				return
			}
			got := struct{ Message string }{}
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &got))
			assert.Equal(t, tt.wantMessage, got.Message)
		})
	}
}
