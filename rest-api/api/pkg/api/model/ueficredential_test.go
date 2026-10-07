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

func TestAPIUEFICredentialRequestValidate(t *testing.T) {
	siteID := uuid.NewString()
	cases := []struct {
		name    string
		req     APIUEFICredentialRequest
		wantErr bool
	}{
		{"Host ok", APIUEFICredentialRequest{SiteID: siteID, Kind: UEFICredentialKindHost, Password: "pw"}, false},
		{"DPU ok", APIUEFICredentialRequest{SiteID: siteID, Kind: UEFICredentialKindDPU, Password: "pw"}, false},
		{"missing siteId", APIUEFICredentialRequest{Kind: UEFICredentialKindHost, Password: "pw"}, true},
		{"invalid siteId", APIUEFICredentialRequest{SiteID: "bad-site-id", Kind: UEFICredentialKindHost, Password: "pw"}, true},
		{"missing password", APIUEFICredentialRequest{SiteID: siteID, Kind: UEFICredentialKindHost}, true},
		{"invalid kind", APIUEFICredentialRequest{SiteID: siteID, Kind: UEFICredentialKind("nope"), Password: "pw"}, true},
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

func TestAPIUEFICredentialRequestToProto(t *testing.T) {
	for _, tc := range []struct {
		kind           UEFICredentialKind
		credentialType corev1.CredentialType
	}{
		{UEFICredentialKindHost, corev1.CredentialType_HostUefi},
		{UEFICredentialKindDPU, corev1.CredentialType_DpuUefi},
	} {
		t.Run(string(tc.kind), func(t *testing.T) {
			req := APIUEFICredentialRequest{Kind: tc.kind, Password: "pw"}
			p := req.ToProto()
			assert.Equal(t, tc.credentialType, p.GetCredentialType())
			assert.Equal(t, "pw", p.GetPassword())
		})
	}
}

func TestAPIUEFICredentialRequestToResponseOmitsPassword(t *testing.T) {
	req := APIUEFICredentialRequest{
		SiteID:   uuid.NewString(),
		Kind:     UEFICredentialKindHost,
		Password: "pw",
	}

	resp := req.ToResponse()
	assert.Equal(t, req.SiteID, resp.SiteID)
	assert.Equal(t, req.Kind, resp.Kind)

	body, err := json.Marshal(resp)
	require.NoError(t, err)
	assert.NotContains(t, string(body), "password")
	assert.NotContains(t, string(body), req.Password)
}
