// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func bmcStrPtr(s string) *string { return &s }

func TestAPIBMCCredentialRequestValidate(t *testing.T) {
	mac := bmcStrPtr("aa:bb:cc:dd:ee:ff")
	siteID := uuid.NewString()
	cases := []struct {
		name    string
		req     APIBMCCredentialRequest
		wantErr bool
	}{
		{"SiteWideRoot ok", APIBMCCredentialRequest{SiteID: siteID, Kind: BMCCredentialKindSiteWideRoot, Password: "pw"}, false},
		{"BMCRoot ok", APIBMCCredentialRequest{SiteID: siteID, Kind: BMCCredentialKindBMCRoot, Password: "pw", MacAddress: mac}, false},
		{"missing siteId", APIBMCCredentialRequest{Kind: BMCCredentialKindSiteWideRoot, Password: "pw"}, true},
		{"invalid siteId", APIBMCCredentialRequest{SiteID: "bad-site-id", Kind: BMCCredentialKindSiteWideRoot, Password: "pw"}, true},
		{"BMCRoot missing mac", APIBMCCredentialRequest{SiteID: siteID, Kind: BMCCredentialKindBMCRoot, Password: "pw"}, true},
		{"missing password", APIBMCCredentialRequest{SiteID: siteID, Kind: BMCCredentialKindSiteWideRoot}, true},
		{"invalid kind", APIBMCCredentialRequest{SiteID: siteID, Kind: "nope", Password: "pw"}, true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.req.Validate()
			if tc.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestAPIBMCCredentialRequestToProto(t *testing.T) {
	req := APIBMCCredentialRequest{
		SiteID:     uuid.NewString(),
		Kind:       BMCCredentialKindBMCRoot,
		Password:   "pw",
		Username:   bmcStrPtr("root"),
		MacAddress: bmcStrPtr("aa:bb:cc:dd:ee:ff"),
	}
	p := req.ToProto()
	assert.Equal(t, corev1.CredentialType_RootBmcByMacAddress, p.GetCredentialType())
	assert.Equal(t, "pw", p.GetPassword())
	assert.Equal(t, "root", p.GetUsername())
	assert.Equal(t, "aa:bb:cc:dd:ee:ff", p.GetMacAddress())

	site := APIBMCCredentialRequest{Kind: BMCCredentialKindSiteWideRoot, Password: "pw"}
	assert.Equal(t, corev1.CredentialType_SiteWideBmcRoot, site.ToProto().GetCredentialType())
}

func TestAPIBMCCredentialRequestToResponseOmitsPassword(t *testing.T) {
	req := APIBMCCredentialRequest{
		SiteID:     uuid.NewString(),
		Kind:       BMCCredentialKindBMCRoot,
		Password:   "pw",
		Username:   bmcStrPtr("root"),
		MacAddress: bmcStrPtr("aa:bb:cc:dd:ee:ff"),
	}

	resp := req.ToResponse()
	assert.Equal(t, req.SiteID, resp.SiteID)
	assert.Equal(t, req.Kind, resp.Kind)
	assert.Equal(t, req.Username, resp.Username)
	assert.Equal(t, req.MacAddress, resp.MacAddress)

	body, err := json.Marshal(resp)
	require.NoError(t, err)
	assert.NotContains(t, string(body), "password")
	assert.NotContains(t, string(body), req.Password)
}
