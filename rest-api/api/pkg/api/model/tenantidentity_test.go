// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"math"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// TestAPITenantIdentityConfigCreateOrUpdateRequest_Validate verifies required fields, audience allowlist rules, and rotateKey/signingKeyOverlapSeconds coupling on the tenant identity create-or-update request.
func TestAPITenantIdentityConfigCreateOrUpdateRequest_Validate(t *testing.T) {
	newValidRequest := func() APITenantIdentityConfigCreateOrUpdateRequest {
		return APITenantIdentityConfigCreateOrUpdateRequest{
			Enabled:         cutil.GetPtr(true),
			DefaultAudience: "openbao",
			Issuer:          "https://issuer.example.com/",
			TokenTtlSeconds: 600,
		}
	}

	withIssuer := func(issuer string) APITenantIdentityConfigCreateOrUpdateRequest {
		req := newValidRequest()
		req.Issuer = issuer
		return req
	}
	withTokenTtlSeconds := func(tokenTtlSeconds uint32) APITenantIdentityConfigCreateOrUpdateRequest {
		req := newValidRequest()
		req.TokenTtlSeconds = tokenTtlSeconds
		return req
	}

	tests := []struct {
		name    string
		req     APITenantIdentityConfigCreateOrUpdateRequest
		wantErr bool
	}{
		{name: "minimum valid", req: newValidRequest()},
		{name: "missing enabled accepted (defaults to true at ToProto)",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.Enabled = nil
				return req
			}()},
		{name: "enabled false accepted",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.Enabled = cutil.GetPtr(false)
				return req
			}()},
		{name: "missing defaultAudience rejected",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.DefaultAudience = ""
				return req
			}(),
			wantErr: true},
		{name: "missing issuer rejected",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.Issuer = ""
				return req
			}(),
			wantErr: true},
		{name: "missing tokenTtlSeconds rejected",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.TokenTtlSeconds = 0
				return req
			}(),
			wantErr: true},
		{name: "empty allowedAudiences accepted",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.AllowedAudiences = []string{}
				return req
			}()},

		{name: "issuer template with {org} placeholder accepted",
			req: withIssuer("https://issuer.example.com/{org}")},
		{name: "tokenTtlSeconds zero rejected",
			req:     withTokenTtlSeconds(0),
			wantErr: true},
		{name: "defaultAudience missing from non-empty allowedAudiences rejected",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.AllowedAudiences = []string{"vault", "spire"}
				return req
			}(),
			wantErr: true},
		{name: "defaultAudience present in non-empty allowedAudiences accepted",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.AllowedAudiences = []string{"openbao", "vault"}
				return req
			}()},

		{name: "rotateKey without signingKeyOverlapSeconds rejected",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.RotateKey = cutil.GetPtr(true)
				return req
			}(),
			wantErr: true},
		{name: "signingKeyOverlapSeconds without rotateKey rejected",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.SigningKeyOverlapSeconds = cutil.GetPtr(uint32(900))
				return req
			}(),
			wantErr: true},
		{name: "rotateKey + signingKeyOverlapSeconds below tokenTtlSeconds rejected",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.RotateKey = cutil.GetPtr(true)
				req.SigningKeyOverlapSeconds = cutil.GetPtr(uint32(300))
				return req
			}(),
			wantErr: true},
		{name: "rotateKey + signingKeyOverlapSeconds equal to tokenTtlSeconds accepted",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.RotateKey = cutil.GetPtr(true)
				req.SigningKeyOverlapSeconds = cutil.GetPtr(uint32(600))
				return req
			}()},
		{name: "rotateKey + signingKeyOverlapSeconds above tokenTtlSeconds accepted",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.RotateKey = cutil.GetPtr(true)
				req.SigningKeyOverlapSeconds = cutil.GetPtr(uint32(3600))
				return req
			}()},
		{name: "rotateKey explicit false without signingKeyOverlapSeconds accepted",
			req: func() APITenantIdentityConfigCreateOrUpdateRequest {
				req := newValidRequest()
				req.RotateKey = cutil.GetPtr(false)
				return req
			}()},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

// TestAPITenantIdentityConfigCreateOrUpdateRequest_ToProto verifies the tenant identity create-or-update request maps populated and nil fields onto the SetTenantIdentityConfig proto as expected.
func TestAPITenantIdentityConfigCreateOrUpdateRequest_ToProto(t *testing.T) {
	t.Run("populates all fields", func(t *testing.T) {
		maxUint32 := uint32(math.MaxUint32)
		protoReq := APITenantIdentityConfigCreateOrUpdateRequest{
			Enabled: cutil.GetPtr(true),
			Issuer:  "https://carbide.example.com/iss", DefaultAudience: "openbao",
			AllowedAudiences: []string{"openbao", "vault"}, TokenTtlSeconds: maxUint32,
			SubjectPrefix: cutil.GetPtr("spiffe://carbide.nvidia.com"), RotateKey: cutil.GetPtr(true),
			SigningKeyOverlapSeconds: &maxUint32,
		}.ToProto("acme-corp")
		require.NotNil(t, protoReq)
		assert.Equal(t, "acme-corp", protoReq.GetOrganizationId())
		cfg := protoReq.GetConfig()
		require.NotNil(t, cfg)
		assert.True(t, cfg.GetEnabled())
		assert.Equal(t, "https://carbide.example.com/iss", cfg.GetIssuer())
		assert.Equal(t, "openbao", cfg.GetDefaultAudience())
		assert.Equal(t, []string{"openbao", "vault"}, cfg.GetAllowedAudiences())
		assert.Equal(t, maxUint32, cfg.GetTokenTtlSec())
		assert.Equal(t, "spiffe://carbide.nvidia.com", cfg.GetSubjectPrefix())
		assert.True(t, cfg.GetRotateKey())
		assert.Equal(t, maxUint32, cfg.GetSigningKeyOverlapSec())
	})

	t.Run("enabled true maps to proto true", func(t *testing.T) {
		protoReq := APITenantIdentityConfigCreateOrUpdateRequest{
			Enabled: cutil.GetPtr(true), Issuer: "https://carbide.example.com/iss", DefaultAudience: "openbao", TokenTtlSeconds: 600,
		}.ToProto("acme-corp")
		assert.True(t, protoReq.GetConfig().GetEnabled())
	})

	t.Run("enabled false when explicitly false", func(t *testing.T) {
		protoReq := APITenantIdentityConfigCreateOrUpdateRequest{
			Issuer: "https://carbide.example.com/iss", DefaultAudience: "openbao", TokenTtlSeconds: 600, Enabled: cutil.GetPtr(false),
		}.ToProto("acme-corp")
		assert.False(t, protoReq.GetConfig().GetEnabled())
	})

	t.Run("nil enabled defaults to proto true", func(t *testing.T) {
		protoReq := APITenantIdentityConfigCreateOrUpdateRequest{
			Issuer: "https://carbide.example.com/iss", DefaultAudience: "openbao", TokenTtlSeconds: 600,
		}.ToProto("acme-corp")
		assert.True(t, protoReq.GetConfig().GetEnabled())
	})

	t.Run("nil rotateKey leaves proto field false", func(t *testing.T) {
		protoReq := APITenantIdentityConfigCreateOrUpdateRequest{
			Enabled: cutil.GetPtr(true), Issuer: "https://carbide.example.com/iss", DefaultAudience: "openbao", TokenTtlSeconds: 600,
		}.ToProto("acme-corp")
		assert.False(t, protoReq.GetConfig().GetRotateKey())
	})

	t.Run("nil subjectPrefix leaves proto field unset", func(t *testing.T) {
		protoReq := APITenantIdentityConfigCreateOrUpdateRequest{
			Enabled: cutil.GetPtr(true), Issuer: "https://carbide.example.com/iss", DefaultAudience: "openbao", TokenTtlSeconds: 600,
		}.ToProto("acme-corp")
		assert.Nil(t, protoReq.GetConfig().SubjectPrefix)
	})

	t.Run("nil signingKeyOverlapSeconds leaves proto field unset", func(t *testing.T) {
		protoReq := APITenantIdentityConfigCreateOrUpdateRequest{
			Enabled: cutil.GetPtr(true), Issuer: "https://carbide.example.com/iss", DefaultAudience: "openbao", TokenTtlSeconds: 600,
		}.ToProto("acme-corp")
		assert.Nil(t, protoReq.GetConfig().SigningKeyOverlapSec)
	})
}

// TestAPITenantIdentityConfig_FromResponseProto verifies the tenant identity config response correctly mirrors the gRPC reply including signing-key rotation overlap state.
func TestAPITenantIdentityConfig_FromResponseProto(t *testing.T) {
	t.Run("full proto, single signing key", func(t *testing.T) {
		maxUint32 := uint32(math.MaxUint32)
		created := time.Date(2026, 4, 20, 12, 0, 0, 0, time.UTC)
		updated := time.Date(2026, 4, 21, 12, 0, 0, 0, time.UTC)
		subjectPrefix := "spiffe://carbide.nvidia.com"
		resp := &APITenantIdentityConfig{}
		resp.FromResponseProto(&corev1.TenantIdentityConfigResponse{
			OrganizationId: "acme-corp",
			Config: &corev1.TenantIdentityConfig{
				Enabled: true, Issuer: "https://carbide.example.com/iss",
				DefaultAudience: "openbao", AllowedAudiences: []string{"openbao"},
				TokenTtlSec: maxUint32, SubjectPrefix: &subjectPrefix,
			},
			SigningKeys: []*corev1.TenantIdentitySigningKey{
				{Kid: "key-123", Alg: "ES256", CurrentSigner: true},
			},
			CreatedAt: timestamppb.New(created), UpdatedAt: timestamppb.New(updated),
		})
		assert.Equal(t, "acme-corp", resp.Org)
		assert.True(t, resp.Enabled)
		assert.Equal(t, "https://carbide.example.com/iss", resp.Issuer)
		assert.Equal(t, "openbao", resp.DefaultAudience)
		assert.Equal(t, []string{"openbao"}, resp.AllowedAudiences)
		assert.Equal(t, maxUint32, resp.TokenTtlSeconds)
		assert.Equal(t, "spiffe://carbide.nvidia.com", resp.SubjectPrefix)
		require.Len(t, resp.SigningKeys, 1)
		assert.Equal(t, "key-123", resp.SigningKeys[0].Kid)
		assert.Equal(t, "ES256", resp.SigningKeys[0].Alg)
		assert.True(t, resp.SigningKeys[0].CurrentSigner)
		assert.Nil(t, resp.SigningKeys[0].ExpireAt)
		assert.Equal(t, created, resp.Created)
		assert.Equal(t, updated, resp.Updated)
	})

	t.Run("rotation overlap: two signing keys, inactive one carries expireAt", func(t *testing.T) {
		expire := time.Date(2026, 5, 12, 9, 30, 0, 0, time.UTC)
		resp := &APITenantIdentityConfig{}
		resp.FromResponseProto(&corev1.TenantIdentityConfigResponse{
			OrganizationId: "acme-corp",
			Config:         &corev1.TenantIdentityConfig{Enabled: true},
			SigningKeys: []*corev1.TenantIdentitySigningKey{
				{Kid: "kid-old", Alg: "ES256", CurrentSigner: false, ExpireAt: timestamppb.New(expire)},
				{Kid: "kid-new", Alg: "ES256", CurrentSigner: true},
			},
		})
		require.Len(t, resp.SigningKeys, 2)
		assert.Equal(t, "kid-old", resp.SigningKeys[0].Kid)
		assert.False(t, resp.SigningKeys[0].CurrentSigner)
		require.NotNil(t, resp.SigningKeys[0].ExpireAt)
		assert.Equal(t, expire, *resp.SigningKeys[0].ExpireAt)
		assert.Equal(t, "kid-new", resp.SigningKeys[1].Kid)
		assert.True(t, resp.SigningKeys[1].CurrentSigner)
		assert.Nil(t, resp.SigningKeys[1].ExpireAt)
	})

	t.Run("minimal proto (no Config, no signing keys)", func(t *testing.T) {
		resp := &APITenantIdentityConfig{}
		resp.FromResponseProto(&corev1.TenantIdentityConfigResponse{OrganizationId: "acme-corp"})
		assert.Equal(t, "acme-corp", resp.Org)
		assert.Empty(t, resp.SigningKeys)
		assert.False(t, resp.Enabled)
	})
}

// TestAPIReencryptTenantIdentitySecretsRequest_Validate verifies omission targets all organizations and a supplied organization must be non-empty.
func TestAPIReencryptTenantIdentitySecretsRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		req     APIReencryptTenantIdentitySecretsRequest
		wantErr string
	}{
		{name: "organization omitted"},
		{
			name: "organization supplied",
			req: APIReencryptTenantIdentitySecretsRequest{
				OrganizationID: cutil.GetPtr("tenant-corp"),
				DryRun:         true,
			},
		},
		{
			name: "organization is empty",
			req: APIReencryptTenantIdentitySecretsRequest{
				OrganizationID: cutil.GetPtr(""),
			},
			wantErr: "organizationId must not be empty",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr != "" {
				assert.ErrorContains(t, err, tt.wantErr)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

// TestAPIReencryptTenantIdentitySecretsRequest_ToProto verifies optional organization scope and dry-run behavior map directly to the Core request.
func TestAPIReencryptTenantIdentitySecretsRequest_ToProto(t *testing.T) {
	tests := []struct {
		name           string
		req            APIReencryptTenantIdentitySecretsRequest
		wantOrg        string
		wantOrgPresent bool
		wantDryRun     bool
	}{
		{name: "organization omitted"},
		{
			name: "organization and dry-run supplied",
			req: APIReencryptTenantIdentitySecretsRequest{
				OrganizationID: cutil.GetPtr("acme-corp"),
				DryRun:         true,
			},
			wantOrg:        "acme-corp",
			wantOrgPresent: true,
			wantDryRun:     true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			protoRequest := tt.req.ToProto()
			require.NotNil(t, protoRequest)
			assert.Equal(t, tt.wantOrg, protoRequest.GetOrganizationId())
			assert.Equal(t, tt.wantOrgPresent, protoRequest.OrganizationId != nil)
			assert.Equal(t, tt.wantDryRun, protoRequest.GetDryRun())
		})
	}
}

// TestAPITenantIdentityTokenDelegationCreateOrUpdateRequest_Validate verifies required fields and clientSecretBasic sub-field validation on the token delegation create-or-update request.
func TestAPITenantIdentityTokenDelegationCreateOrUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		req     APITenantIdentityTokenDelegationCreateOrUpdateRequest
		wantErr bool
	}{
		{name: "valid with client_secret_basic", req: APITenantIdentityTokenDelegationCreateOrUpdateRequest{
			TokenEndpoint: "https://auth.acme.com/oauth2/token",
			ClientSecretBasic: &APITenantIdentityBasicClientSecretRequest{
				ClientID: "client-123", ClientSecret: "super-secret",
			},
			SubjectTokenAudience: "acme-exchange",
		}},
		{name: "valid without clientSecretBasic (auth method none)",
			req: APITenantIdentityTokenDelegationCreateOrUpdateRequest{
				TokenEndpoint:        "https://auth.acme.com/oauth2/token",
				SubjectTokenAudience: "acme-exchange",
			}},
		{name: "missing tokenEndpoint",
			req: APITenantIdentityTokenDelegationCreateOrUpdateRequest{SubjectTokenAudience: "acme-exchange"}, wantErr: true},
		{name: "missing subjectTokenAudience",
			req: APITenantIdentityTokenDelegationCreateOrUpdateRequest{TokenEndpoint: "https://auth.acme.com/oauth2/token"}, wantErr: true},
		{name: "clientSecretBasic missing clientId rejected",
			req: APITenantIdentityTokenDelegationCreateOrUpdateRequest{
				TokenEndpoint:        "https://auth.acme.com/oauth2/token",
				ClientSecretBasic:    &APITenantIdentityBasicClientSecretRequest{ClientSecret: "super-secret"},
				SubjectTokenAudience: "acme-exchange",
			},
			wantErr: true},
		{name: "clientSecretBasic missing clientSecret rejected",
			req: APITenantIdentityTokenDelegationCreateOrUpdateRequest{
				TokenEndpoint:        "https://auth.acme.com/oauth2/token",
				ClientSecretBasic:    &APITenantIdentityBasicClientSecretRequest{ClientID: "client-123"},
				SubjectTokenAudience: "acme-exchange",
			},
			wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

// TestAPITenantIdentityTokenDelegationCreateOrUpdateRequest_ToProto verifies the token delegation create-or-update request maps populated fields and the empty oneof to the SetTokenDelegation proto.
func TestAPITenantIdentityTokenDelegationCreateOrUpdateRequest_ToProto(t *testing.T) {
	t.Run("with client_secret_basic", func(t *testing.T) {
		protoReq := APITenantIdentityTokenDelegationCreateOrUpdateRequest{
			TokenEndpoint: "https://auth.acme.com/oauth2/token",
			ClientSecretBasic: &APITenantIdentityBasicClientSecretRequest{
				ClientID: "client-123", ClientSecret: "super-secret",
			},
			SubjectTokenAudience: "acme-exchange",
		}.ToProto("acme-corp")
		require.NotNil(t, protoReq)
		assert.Equal(t, "acme-corp", protoReq.GetOrganizationId())
		cfg := protoReq.GetConfig()
		require.NotNil(t, cfg)
		assert.Equal(t, "https://auth.acme.com/oauth2/token", cfg.GetTokenEndpoint())
		assert.Equal(t, "acme-exchange", cfg.GetSubjectTokenAudience())
		basic := cfg.GetClientSecretBasic()
		require.NotNil(t, basic)
		assert.Equal(t, "client-123", basic.GetClientId())
		assert.Equal(t, "super-secret", basic.GetClientSecret())
	})

	t.Run("without client_secret_basic (auth method none)", func(t *testing.T) {
		protoReq := APITenantIdentityTokenDelegationCreateOrUpdateRequest{
			TokenEndpoint:        "https://auth.acme.com/oauth2/token",
			SubjectTokenAudience: "acme-exchange",
		}.ToProto("acme-corp")
		assert.Nil(t, protoReq.GetConfig().GetAuthMethodConfig())
	})
}

// TestAPITenantIdentityTokenDelegation_FromResponseProto verifies the token delegation response mirrors the gRPC reply and never carries the raw client secret.
func TestAPITenantIdentityTokenDelegation_FromResponseProto(t *testing.T) {
	t.Run("with client_secret_basic", func(t *testing.T) {
		created := time.Date(2026, 4, 20, 12, 0, 0, 0, time.UTC)
		updated := time.Date(2026, 4, 21, 12, 0, 0, 0, time.UTC)
		resp := &APITenantIdentityTokenDelegation{}
		resp.FromResponseProto(&corev1.TokenDelegationResponse{
			OrganizationId:       "acme-corp",
			TokenEndpoint:        "https://auth.acme.com/oauth2/token",
			SubjectTokenAudience: "acme-exchange",
			AuthMethodConfig: &corev1.TokenDelegationResponse_ClientSecretBasic{
				ClientSecretBasic: &corev1.ClientSecretBasicResponse{
					ClientId: "client-123", ClientSecretHash: "sha256:abcd1234",
				},
			},
			CreatedAt: timestamppb.New(created), UpdatedAt: timestamppb.New(updated),
		})
		assert.Equal(t, "https://auth.acme.com/oauth2/token", resp.TokenEndpoint)
		assert.Equal(t, "acme-exchange", resp.SubjectTokenAudience)
		require.NotNil(t, resp.ClientSecretBasic)
		assert.Equal(t, "client-123", resp.ClientSecretBasic.ClientID)
		assert.Equal(t, "sha256:abcd1234", resp.ClientSecretBasic.ClientSecretHash)
		assert.Equal(t, created, resp.Created)
		assert.Equal(t, updated, resp.Updated)
		assert.NotContains(t, resp.ClientSecretBasic.ClientSecretHash, "super-secret",
			"raw client secret must never appear in response")
	})

	t.Run("none auth method (oneof unset)", func(t *testing.T) {
		resp := &APITenantIdentityTokenDelegation{}
		resp.FromResponseProto(&corev1.TokenDelegationResponse{
			OrganizationId: "acme-corp", TokenEndpoint: "https://auth.acme.com/oauth2/token",
			SubjectTokenAudience: "acme-exchange",
		})
		assert.Nil(t, resp.ClientSecretBasic)
	})
}

// TestAPIOpenIDConfiguration_FromResponseProto verifies the OpenID discovery response mirrors the gRPC reply, including jwks_uri and the always-empty id_token_signing_alg_values_supported.
func TestAPIOpenIDConfiguration_FromResponseProto(t *testing.T) {
	resp := &APIOpenIDConfiguration{}
	resp.FromResponseProto(&corev1.OpenIdConfiguration{
		Issuer:                           "https://carbide.example.com/iss",
		JwksUri:                          "https://carbide.example.com/iss/.well-known/jwks.json",
		ResponseTypesSupported:           []string{"token"},
		SubjectTypesSupported:            []string{"public"},
		IdTokenSigningAlgValuesSupported: []string{},
		SpiffeJwksUri:                    "https://carbide.example.com/iss/.well-known/spiffe/jwks.json",
	})
	assert.Equal(t, "https://carbide.example.com/iss", resp.Issuer)
	assert.Equal(t, "https://carbide.example.com/iss/.well-known/jwks.json", resp.JwksURI)
	assert.Equal(t, []string{"token"}, resp.ResponseTypesSupported)
	assert.Equal(t, []string{"public"}, resp.SubjectTypesSupported)
	assert.Empty(t, resp.IDTokenSigningAlgValuesSupported)
	assert.Equal(t, "https://carbide.example.com/iss/.well-known/spiffe/jwks.json", resp.SpiffeJwksURI)
}

func TestAPITenantIdentityConfig_IsCreated(t *testing.T) {
	now := time.Date(2026, 4, 20, 12, 0, 0, 0, time.UTC)
	later := now.Add(5 * time.Second)

	tests := []struct {
		name string
		resp *APITenantIdentityConfig
		want bool
	}{
		{name: "nil receiver", resp: nil, want: false},
		{name: "first create -> true", resp: &APITenantIdentityConfig{Created: now, Updated: now}, want: true},
		{name: "subsequent update -> false", resp: &APITenantIdentityConfig{Created: now, Updated: later}, want: false},
		{name: "zero Created -> false", resp: &APITenantIdentityConfig{Updated: now}, want: false},
		{name: "zero Updated -> false", resp: &APITenantIdentityConfig{Created: now}, want: false},
		{name: "both zero -> false", resp: &APITenantIdentityConfig{}, want: false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, tt.resp.IsCreated())
		})
	}
}
