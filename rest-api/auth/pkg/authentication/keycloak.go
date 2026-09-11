// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authentication

import (
	"context"
	"fmt"
	"net/http"
	"net/url"
	"strings"

	"github.com/Nerzal/gocloak/v13"
	"github.com/rs/zerolog/log"
	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
)

const (
	GrantTypeAuthorizationCode = "authorization_code"
	ClientScopes               = "openid"
	LoginResponseType          = "code"
)

// KeycloakAuthService handles Keycloak OAuth 2.0 authentication flows
type KeycloakAuthService struct {
	config *config.KeycloakConfig
	client *gocloak.GoCloak
}

// NewKeycloakAuthService creates a new Keycloak authentication service
func NewKeycloakAuthService(keycloakConfig *config.KeycloakConfig) *KeycloakAuthService {
	client := gocloak.NewClient(keycloakConfig.BaseURL)
	// Trace outbound Keycloak HTTP calls (no-op when tracing is disabled)
	client.RestyClient().SetTransport(otelhttp.NewTransport(http.DefaultTransport))
	return &KeycloakAuthService{
		config: keycloakConfig,
		client: client,
	}
}

// NewKeycloakAuthServiceWithClient creates a new Keycloak authentication service with a custom client
func NewKeycloakAuthServiceWithClient(keycloakConfig *config.KeycloakConfig, client *gocloak.GoCloak) *KeycloakAuthService {
	return &KeycloakAuthService{
		config: keycloakConfig,
		client: client,
	}
}

// getIDPAliasForDomain queries Keycloak admin API to find the appropriate IDP alias for a given email domain
func (k *KeycloakAuthService) getIDPAliasForDomain(ctx context.Context, adminToken, emailDomain string) (string, error) {
	// Get all identity providers for the realm
	idps, err := k.client.GetIdentityProviders(ctx, adminToken, k.config.Realm)
	if err != nil {
		return "", fmt.Errorf("failed to get identity providers: %w", err)
	}

	// Look for an IDP that matches the email domain
	for _, idp := range idps {
		if idp.Config != nil && idp.Alias != nil {
			config := *idp.Config
			// Check for domain mapping in IDP configuration
			if domain, exists := config["kc.org.domain"]; exists && domain == emailDomain {
				return *idp.Alias, nil
			}
		}
	}

	// Log available IDPs for debugging
	log.Error().
		Str("email_domain", emailDomain).
		Msg("No identity provider found for domain")

	return "", fmt.Errorf("no identity provider found for domain: %s", emailDomain)
}

// InitiateAuthFlow starts the OAuth 2.0 authentication flow
// It uses the realm admin credentials to query Keycloak admin API, finds the IDP alias for the domain,
// and returns the public Keycloak authorization URL with kc_idp_hint.
func (k *KeycloakAuthService) InitiateAuthFlow(ctx context.Context, email, redirectURI string) (*model.APILoginResponse, error) {
	// Extract email domain from email address
	emailParts := strings.Split(email, "@")
	if len(emailParts) != 2 {
		return nil, fmt.Errorf("invalid email format: %s", email)
	}
	emailDomain := emailParts[1]

	// Get admin token using client credentials to query IDPs
	if k.config.ClientID == "" || k.config.ClientSecret == "" {
		return nil, fmt.Errorf("client credentials not configured")
	}

	tokenOptions := gocloak.TokenOptions{
		ClientID:     &k.config.ClientID,
		ClientSecret: &k.config.ClientSecret,
		GrantType:    gocloak.StringP("client_credentials"),
	}

	token, err := k.client.GetToken(ctx, k.config.Realm, tokenOptions)
	if err != nil {
		log.Error().Err(err).Msg("Failed to get admin token using client credentials")
		return nil, fmt.Errorf("failed to get admin token using client credentials: %w", err)
	}

	adminToken := token.AccessToken

	// Get the IDP alias for the email domain
	idpAlias, err := k.getIDPAliasForDomain(ctx, adminToken, emailDomain)
	if err != nil {
		log.Error().Err(err).Str("domain", emailDomain).Msg("Failed to find IDP alias for domain")
		return nil, fmt.Errorf("failed to find identity provider for domain %s: %w", emailDomain, err)
	}

	// Construct simplified Keycloak authorization URL with kc_idp_hint
	// For confidential clients using Keycloak as broker, we only need basic OAuth parameters
	authParams := url.Values{
		"client_id":     {k.config.ClientID},
		"response_type": {LoginResponseType},
		"scope":         {ClientScopes},
		"kc_idp_hint":   {idpAlias},
		"login_hint":    {email},
	}

	// Add redirect_uri if provided
	if redirectURI != "" {
		authParams.Set("redirect_uri", redirectURI)
	}

	authURL := k.config.ExternalBaseURL + "/realms/" + k.config.Realm + "/protocol/openid-connect/auth?" + authParams.Encode()

	return &model.APILoginResponse{
		AuthURL:   authURL,
		State:     "", // No state needed for broker flow
		IDP:       idpAlias,
		RealmName: k.config.Realm,
	}, nil
}

// ExchangeCodeForTokens exchanges authorization code for access and refresh tokens
// For confidential clients, we use standard OAuth flow without PKCE
// The codeVerifier parameter is ignored for confidential clients
func (k *KeycloakAuthService) ExchangeCodeForTokens(ctx context.Context, code string, redirectURI string, codeVerifier string) (*model.APITokenResponse, error) {
	// Use gocloak GetToken for standard confidential client flow
	tokenOptions := gocloak.TokenOptions{
		ClientID:     &k.config.ClientID,
		ClientSecret: &k.config.ClientSecret,
		GrantType:    gocloak.StringP(GrantTypeAuthorizationCode),
		Code:         &code,
		RedirectURI:  &redirectURI,
	}

	tokens, err := k.client.GetToken(ctx, k.config.Realm, tokenOptions)
	if err != nil {
		return nil, fmt.Errorf("failed to exchange authorization code: %w", err)
	}

	return &model.APITokenResponse{
		AccessToken:  tokens.AccessToken,
		RefreshToken: tokens.RefreshToken,
		ExpiresIn:    tokens.ExpiresIn,
		TokenType:    tokens.TokenType,
	}, nil
}

// GetUserInfo fetches user information using the access token
func (k *KeycloakAuthService) GetUserInfo(ctx context.Context, accessToken string) (*gocloak.UserInfo, error) {
	userInfo, err := k.client.GetUserInfo(ctx, accessToken, k.config.Realm)
	if err != nil {
		return nil, fmt.Errorf("failed to get user info: %w", err)
	}
	return userInfo, nil
}

// RefreshAccessToken refreshes an access token using refresh token
func (k *KeycloakAuthService) RefreshAccessToken(ctx context.Context, refreshToken string) (*model.APITokenResponse, error) {
	tokens, err := k.client.RefreshToken(
		ctx,
		refreshToken,
		k.config.ClientID,
		k.config.ClientSecret,
		k.config.Realm,
	)
	if err != nil {
		return nil, fmt.Errorf("failed to refresh token: %w", err)
	}

	return &model.APITokenResponse{
		AccessToken:  tokens.AccessToken,
		RefreshToken: tokens.RefreshToken,
		ExpiresIn:    tokens.ExpiresIn,
		TokenType:    tokens.TokenType,
	}, nil
}

// Logout logs out a user by revoking the refresh token
func (k *KeycloakAuthService) Logout(ctx context.Context, refreshToken string) error {
	err := k.client.Logout(ctx, k.config.ClientID, k.config.ClientSecret, k.config.Realm, refreshToken)
	if err != nil {
		log.Error().Err(err).Msg("Failed to logout from Keycloak")
		// Don't return error as the client should still clear their local tokens
	}
	return nil
}

// ClientCredentialsAuth performs client credentials authentication flow
func (k *KeycloakAuthService) ClientCredentialsAuth(ctx context.Context, clientID, clientSecret string) (*model.APITokenResponse, error) {
	log.Info().
		Str("client_id", clientID).
		Msg("Starting client credentials authentication flow")

	// Use gocloak GetToken for client credentials flow
	tokenOptions := gocloak.TokenOptions{
		ClientID:     &clientID,
		ClientSecret: &clientSecret,
		GrantType:    gocloak.StringP("client_credentials"),
		Scope:        gocloak.StringP("email profile openid"),
	}

	tokens, err := k.client.GetToken(ctx, k.config.Realm, tokenOptions)
	if err != nil {
		log.Error().
			Err(err).
			Str("client_id", clientID).
			Msg("Failed to authenticate using client credentials")
		return nil, fmt.Errorf("failed to authenticate using client credentials: %w", err)
	}

	return &model.APITokenResponse{
		AccessToken:  tokens.AccessToken,
		RefreshToken: tokens.RefreshToken,
		ExpiresIn:    tokens.ExpiresIn,
		TokenType:    tokens.TokenType,
	}, nil
}

// IsServiceAccountEnabled returns whether service accounts (client credentials) are enabled
func (k *KeycloakAuthService) IsServiceAccountEnabled() bool {
	return k.config.ServiceAccountEnabled
}
