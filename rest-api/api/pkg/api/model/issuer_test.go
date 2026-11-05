// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"
	"time"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestDefaultJWKSURL(t *testing.T) {
	assert.Equal(t, "https://idp.acme.com/.well-known/jwks.json", (APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com"}).DefaultJWKSURL())
	assert.Equal(t, "https://idp.acme.com/.well-known/jwks.json", (APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com/"}).DefaultJWKSURL())
	assert.Equal(t, "https://custom/jwks", (APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com", JWKSUrl: "https://custom/jwks"}).DefaultJWKSURL())
}

func TestAPIAuthIssuerCreateOrUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		req     APIAuthIssuerCreateOrUpdateRequest
		wantErr bool
	}{
		{
			name:    "missing_issuer_url",
			req:     APIAuthIssuerCreateOrUpdateRequest{},
			wantErr: true,
		},
		{
			name:    "whitespace_issuer_url",
			req:     APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "   "},
			wantErr: true,
		},
		{
			name: "issuer_url_without_claim_mappings",
			req:  APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com"},
		},
		{
			name:    "relative_issuer_url",
			req:     APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "/realms/acme"},
			wantErr: true,
		},
		{
			name:    "issuer_url_without_host",
			req:     APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https:///realms/acme"},
			wantErr: true,
		},
		{
			name:    "non_http_issuer_url",
			req:     APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "file:///tmp/issuer"},
			wantErr: true,
		},
		{
			name:    "relative_jwks_url",
			req:     APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com", JWKSUrl: "/jwks"},
			wantErr: true,
		},
		{
			name: "explicit_http_urls",
			req:  APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "http://idp.acme.test", JWKSUrl: "http://idp.acme.test/jwks"},
		},
		{
			name: "static_org_name_and_roles",
			req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com",
				ClaimMappings: APIAuthIssuerClaimMappings{{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}}}},
		},
		{
			name:    "service_account_switch_rejected",
			req:     APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com", ServiceAccount: true},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)
		})
	}
}

func TestAPIAuthIssuer_FromDBModel(t *testing.T) {
	var nilSource APIAuthIssuer
	nilSource.FromDBModel(nil, nil)
	assert.Empty(t, nilSource)

	id := uuid.New()
	db := &cdbm.Issuer{
		ID:          id,
		Origin:      "custom",
		IssuerURL:   "https://idp.acme.com",
		JWKSUrl:     "https://idp.acme.com/jwks",
		JWKSTimeout: "5s",
		Audiences:   []string{"api"},
		Scopes:      []string{"carbide"},
		ClaimMappings: []cdbm.ClaimMapping{{
			OrgName:   "acme",
			Roles:     []string{"TENANT_ADMIN"},
			Audiences: []string{"org-acme"},
			Scopes:    []string{"nico:read"},
			Source:    cdbm.IssuerSourceAPI,
		}},
	}
	api := &APIAuthIssuer{}
	api.FromDBModel(db, nil)
	assert.Equal(t, id.String(), api.ID)
	assert.Equal(t, "https://idp.acme.com", api.IssuerURL)
	require.Len(t, api.ClaimMappings, 1)
	assert.Equal(t, "acme", api.ClaimMappings[0].OrgName)
	assert.Equal(t, []string{"org-acme"}, api.ClaimMappings[0].Audiences)
	assert.Equal(t, []string{"nico:read"}, api.ClaimMappings[0].Scopes)

	// Per-mapping filters survive the round trip back to persistence.
	assert.Equal(t, db.ClaimMappings, APIAuthIssuerClaimMappings(api.ClaimMappings).ToDBModel())

	// nil slices are normalized to empty (stable JSON output)
	empty := &APIAuthIssuer{}
	empty.FromDBModel(&cdbm.Issuer{ID: id}, nil)
	assert.NotNil(t, empty.Audiences)
	assert.NotNil(t, empty.Scopes)
	assert.NotNil(t, empty.ClaimMappings)
}

// TestAPIAuthIssuer_FromDBModel_Status: status and its message are the stored
// values, and the history is the status_detail rows in the order given.
func TestAPIAuthIssuer_FromDBModel_Status(t *testing.T) {
	msg := "JWKS refresh failed, serving the cached key set"
	fetchedAt := time.Now().UTC()
	api := &APIAuthIssuer{}
	api.FromDBModel(&cdbm.Issuer{
		ID: uuid.New(), Status: cdbm.IssuerStatusDegraded, StatusMessage: &msg,
		JWKSFetchedAt: &fetchedAt, JWKSLastAttemptAt: &fetchedAt, JWKSLastError: &msg,
	}, []cdbm.StatusDetail{{Status: cdbm.IssuerStatusReady}, {Status: cdbm.IssuerStatusDegraded}})

	assert.Equal(t, AuthIssuerStatusDegraded, api.Status)
	assert.Equal(t, &msg, api.StatusMessage)
	assert.Equal(t, &fetchedAt, api.JWKSFetchedAt)
	assert.Equal(t, &msg, api.JWKSLastError)
	require.Len(t, api.StatusHistory, 2)
	assert.Equal(t, cdbm.IssuerStatusReady, api.StatusHistory[0].Status)
}

func TestAPIAuthIssuerCreateOrUpdateRequest_ToCreateOrUpdateInput(t *testing.T) {
	creator := uuid.New()
	req := &APIAuthIssuerCreateOrUpdateRequest{
		IssuerURL:     "https://idp.acme.com",
		ClaimMappings: nil,
	}
	in := req.ToCreateOrUpdateInput(&creator)
	assert.Equal(t, "https://idp.acme.com/.well-known/jwks.json", in.JWKSUrl)
	assert.Equal(t, &creator, in.CreatedBy)
	assert.Equal(t, &creator, in.UpdatedBy)
	assert.Equal(t, "custom", in.Origin)
	assert.NotNil(t, in.ClaimMappings)
}
