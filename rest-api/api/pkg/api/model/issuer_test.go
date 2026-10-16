// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
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
			name: "org_attribute_rejected",
			req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com",
				ClaimMappings: APIAuthIssuerClaimMappings{{OrgAttribute: "org", RolesAttribute: "roles"}}},
			wantErr: true,
		},
		{
			name: "org_display_attribute_rejected",
			req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com",
				ClaimMappings: APIAuthIssuerClaimMappings{{OrgName: "acme", OrgDisplayAttribute: "org_display"}}},
			wantErr: true,
		},
		{
			name: "roles_attribute_rejected",
			req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com",
				ClaimMappings: APIAuthIssuerClaimMappings{{OrgName: "acme", RolesAttribute: "roles"}}},
			wantErr: true,
		},
		{
			name: "missing_org_name",
			req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com",
				ClaimMappings: APIAuthIssuerClaimMappings{{Roles: []string{"TENANT_ADMIN"}}}},
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

func TestAPIAuthIssuerCreateOrUpdateRequest_ValidateExternalHosts(t *testing.T) {
	tests := []struct {
		name    string
		req     APIAuthIssuerCreateOrUpdateRequest
		wantErr bool
	}{
		{name: "public_hosts", req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com", JWKSUrl: "https://keys.acme.com/jwks"}},
		{name: "public_ip", req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://203.0.113.10/realm"}},
		{name: "loopback_ip", req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com", JWKSUrl: "http://127.0.0.1:8080/jwks"}, wantErr: true},
		{name: "link_local_metadata", req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com", JWKSUrl: "http://169.254.169.254/latest/meta-data/"}, wantErr: true},
		{name: "private_ip", req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "http://10.0.0.5/realm"}, wantErr: true},
		{name: "ipv6_loopback", req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "http://[::1]/realm"}, wantErr: true},
		{name: "localhost", req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "http://localhost:8080/realm"}, wantErr: true},
		{name: "cluster_service", req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com", JWKSUrl: "http://postgres.nico-rest.svc.cluster.local:5432/"}, wantErr: true},
		{name: "short_service", req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "https://idp.acme.com", JWKSUrl: "http://keycloak.keycloak.svc:8082/jwks"}, wantErr: true},
		{name: "malformed_left_to_validate", req: APIAuthIssuerCreateOrUpdateRequest{IssuerURL: "/relative"}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.ValidateExternalHosts()
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
	nilSource.FromDBModel(nil)
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
		}},
	}
	api := &APIAuthIssuer{}
	api.FromDBModel(db)
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
	empty.FromDBModel(&cdbm.Issuer{ID: id})
	assert.NotNil(t, empty.Audiences)
	assert.NotNil(t, empty.Scopes)
	assert.NotNil(t, empty.ClaimMappings)
}

// TestAPIAuthIssuer_FromDBModel_Status covers the readiness an operator polls for after a
// create the identity provider was not part of: the status is derived from the
// row's cached key set, and it takes both the keys and the fetch time, because
// hydration refuses a key set it cannot stamp.
func TestAPIAuthIssuer_FromDBModel_Status(t *testing.T) {
	fetchedAt := time.Now().UTC()
	keys := json.RawMessage(`{"keys":[{"kty":"RSA","use":"sig","kid":"key-1","alg":"RS256","n":"n","e":"AQAB"}]}`)

	tests := []struct {
		name       string
		keys       json.RawMessage
		fetchedAt  *time.Time
		wantStatus APIAuthIssuerStatus
	}{
		{
			name:       "never_fetched_is_pending",
			wantStatus: AuthIssuerStatusPending,
		},
		{
			name:       "cached_key_set_is_ready",
			keys:       keys,
			fetchedAt:  &fetchedAt,
			wantStatus: AuthIssuerStatusReady,
		},
		{
			name:       "keys_without_fetch_time_are_pending",
			keys:       keys,
			wantStatus: AuthIssuerStatusPending,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			api := &APIAuthIssuer{}
			api.FromDBModel(&cdbm.Issuer{ID: uuid.New(), JWKSKeys: tt.keys, JWKSFetchedAt: tt.fetchedAt})
			assert.Equal(t, tt.wantStatus, api.Status)
			assert.Equal(t, tt.fetchedAt, api.JWKSFetchedAt)
		})
	}
}

func TestAPIAuthIssuerCreateOrUpdateRequest_ToCreateOrUpdateInput(t *testing.T) {
	creator := uuid.New()
	req := &APIAuthIssuerCreateOrUpdateRequest{
		IssuerURL:     "https://idp.acme.com",
		ClaimMappings: nil,
	}
	in := req.ToCreateOrUpdateInput(&creator)
	assert.Equal(t, "https://idp.acme.com/.well-known/jwks.json", in.JWKSUrl)
	assert.Equal(t, &creator, in.ActorID)
	assert.Equal(t, "custom", in.Origin)
	assert.NotNil(t, in.ClaimMappings)
}
