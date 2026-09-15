// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sitemgr

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	csmtypes "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/types"
	"github.com/google/uuid"
	"github.com/gorilla/mux"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// testUtils simulates a Site Manager service.
type testUtils struct {
	l        net.Listener
	srv      *httptest.Server
	forceErr bool
	sites    map[string]*csmtypes.SiteGetResponse
}

func (util *testUtils) setup(t *testing.T) {
	util.sites = make(map[string]*csmtypes.SiteGetResponse)
	l, err := net.Listen("tcp", "localhost:0")
	require.NoError(t, err)
	util.l = l

	rtr := mux.NewRouter()

	// POST /v1/site creates a new site.
	rtr.Methods(http.MethodPost).Path("/v1/site").HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if util.forceErr {
			http.Error(w, "forced error", http.StatusInternalServerError)
			return
		}

		var req csmtypes.SiteCreateRequest
		content, err := io.ReadAll(r.Body)
		require.NoError(t, err)
		err = json.Unmarshal(content, &req)
		require.NoError(t, err)
		// Return an OTP and expiry
		util.sites[req.SiteUUID] = &csmtypes.SiteGetResponse{
			OTP:       "test-otp",
			OTPExpiry: time.Now().Add(24 * time.Hour).Format("2006-01-02 15:04:05.999999999 -0700 MST"),
		}
	})

	// GET /v1/site/{id} returns a site's OTP information.
	rtr.Methods(http.MethodGet).Path("/v1/site/{id}").HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if util.forceErr {
			http.Error(w, "forced error", http.StatusNotFound)
			return
		}
		vars := mux.Vars(r)
		siteID := vars["id"]
		resp := util.sites[siteID]
		if resp != nil {
			c, err := json.Marshal(resp)
			require.NoError(t, err)
			w.Header().Set("Content-Type", "application/json")
			_, err = w.Write(c)
			require.NoError(t, err)
		}
	})

	// DELETE /v1/site/{id} deletes a site.
	rtr.Methods(http.MethodDelete).Path("/v1/site/{id}").HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if util.forceErr {
			http.Error(w, "forced error", http.StatusNotFound)
			return
		}
		vars := mux.Vars(r)
		siteID := vars["id"]
		delete(util.sites, siteID)
		w.WriteHeader(http.StatusOK)
	})

	// POST /v1/site/roll/{id} rolls a site.
	rtr.Methods(http.MethodPost).Path("/v1/site/roll/{id}").HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		if util.forceErr {
			http.Error(w, "forced error", http.StatusNotFound)
			return
		}
		w.WriteHeader(http.StatusOK)
	})

	util.srv = httptest.NewUnstartedServer(rtr)
	util.srv.Listener = l
	util.srv.StartTLS()
}

func (util *testUtils) getURL() string {
	// Return the base URL for Site Manager operations.
	return fmt.Sprintf("https://%s/v1/site", util.l.Addr().String())
}

func (util *testUtils) teardown() {
	util.srv.Close()
}

func TestUtils(t *testing.T) {
	tcsm := &testUtils{}
	tcsm.setup(t)
	defer tcsm.teardown()

	url := tcsm.getURL()
	logger := log.With().Str("Test", "CSM").Logger()

	// Define our test site parameters.
	siteUUID := uuid.New().String()
	siteName := "testSite"
	provider := "testProvider"
	fcOrg := "testOrg"

	// Create a site.
	err := CreateSite(context.Background(), logger, siteUUID, siteName, provider, fcOrg, url)
	assert.NoError(t, err)

	// Roll the site.
	err = RollSite(context.Background(), logger, siteUUID, siteName, url)
	assert.NoError(t, err)

	// Retrieve OTP for the site.
	otp, _, err := GetSiteOTP(context.Background(), logger, siteUUID, url)
	assert.NoError(t, err)
	assert.NotNil(t, otp)

	// Delete the site.
	err = DeleteSite(context.Background(), logger, siteUUID, url)
	assert.NoError(t, err)

	// Now force errors.
	tcsm.forceErr = true

	err = CreateSite(context.Background(), logger, siteUUID, siteName, provider, fcOrg, url)
	assert.Error(t, err)

	err = RollSite(context.Background(), logger, siteUUID, siteName, url)
	assert.Error(t, err)

	_, _, err = GetSiteOTP(context.Background(), logger, siteUUID, url)
	assert.Error(t, err)

	err = DeleteSite(context.Background(), logger, siteUUID, url)
	// When DELETE receives a forced error, our helper returns ErrSiteNotFound.
	assert.Equal(t, ErrSiteNotFound, err)
}
