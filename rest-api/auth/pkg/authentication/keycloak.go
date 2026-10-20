// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authentication

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/Nerzal/gocloak/v13"
	"github.com/golang-jwt/jwt/v5"
	"github.com/rs/zerolog/log"
	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
	"go.opentelemetry.io/otel/attribute"
	"golang.org/x/oauth2"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

const (
	ClientScopes = "openid"

	// oauthStateTTL bounds how long a login may take from /auth/login to /auth/callback.
	oauthStateTTL = 10 * time.Minute

	// oauthStateIssuer prefixes the realm in the OAuth state's iss claim, so a JWT
	// signed with the same client secret for any other purpose is not a valid state.
	oauthStateIssuer = "nico-rest oauth state/"

	// oauthStatePruneGrace keeps consumed state digests slightly past their timestamp
	// expiry so minor clock skew across replicas cannot prune a row while another
	// replica still considers the timestamp unexpired.
	oauthStatePruneGrace = 5 * time.Minute

	oauthConsumedStatePruneLimit = 128

	// oauthStateStoreTimeout bounds the prune and redeem calls together, well under
	// the API server's 60 s WriteTimeout, so a PostgreSQL outage is answered with
	// a 500 instead of a dropped connection.
	oauthStateStoreTimeout = 5 * time.Second
)

// ErrInvalidOAuthState means a callback's state was not issued by this service for
// the login whose PKCE verifier and redirect URI accompany it, has expired, or was
// already redeemed.
var ErrInvalidOAuthState = errors.New("invalid or expired OAuth state")

// KeycloakAuthService handles Keycloak OAuth 2.0 authentication flows
type KeycloakAuthService struct {
	config *config.KeycloakConfig
	client *gocloak.GoCloak
	// stateDAO records redeemed OAuth states. When nil, ConsumeState fails closed,
	// so only a service built with NewKeycloakAuthServiceWithStateDAO can complete
	// /auth/callback.
	stateDAO cdbm.OAuthConsumedStateDAO
}

func newKeycloakClient(keycloakConfig *config.KeycloakConfig) *gocloak.GoCloak {
	var baseURL string
	if keycloakConfig != nil {
		baseURL = keycloakConfig.BaseURL
	}
	client := gocloak.NewClient(baseURL)
	client.RestyClient().SetTransport(otelhttp.NewTransport(http.DefaultTransport))
	return client
}

// NewKeycloakAuthService creates a Keycloak authentication service without a
// consumed-state store; ConsumeState fails closed until one is supplied through
// NewKeycloakAuthServiceWithStateDAO.
func NewKeycloakAuthService(keycloakConfig *config.KeycloakConfig) *KeycloakAuthService {
	return &KeycloakAuthService{
		config: keycloakConfig,
		client: newKeycloakClient(keycloakConfig),
	}
}

// NewKeycloakAuthServiceWithStateDAO creates a Keycloak authentication service that
// records redeemed OAuth states through stateDAO.
func NewKeycloakAuthServiceWithStateDAO(keycloakConfig *config.KeycloakConfig, stateDAO cdbm.OAuthConsumedStateDAO) *KeycloakAuthService {
	return &KeycloakAuthService{
		config:   keycloakConfig,
		client:   newKeycloakClient(keycloakConfig),
		stateDAO: stateDAO,
	}
}

// NewKeycloakAuthServiceWithClient creates a Keycloak authentication service with a
// custom client and no consumed-state store, so ConsumeState fails closed.
func NewKeycloakAuthServiceWithClient(keycloakConfig *config.KeycloakConfig, client *gocloak.GoCloak) *KeycloakAuthService {
	return &KeycloakAuthService{
		config: keycloakConfig,
		client: client,
	}
}

func (k *KeycloakAuthService) oauth2Config(redirectURI string) *oauth2.Config {
	realm := url.PathEscape(k.config.Realm)
	return &oauth2.Config{
		ClientID:     k.config.ClientID,
		ClientSecret: k.config.ClientSecret,
		RedirectURL:  redirectURI,
		Scopes:       []string{ClientScopes},
		Endpoint: oauth2.Endpoint{
			AuthURL:  strings.TrimRight(k.config.ExternalBaseURL, "/") + "/realms/" + realm + "/protocol/openid-connect/auth",
			TokenURL: strings.TrimRight(k.config.BaseURL, "/") + "/realms/" + realm + "/protocol/openid-connect/token",
			// Form parameters (client_secret_post) are form-encoded and decoded by
			// contract, so a secret with reserved characters round-trips intact.
			// Header auth would URL-escape the credentials, which Keycloak decodes
			// only in some versions. Pinning the style also avoids the probe that
			// would send the secret twice.
			AuthStyle: oauth2.AuthStyleInParams,
		},
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
			idpCfg := *idp.Config
			// Check for domain mapping in IDP configuration
			domain, exists := idpCfg["kc.org.domain"]
			if exists && domain == emailDomain {
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
	if k.config == nil || k.config.ClientID == "" || k.config.ClientSecret == "" {
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

	// The state and the PKCE challenge bind the authorization response to this login:
	// the caller keeps the verifier in the user's session and the callback must
	// present it with the state and matching redirect URI, so a code from someone
	// else's login is refused before it is exchanged.
	verifier := oauth2.GenerateVerifier()
	state, err := k.issueState(verifier, redirectURI, time.Now())
	if err != nil {
		return nil, err
	}
	authURL := k.oauth2Config(redirectURI).AuthCodeURL(
		state,
		oauth2.S256ChallengeOption(verifier),
		oauth2.SetAuthURLParam("kc_idp_hint", idpAlias),
		oauth2.SetAuthURLParam("login_hint", email),
	)

	return &model.APILoginResponse{
		AuthURL:      authURL,
		State:        state,
		CodeVerifier: verifier,
		IDP:          idpAlias,
		RealmName:    k.config.Realm,
	}, nil
}

// ExchangeCodeForTokens exchanges an authorization code for access and refresh
// tokens, presenting the login's PKCE code verifier so Keycloak redeems only a code
// issued for that login's challenge.
func (k *KeycloakAuthService) ExchangeCodeForTokens(ctx context.Context, code string, redirectURI string, codeVerifier string) (_ *model.APITokenResponse, retErr error) {
	ctx, span := cotel.StartSpan(ctx, "KeycloakAuthService.ExchangeCodeForTokens")
	defer func() { cotel.EndSpan(span, retErr) }()

	if codeVerifier == "" {
		return nil, errors.New("PKCE code verifier is required")
	}
	if k.config == nil || k.config.ClientID == "" || k.config.ClientSecret == "" {
		return nil, errors.New("client credentials not configured")
	}

	ctx = context.WithValue(ctx, oauth2.HTTPClient, k.client.RestyClient().GetClient())
	tokens, err := k.oauth2Config(redirectURI).Exchange(ctx, code, oauth2.VerifierOption(codeVerifier))
	if err != nil {
		var retrieveErr *oauth2.RetrieveError
		if errors.As(err, &retrieveErr) && retrieveErr.Response != nil {
			return nil, fmt.Errorf("failed to exchange authorization code: HTTP %d", retrieveErr.Response.StatusCode)
		}
		return nil, fmt.Errorf("failed to exchange authorization code: %w", err)
	}

	return &model.APITokenResponse{
		AccessToken:  tokens.AccessToken,
		RefreshToken: tokens.RefreshToken,
		ExpiresIn:    int(tokens.ExpiresIn),
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

// oauthStateClaims binds an OAuth state to one login: its PKCE challenge and
// redirect URI, with the realm in the issuer and the client ID as audience.
type oauthStateClaims struct {
	jwt.RegisteredClaims
	CodeChallenge string `json:"code_challenge"`
	RedirectURI   string `json:"redirect_uri"`
}

// issueState returns the OAuth state for a login whose PKCE verifier is verifier
// and redirect target is redirectURI: an HS256 JWT keyed by the client secret,
// which every replica shares and no caller sees. Any replica can verify it without
// reading a session store, and redemption records its digest in PostgreSQL so
// each state can be used at most once.
func (k *KeycloakAuthService) issueState(verifier, redirectURI string, now time.Time) (string, error) {
	state, err := jwt.NewWithClaims(jwt.SigningMethodHS256, oauthStateClaims{
		RegisteredClaims: jwt.RegisteredClaims{
			Issuer:    oauthStateIssuer + k.config.Realm,
			Audience:  jwt.ClaimStrings{k.config.ClientID},
			ExpiresAt: jwt.NewNumericDate(now.Add(oauthStateTTL)),
			ID:        rand.Text(),
		},
		CodeChallenge: oauth2.S256ChallengeFromVerifier(verifier),
		RedirectURI:   redirectURI,
	}).SignedString([]byte(k.config.ClientSecret))
	if err != nil {
		return "", fmt.Errorf("failed to sign OAuth state: %w", err)
	}
	// The callback rejects a state over this bound before verifying it, so a
	// redirect URI long enough to exceed it must fail here, not at the callback.
	if len(state) > model.MaxOAuthStateLength {
		return "", errors.New("redirect URI is too long for the OAuth state")
	}
	return state, nil
}

// ConsumeState verifies that state was issued by this service for the login whose
// PKCE verifier and redirect URI accompany it, has not expired, and has not been
// redeemed before, then atomically marks it consumed. It returns ErrInvalidOAuthState
// when validation or replay checks fail, and another error when the backing state
// store cannot be reached.
func (k *KeycloakAuthService) ConsumeState(ctx context.Context, state, verifier, redirectURI string) (retErr error) {
	ctx, span := cotel.StartSpan(ctx, "KeycloakAuthService.ConsumeState")
	defer func() {
		// A rejected state is the caller's failure: record the bounded reason
		// without marking the span failed, as cotel.RecordHTTPError does for 4xx.
		if errors.Is(retErr, ErrInvalidOAuthState) {
			cotel.SetAttribute(span, attribute.String("oauth.state.rejection", retErr.Error()))
			span.End()
			return
		}
		cotel.EndSpan(span, retErr)
	}()
	return k.consumeStateAt(ctx, state, verifier, redirectURI, time.Now())
}

func (k *KeycloakAuthService) consumeStateAt(ctx context.Context, state, verifier, redirectURI string, now time.Time) error {
	// Server-side faults are reported as such, not as ErrInvalidOAuthState, so the
	// handler answers 500 instead of blaming the caller.
	if k.config == nil || k.config.ClientSecret == "" {
		return errors.New("client credentials not configured")
	}
	if k.stateDAO == nil {
		return errors.New("OAuth consumed state DAO is not initialized")
	}
	expiresAt, err := k.checkState(state, verifier, redirectURI, now)
	if err != nil {
		return err
	}

	ctx, cancel := context.WithTimeout(ctx, oauthStateStoreTimeout)
	defer cancel()

	pruneBefore := now.Add(-oauthStatePruneGrace)
	perr := k.stateDAO.DeleteExpired(ctx, nil, pruneBefore, oauthConsumedStatePruneLimit)
	if perr != nil {
		logger := cotel.LoggerWithTrace(ctx, log.Logger)
		logger.Warn().Err(perr).Msg("failed to delete expired OAuth consumed states")
	}

	digest := sha256.Sum256([]byte(state))
	_, created, err := k.stateDAO.Create(ctx, nil, cdbm.OAuthConsumedStateCreateInput{
		StateDigest: digest[:],
		ExpiresAt:   expiresAt,
	})
	if err != nil {
		return fmt.Errorf("failed to consume OAuth state: %w", err)
	}
	if !created {
		return fmt.Errorf("%w: already redeemed", ErrInvalidOAuthState)
	}
	return nil
}

// checkState wraps ErrInvalidOAuthState with a bounded reason so logs can tell a
// slow login, a mismatched login and a replay apart; the HTTP response stays generic.
func (k *KeycloakAuthService) checkState(state, verifier, redirectURI string, now time.Time) (time.Time, error) {
	if len(state) == 0 || len(state) > model.MaxOAuthStateLength || verifier == "" {
		return time.Time{}, fmt.Errorf("%w: malformed", ErrInvalidOAuthState)
	}
	var claims oauthStateClaims
	_, err := jwt.ParseWithClaims(state, &claims,
		func(*jwt.Token) (any, error) { return []byte(k.config.ClientSecret), nil },
		jwt.WithValidMethods([]string{jwt.SigningMethodHS256.Alg()}),
		jwt.WithExpirationRequired(),
		jwt.WithIssuer(oauthStateIssuer+k.config.Realm),
		jwt.WithAudience(k.config.ClientID),
		jwt.WithTimeFunc(func() time.Time { return now }),
	)
	switch {
	case errors.Is(err, jwt.ErrTokenMalformed):
		return time.Time{}, fmt.Errorf("%w: malformed", ErrInvalidOAuthState)
	case errors.Is(err, jwt.ErrTokenExpired):
		return time.Time{}, fmt.Errorf("%w: expired", ErrInvalidOAuthState)
	case errors.Is(err, jwt.ErrTokenInvalidIssuer), errors.Is(err, jwt.ErrTokenInvalidAudience):
		return time.Time{}, fmt.Errorf("%w: binding mismatch", ErrInvalidOAuthState)
	case err != nil:
		// Bad signature, disallowed algorithm, missing exp, and any other failure.
		return time.Time{}, fmt.Errorf("%w: signature mismatch", ErrInvalidOAuthState)
	}
	if claims.ID == "" {
		return time.Time{}, fmt.Errorf("%w: malformed", ErrInvalidOAuthState)
	}
	if claims.CodeChallenge != oauth2.S256ChallengeFromVerifier(verifier) || claims.RedirectURI != redirectURI {
		return time.Time{}, fmt.Errorf("%w: binding mismatch", ErrInvalidOAuthState)
	}
	return claims.ExpiresAt.Time, nil
}
