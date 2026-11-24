// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	k8serr "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"

	fakecrdclient "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/client/clientset/versioned/fake"
	crdclientv1 "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/client/clientset/versioned/typed/crds/v1"
	crdsv1 "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/crds/v1"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/types"
)

const (
	testNamespace = "site-manager"
	testUUID      = "site-uuid"
)

// newSites returns a fake Site client that holds existing.
func newSites(existing ...runtime.Object) crdclientv1.SiteInterface {
	return fakecrdclient.NewSimpleClientset(existing...).ForgeV1().Sites(testNamespace)
}

// newSite returns the Site resource for testUUID with state and an OTP that
// expires at expiry.
func newSite(state, passcode string, expiry time.Time) *crdsv1.Site {
	timestamp, _ := expiry.MarshalText()
	return &crdsv1.Site{
		ObjectMeta: metav1.ObjectMeta{Name: nameFromUUID(testUUID), Namespace: testNamespace},
		Spec:       crdsv1.SiteSpec{UUID: testUUID, SiteName: "site", Provider: "provider", FCOrg: "org"},
		Status: crdsv1.SiteStatus{
			OTP:            crdsv1.OTPInfo{Passcode: passcode, Timestamp: string(timestamp)},
			BootstrapState: state,
		},
	}
}

// serve runs h for a request with body and, when set, the uuid path
// parameter.
func serve(t *testing.T, h interface{ Handle(echo.Context) error }, body, uuid string) *httptest.ResponseRecorder {
	req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(body))
	rec := httptest.NewRecorder()
	c := echo.New().NewContext(req, rec)
	if uuid != "" {
		c.SetParamNames("uuid")
		c.SetParamValues(uuid)
	}
	require.NoError(t, h.Handle(c))
	return rec
}

// getSite returns the stored Site for testUUID.
func getSite(t *testing.T, sites crdclientv1.SiteInterface) *crdsv1.Site {
	site, err := sites.Get(context.Background(), nameFromUUID(testUUID), metav1.GetOptions{})
	require.NoError(t, err)
	return site
}

// TestSiteHandlers checks each Site route's status codes. The REST API treats
// `404` from delete as already deleted. Every other non-200 is a failure.
func TestSiteHandlers(t *testing.T) {
	expiry := time.Now().Add(time.Hour).Truncate(time.Second)

	tests := []struct {
		name       string
		existing   []runtime.Object
		handler    func(sites crdclientv1.SiteInterface) interface{ Handle(echo.Context) error }
		body       string
		uuid       string
		wantStatus int
		check      func(t *testing.T, sites crdclientv1.SiteInterface, rec *httptest.ResponseRecorder)
	}{
		{
			name: "create stores the Site waiting for its handshake",
			handler: func(sites crdclientv1.SiteInterface) interface{ Handle(echo.Context) error } {
				return NewCreateSiteHandler(sites, time.Hour)
			},
			body:       `{"siteuuid":"site-uuid","name":"site","provider":"provider","fcorg":"org"}`,
			wantStatus: http.StatusOK,
			check: func(t *testing.T, sites crdclientv1.SiteInterface, _ *httptest.ResponseRecorder) {
				site := getSite(t, sites)
				assert.Equal(t, crdsv1.SiteSpec{UUID: testUUID, SiteName: "site", Provider: "provider", FCOrg: "org"}, site.Spec)
				assert.Equal(t, crdsv1.SiteAwaitHandshake, site.Status.BootstrapState)
				assert.NotEmpty(t, site.Status.OTP.Passcode)
				otpExpiry, err := parseExpiry(&site.Status.OTP)
				require.NoError(t, err)
				assert.WithinDuration(t, time.Now().Add(time.Hour), *otpExpiry, time.Minute)
			},
		},
		{
			name: "create rejects an existing Site",
			handler: func(sites crdclientv1.SiteInterface) interface{ Handle(echo.Context) error } {
				return NewCreateSiteHandler(sites, time.Hour)
			},
			existing:   []runtime.Object{newSite(crdsv1.SiteAwaitHandshake, "otp", expiry)},
			body:       `{"siteuuid":"site-uuid"}`,
			wantStatus: http.StatusBadRequest,
		},
		{
			name: "create rejects a body that isn't JSON",
			handler: func(sites crdclientv1.SiteInterface) interface{ Handle(echo.Context) error } {
				return NewCreateSiteHandler(sites, time.Hour)
			},
			body:       "not json",
			wantStatus: http.StatusBadRequest,
		},
		{
			name: "get returns the Site with its OTP",
			handler: func(sites crdclientv1.SiteInterface) interface{ Handle(echo.Context) error } {
				return NewGetSiteHandler(sites)
			},
			existing:   []runtime.Object{newSite(crdsv1.SiteAwaitHandshake, "otp", expiry)},
			uuid:       testUUID,
			wantStatus: http.StatusOK,
			check: func(t *testing.T, _ crdclientv1.SiteInterface, rec *httptest.ResponseRecorder) {
				got := types.SiteGetResponse{}
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &got))
				assert.Equal(t, types.SiteGetResponse{
					SiteUUID:       testUUID,
					Name:           "site",
					Provider:       "provider",
					FCOrg:          "org",
					BootstrapState: crdsv1.SiteAwaitHandshake,
					OTP:            "otp",
					OTPExpiry:      expiry.String(),
				}, got)
			},
		},
		{
			name: "delete removes the Site",
			handler: func(sites crdclientv1.SiteInterface) interface{ Handle(echo.Context) error } {
				return NewDeleteSiteHandler(sites)
			},
			existing:   []runtime.Object{newSite(crdsv1.SiteAwaitHandshake, "otp", expiry)},
			uuid:       testUUID,
			wantStatus: http.StatusOK,
			check: func(t *testing.T, sites crdclientv1.SiteInterface, _ *httptest.ResponseRecorder) {
				_, err := sites.Get(context.Background(), nameFromUUID(testUUID), metav1.GetOptions{})
				assert.True(t, k8serr.IsNotFound(err), "Site still exists: %v", err)
			},
		},
		{
			name: "delete of an unknown Site is 404",
			handler: func(sites crdclientv1.SiteInterface) interface{ Handle(echo.Context) error } {
				return NewDeleteSiteHandler(sites)
			},
			uuid:       testUUID,
			wantStatus: http.StatusNotFound,
		},
		{
			name: "register completes the registration",
			handler: func(sites crdclientv1.SiteInterface) interface{ Handle(echo.Context) error } {
				return NewRegisterSiteHandler(sites)
			},
			existing:   []runtime.Object{newSite(crdsv1.SiteHandshakeComplete, "otp", expiry)},
			uuid:       testUUID,
			wantStatus: http.StatusOK,
			check: func(t *testing.T, sites crdclientv1.SiteInterface, _ *httptest.ResponseRecorder) {
				assert.Equal(t, crdsv1.SiteRegistrationComplete, getSite(t, sites).Status.BootstrapState)
			},
		},
		{
			name: "roll replaces the OTP and waits for a new handshake",
			handler: func(sites crdclientv1.SiteInterface) interface{ Handle(echo.Context) error } {
				return NewRollSiteHandler(sites, time.Hour)
			},
			existing:   []runtime.Object{newSite(crdsv1.SiteRegistrationComplete, "otp", expiry)},
			uuid:       testUUID,
			wantStatus: http.StatusOK,
			check: func(t *testing.T, sites crdclientv1.SiteInterface, _ *httptest.ResponseRecorder) {
				site := getSite(t, sites)
				assert.Equal(t, crdsv1.SiteAwaitHandshake, site.Status.BootstrapState)
				assert.NotEqual(t, "otp", site.Status.OTP.Passcode)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			sites := newSites(tt.existing...)
			rec := serve(t, tt.handler(sites), tt.body, tt.uuid)
			assert.Equal(t, tt.wantStatus, rec.Code, rec.Body.String())
			if tt.check != nil {
				tt.check(t, sites, rec)
			}
		})
	}
}
