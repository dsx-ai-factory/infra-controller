// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// newMockIdentityManager builds a ManageTenantIdentity backed by a mock Forge client that satisfies all tenant identity RPCs.
func newMockIdentityManager() ManageTenantIdentity {
	mockCarbide := cClient.NewMockCoreGrpcClient()
	carbideAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	carbideAtomicClient.SwapClient(mockCarbide)
	return NewManageTenantIdentity(carbideAtomicClient)
}

// TestManageTenantIdentity_CreateOrUpdateTenantIdentityConfigurationOnSite verifies the CreateOrUpdateTenantIdentityConfigurationOnSite activity validates input, calls Core, and surfaces the echoed config and current signing key.
func TestManageTenantIdentity_CreateOrUpdateTenantIdentityConfigurationOnSite(t *testing.T) {
	identityMgr := newMockIdentityManager()
	ctx := context.Background()

	t.Run("success echoes config and publishes a current signing key", func(t *testing.T) {
		req := &corev1.SetTenantIdentityConfigRequest{
			OrganizationId: "acme-corp",
			Config: &corev1.TenantIdentityConfig{
				Enabled:         true,
				Issuer:          "https://carbide.example.com/iss",
				DefaultAudience: "openbao",
				TokenTtlSec:     600,
			},
		}
		resp, err := identityMgr.CreateOrUpdateTenantIdentityConfigurationOnSite(ctx, req)
		require.NoError(t, err)
		require.NotNil(t, resp)
		assert.Equal(t, "acme-corp", resp.GetOrganizationId())
		keys := resp.GetSigningKeys()
		require.Len(t, keys, 1)
		assert.NotEmpty(t, keys[0].GetKid())
		assert.True(t, keys[0].GetCurrentSigner())
		assert.Nil(t, keys[0].GetExpireAt())
		require.NotNil(t, resp.GetConfig())
		assert.Equal(t, "openbao", resp.GetConfig().GetDefaultAudience())
		assert.Equal(t, resp.GetCreatedAt().GetSeconds(), resp.GetUpdatedAt().GetSeconds())
	})

	t.Run("rejects nil request", func(t *testing.T) {
		_, err := identityMgr.CreateOrUpdateTenantIdentityConfigurationOnSite(ctx, nil)
		assert.Error(t, err)
	})

	t.Run("rejects missing organization_id", func(t *testing.T) {
		_, err := identityMgr.CreateOrUpdateTenantIdentityConfigurationOnSite(ctx, &corev1.SetTenantIdentityConfigRequest{
			Config: &corev1.TenantIdentityConfig{DefaultAudience: "openbao"},
		})
		assert.Error(t, err)
	})

	t.Run("rejects missing config", func(t *testing.T) {
		_, err := identityMgr.CreateOrUpdateTenantIdentityConfigurationOnSite(ctx, &corev1.SetTenantIdentityConfigRequest{
			OrganizationId: "acme-corp",
		})
		assert.Error(t, err)
	})
}

// TestManageTenantIdentity_GetTenantIdentityConfigurationFromSite verifies the GetTenantIdentityConfiguration activity validates input and returns the org's current config and signing keys.
func TestManageTenantIdentity_GetTenantIdentityConfigurationFromSite(t *testing.T) {
	identityMgr := newMockIdentityManager()
	ctx := context.Background()

	t.Run("success returns the org's current config and signing keys", func(t *testing.T) {
		resp, err := identityMgr.GetTenantIdentityConfigurationFromSite(ctx, &corev1.GetTenantIdentityConfigRequest{
			OrganizationId: "acme-corp",
		})
		require.NoError(t, err)
		require.NotNil(t, resp)
		assert.Equal(t, "acme-corp", resp.GetOrganizationId())
		keys := resp.GetSigningKeys()
		require.Len(t, keys, 1)
		assert.Equal(t, "mock-key-id", keys[0].GetKid())
		assert.True(t, keys[0].GetCurrentSigner())
	})

	t.Run("rejects nil request", func(t *testing.T) {
		_, err := identityMgr.GetTenantIdentityConfigurationFromSite(ctx, nil)
		assert.Error(t, err)
	})

	t.Run("rejects missing organization_id", func(t *testing.T) {
		_, err := identityMgr.GetTenantIdentityConfigurationFromSite(ctx, &corev1.GetTenantIdentityConfigRequest{})
		assert.Error(t, err)
	})
}

// TestManageTenantIdentity_DeleteTenantIdentityConfigurationOnSite verifies the DeleteTenantIdentityConfiguration activity validates input and returns an empty proto on success.
func TestManageTenantIdentity_DeleteTenantIdentityConfigurationOnSite(t *testing.T) {
	identityMgr := newMockIdentityManager()
	ctx := context.Background()

	t.Run("success returns empty proto", func(t *testing.T) {
		resp, err := identityMgr.DeleteTenantIdentityConfigurationOnSite(ctx, &corev1.GetTenantIdentityConfigRequest{
			OrganizationId: "acme-corp",
		})
		require.NoError(t, err)
		require.NotNil(t, resp)
	})

	t.Run("rejects nil request", func(t *testing.T) {
		_, err := identityMgr.DeleteTenantIdentityConfigurationOnSite(ctx, nil)
		assert.Error(t, err)
	})

	t.Run("rejects missing organization_id", func(t *testing.T) {
		_, err := identityMgr.DeleteTenantIdentityConfigurationOnSite(ctx, &corev1.GetTenantIdentityConfigRequest{})
		assert.Error(t, err)
	})
}

// TestManageTenantIdentity_CreateOrUpdateTenantIdentityTokenDelegationOnSite verifies the CreateOrUpdateTenantIdentityTokenDelegationOnSite activity validates input, calls Core, and never returns the raw client secret.
func TestManageTenantIdentity_CreateOrUpdateTenantIdentityTokenDelegationOnSite(t *testing.T) {
	identityMgr := newMockIdentityManager()
	ctx := context.Background()

	t.Run("success with client_secret_basic (hash returned, raw never)", func(t *testing.T) {
		req := &corev1.TokenDelegationRequest{
			OrganizationId: "acme-corp",
			Config: &corev1.TokenDelegation{
				TokenEndpoint:        "https://auth.acme.com/oauth2/token",
				SubjectTokenAudience: "acme-exchange",
				AuthMethodConfig: &corev1.TokenDelegation_ClientSecretBasic{
					ClientSecretBasic: &corev1.ClientSecretBasic{
						ClientId:     "client-123",
						ClientSecret: "super-secret",
					},
				},
			},
		}
		resp, err := identityMgr.CreateOrUpdateTenantIdentityTokenDelegationOnSite(ctx, req)
		require.NoError(t, err)
		require.NotNil(t, resp)
		assert.Equal(t, "acme-corp", resp.GetOrganizationId())
		assert.Equal(t, "https://auth.acme.com/oauth2/token", resp.GetTokenEndpoint())
		basic := resp.GetClientSecretBasic()
		require.NotNil(t, basic, "response oneof should carry hashed client_secret")
		assert.Equal(t, "client-123", basic.GetClientId())
		assert.NotEmpty(t, basic.GetClientSecretHash())
		assert.NotContains(t, basic.GetClientSecretHash(), "super-secret",
			"raw client secret must never appear in the response proto")
	})

	t.Run("success with auth method none (no client_secret_basic)", func(t *testing.T) {
		req := &corev1.TokenDelegationRequest{
			OrganizationId: "acme-corp",
			Config: &corev1.TokenDelegation{
				TokenEndpoint:        "https://auth.acme.com/oauth2/token",
				SubjectTokenAudience: "acme-exchange",
			},
		}
		resp, err := identityMgr.CreateOrUpdateTenantIdentityTokenDelegationOnSite(ctx, req)
		require.NoError(t, err)
		require.NotNil(t, resp)
		assert.Nil(t, resp.GetAuthMethodConfig(), "oneof should stay unset for auth method none")
	})

	t.Run("rejects nil request", func(t *testing.T) {
		_, err := identityMgr.CreateOrUpdateTenantIdentityTokenDelegationOnSite(ctx, nil)
		assert.Error(t, err)
	})

	t.Run("rejects missing organization_id", func(t *testing.T) {
		_, err := identityMgr.CreateOrUpdateTenantIdentityTokenDelegationOnSite(ctx, &corev1.TokenDelegationRequest{
			Config: &corev1.TokenDelegation{TokenEndpoint: "https://example.com"},
		})
		assert.Error(t, err)
	})

	t.Run("rejects missing config", func(t *testing.T) {
		_, err := identityMgr.CreateOrUpdateTenantIdentityTokenDelegationOnSite(ctx, &corev1.TokenDelegationRequest{
			OrganizationId: "acme-corp",
		})
		assert.Error(t, err)
	})
}

// TestManageTenantIdentity_GetTenantIdentityTokenDelegationFromSite verifies the GetTenantIdentityTokenDelegation activity validates input and returns the hashed (never raw) client secret.
func TestManageTenantIdentity_GetTenantIdentityTokenDelegationFromSite(t *testing.T) {
	identityMgr := newMockIdentityManager()
	ctx := context.Background()

	t.Run("success returns hashed secret, never raw", func(t *testing.T) {
		resp, err := identityMgr.GetTenantIdentityTokenDelegationFromSite(ctx, &corev1.GetTokenDelegationRequest{
			OrganizationId: "acme-corp",
		})
		require.NoError(t, err)
		require.NotNil(t, resp)
		assert.Equal(t, "acme-corp", resp.GetOrganizationId())
		basic := resp.GetClientSecretBasic()
		require.NotNil(t, basic)
		assert.NotEmpty(t, basic.GetClientSecretHash())
	})

	t.Run("rejects nil request", func(t *testing.T) {
		_, err := identityMgr.GetTenantIdentityTokenDelegationFromSite(ctx, nil)
		assert.Error(t, err)
	})

	t.Run("rejects missing organization_id", func(t *testing.T) {
		_, err := identityMgr.GetTenantIdentityTokenDelegationFromSite(ctx, &corev1.GetTokenDelegationRequest{})
		assert.Error(t, err)
	})
}

// TestManageTenantIdentity_DeleteTenantIdentityTokenDelegationOnSite verifies the DeleteTenantIdentityTokenDelegation activity validates input and returns an empty proto on success.
func TestManageTenantIdentity_DeleteTenantIdentityTokenDelegationOnSite(t *testing.T) {
	identityMgr := newMockIdentityManager()
	ctx := context.Background()

	t.Run("success returns empty proto", func(t *testing.T) {
		resp, err := identityMgr.DeleteTenantIdentityTokenDelegationOnSite(ctx, &corev1.GetTokenDelegationRequest{
			OrganizationId: "acme-corp",
		})
		require.NoError(t, err)
		require.NotNil(t, resp)
	})

	t.Run("rejects nil request", func(t *testing.T) {
		_, err := identityMgr.DeleteTenantIdentityTokenDelegationOnSite(ctx, nil)
		assert.Error(t, err)
	})

	t.Run("rejects missing organization_id", func(t *testing.T) {
		_, err := identityMgr.DeleteTenantIdentityTokenDelegationOnSite(ctx, &corev1.GetTokenDelegationRequest{})
		assert.Error(t, err)
	})
}

// TestManageTenantIdentity_GetJWKSFromSite verifies the GetJWKS activity returns use=sig for OIDC kind and use=jwt-svid for SPIFFE kind.
func TestManageTenantIdentity_GetJWKSFromSite(t *testing.T) {
	identityMgr := newMockIdentityManager()
	ctx := context.Background()

	t.Run("success oidc kind yields use=sig", func(t *testing.T) {
		kind := corev1.JwksKind_Oidc
		resp, err := identityMgr.GetJWKSFromSite(ctx, &corev1.JwksRequest{
			OrganizationId: "acme-corp",
			Kind:           &kind,
		})
		require.NoError(t, err)
		require.NotNil(t, resp)
		assert.Contains(t, resp.GetJwks(), `"use":"sig"`)
	})

	t.Run("success spiffe kind yields use=jwt-svid", func(t *testing.T) {
		kind := corev1.JwksKind_Spiffe
		resp, err := identityMgr.GetJWKSFromSite(ctx, &corev1.JwksRequest{
			OrganizationId: "acme-corp",
			Kind:           &kind,
		})
		require.NoError(t, err)
		require.NotNil(t, resp)
		assert.Contains(t, resp.GetJwks(), `"use":"jwt-svid"`)
	})

	t.Run("rejects nil request", func(t *testing.T) {
		_, err := identityMgr.GetJWKSFromSite(ctx, nil)
		assert.Error(t, err)
	})

	t.Run("rejects missing organization_id", func(t *testing.T) {
		_, err := identityMgr.GetJWKSFromSite(ctx, &corev1.JwksRequest{})
		assert.Error(t, err)
	})
}

// TestManageTenantIdentity_GetOpenIDConfigurationFromSite verifies the GetOpenIDConfiguration activity returns a well-formed discovery doc with an empty id_token_signing_alg_values_supported.
func TestManageTenantIdentity_GetOpenIDConfigurationFromSite(t *testing.T) {
	identityMgr := newMockIdentityManager()
	ctx := context.Background()

	t.Run("success returns well-formed discovery doc", func(t *testing.T) {
		resp, err := identityMgr.GetOpenIDConfigurationFromSite(ctx, &corev1.OpenIdConfigRequest{
			OrganizationId: "acme-corp",
		})
		require.NoError(t, err)
		require.NotNil(t, resp)
		assert.NotEmpty(t, resp.GetIssuer())
		assert.Contains(t, resp.GetJwksUri(), "/.well-known/jwks.json")
		assert.Contains(t, resp.GetSpiffeJwksUri(), "/.well-known/spiffe/jwks.json")
		assert.Equal(t, []string{"token"}, resp.GetResponseTypesSupported())
		assert.Empty(t, resp.GetIdTokenSigningAlgValuesSupported(),
			"Carbide does not issue OIDC id_tokens; this field must be empty")
	})

	t.Run("rejects nil request", func(t *testing.T) {
		_, err := identityMgr.GetOpenIDConfigurationFromSite(ctx, nil)
		assert.Error(t, err)
	})

	t.Run("rejects missing organization_id", func(t *testing.T) {
		_, err := identityMgr.GetOpenIDConfigurationFromSite(ctx, &corev1.OpenIdConfigRequest{})
		assert.Error(t, err)
	})
}
