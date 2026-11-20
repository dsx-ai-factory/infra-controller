// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"errors"
	"net/http"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog/log"

	cam "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/api/model"
	caa "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authentication"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	ccu "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// LoginHandler is the API Handler for user authentication with OAuth2 flow
type LoginHandler struct {
	keycloakAuth *caa.KeycloakAuthService
}

// NewLoginHandler initializes and returns a new handler for user login
func NewLoginHandler(keycloakAuth *caa.KeycloakAuthService) LoginHandler {
	return LoginHandler{
		keycloakAuth: keycloakAuth,
	}
}

// Handle godoc
// @Summary Authenticate user with OAuth2 flow or client credentials
// @Description Authenticate using either the email-based OAuth2 flow (returns APILoginResponse) or the client credentials flow (returns APITokenResponse). The email flow returns authURL, which carries a single-use state and an S256 PKCE challenge, plus the state and the matching codeVerifier. The BFF must keep codeVerifier in the user's server-side session, never send it to the browser, and present it with code, redirectUri and state to /auth/callback.
// @Tags auth
// @Accept json
// @Produce json
// @Param message body cam.APILoginRequest true "Login request with either email+redirectURI or clientId+clientSecret"
// @Success 200 {object} cam.APILoginResponse "OAuth2 flow initiated successfully (for email-based auth); keep codeVerifier server-side"
// @Success 200 {object} cam.APITokenResponse "Client credentials authentication successful"
// @Failure 400 {object} ccu.APIError "Invalid request data"
// @Failure 401 {object} ccu.APIError "Failed to authenticate using client credentials"
// @Failure 500 {object} ccu.APIError "Failed to initiate authentication"
// @Router /auth/login [post]
func (lh LoginHandler) Handle(c echo.Context) error {
	// Get context
	ctx := c.Request().Context()

	// Initialize logger
	logger := cotel.LoggerWithTrace(ctx, log.With().Str("Model", "Auth").Str("Handler", "Login").Logger())

	logger.Info().Msg("started API handler")

	// Create a child span and set the attributes for current request
	ctx, handlerSpan := cotel.StartSpan(ctx, "LoginHandler")
	defer handlerSpan.End()

	var req cam.APILoginRequest
	// Bind request data into API model
	err := c.Bind(&req)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return ccu.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request data
	verr := req.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating request data")
		return ccu.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate request data", verr)
	}

	// Check for client credentials flow
	if req.IsClientCredentials() {
		logger.Info().Str("client_id", *req.ClientID).Msg("Processing client credentials authentication")

		tokens, err := lh.keycloakAuth.ClientCredentialsAuth(ctx, *req.ClientID, *req.ClientSecret)
		if err != nil {
			logger.Error().Err(err).Msg("failed client credentials authentication")
			return ccu.NewAPIErrorResponse(c, http.StatusUnauthorized, "Failed to authenticate using client credentials", nil)
		}

		logger.Info().Msg("client credentials authentication successful")
		return c.JSON(http.StatusOK, tokens)
	}

	// Check for email-based authentication flow
	logger.Info().Str("email", *req.Email).Msg("Processing email-based authentication flow")

	authFlow, err := lh.keycloakAuth.InitiateAuthFlow(ctx, *req.Email, *req.RedirectURI)
	if err != nil {
		logger.Error().Err(err).Msg("failed to initiate auth flow")
		return ccu.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to initiate authentication", nil)
	}

	logger.Info().Msg("login initiated successfully")

	return c.JSON(http.StatusOK, authFlow)
}

// CallbackHandler is the API Handler for OAuth2 callback handling
type CallbackHandler struct {
	keycloakAuth *caa.KeycloakAuthService
}

// NewCallbackHandler initializes and returns a new handler for OAuth2 callback
func NewCallbackHandler(keycloakAuth *caa.KeycloakAuthService) CallbackHandler {
	return CallbackHandler{
		keycloakAuth: keycloakAuth,
	}
}

// Handle godoc
// @Summary Exchange authorization code for tokens
// @Description Verify and redeem single-use OAuth state against the login's PKCE codeVerifier and redirectUri, then exchange authorization code for tokens
// @Tags auth
// @Accept json
// @Produce json
// @Param message body cam.APICallbackRequest true "Callback request with code, redirectUri, state and the login's codeVerifier from BFF"
// @Success 200 {object} cam.APITokenResponse "Token exchange successful"
// @Failure 400 {object} ccu.APIError "Invalid request data or invalid, expired or already-redeemed state"
// @Failure 401 {object} ccu.APIError "Failed to exchange authorization code for tokens"
// @Failure 500 {object} ccu.APIError "Failed to verify authentication state"
// @Router /auth/callback [post]
func (ch CallbackHandler) Handle(c echo.Context) error {
	// Get context
	ctx := c.Request().Context()

	// Initialize logger
	logger := cotel.LoggerWithTrace(ctx, log.With().Str("Model", "Auth").Str("Handler", "Callback").Logger())

	// Create a child span and set the attributes for current request
	ctx, handlerSpan := cotel.StartSpan(ctx, "CallbackHandler")
	defer handlerSpan.End()

	// Bind request data into API model
	var req cam.APICallbackRequest
	err := c.Bind(&req)
	if err != nil {
		logger.Error().Err(err).Msg("failed to bind request data into API model")
		return ccu.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request data
	verr := req.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating request data")
		return ccu.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate request data", verr)
	}

	// The state must be an unredeemed one this service issued for the login whose
	// verifier and redirect URI the caller presents. Checked and consumed before
	// the code is exchanged, so a replayed state or an authorization code from
	// someone else's login (login CSRF) is never redeemed.
	serr := ch.keycloakAuth.ConsumeState(ctx, req.State, req.CodeVerifier, req.RedirectURI)
	if serr != nil {
		if errors.Is(serr, caa.ErrInvalidOAuthState) {
			logger.Warn().Err(serr).Msg("rejected callback state")
			return ccu.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid or expired state", nil)
		}
		logger.Error().Err(serr).Msg("failed to consume callback state")
		return ccu.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to verify authentication state", nil)
	}

	// Exchange code for tokens
	tokens, err := ch.keycloakAuth.ExchangeCodeForTokens(ctx, req.Code, req.RedirectURI, req.CodeVerifier)
	if err != nil {
		logger.Error().Err(err).Msg("failed to exchange code for tokens")
		return ccu.NewAPIErrorResponse(c, http.StatusUnauthorized, "Failed to exchange authorization code for tokens", nil)
	}

	logger.Info().Msg("token exchange successful")

	return c.JSON(http.StatusOK, tokens)
}

// LogoutHandler is the API Handler for user logout
type LogoutHandler struct {
	keycloakAuth *caa.KeycloakAuthService
}

// NewLogoutHandler initializes and returns a new handler for user logout
func NewLogoutHandler(keycloakAuth *caa.KeycloakAuthService) LogoutHandler {
	return LogoutHandler{
		keycloakAuth: keycloakAuth,
	}
}

// Handle godoc
// @Summary User logout
// @Description Revoke refresh tokens and end Keycloak session
// @Tags auth
// @Accept json
// @Produce json
// @Param message body cam.APILogoutRequest true "Logout request"
// @Success 200 {object} string
// @Router /auth/logout [post]
func (lh LogoutHandler) Handle(c echo.Context) error {
	// Get context
	ctx := c.Request().Context()

	// Initialize logger
	logger := cotel.LoggerWithTrace(ctx, log.With().Str("Model", "Auth").Str("Handler", "Logout").Logger())

	logger.Info().Msg("started API handler")

	// Create a child span and set the attributes for current request
	ctx, handlerSpan := cotel.StartSpan(ctx, "LogoutHandler")
	defer handlerSpan.End()

	var req cam.APILogoutRequest

	// Bind request data into API model
	err := c.Bind(&req)
	if err != nil {
		logger.Error().Err(err).Msg("failed to bind request data into API model")
		return ccu.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request data
	verr := req.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating request data")
		return ccu.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate request data", verr)
	}

	// Logout from Keycloak
	err = lh.keycloakAuth.Logout(ctx, req.RefreshToken)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to logout from Keycloak, proceeding with client cleanup")
	}

	logger.Info().Msg("logout successful")

	return c.JSON(http.StatusOK, "Logout successful")
}

// RefreshTokenHandler is the API Handler for refreshing access tokens
type RefreshTokenHandler struct {
	keycloakAuth *caa.KeycloakAuthService
}

// NewRefreshTokenHandler initializes and returns a new handler for token refresh
func NewRefreshTokenHandler(keycloakAuth *caa.KeycloakAuthService) RefreshTokenHandler {
	return RefreshTokenHandler{
		keycloakAuth: keycloakAuth,
	}
}

// Handle godoc
// @Summary Refresh access token
// @Description Refresh an access token using a refresh token
// @Tags auth
// @Accept json
// @Produce json
// @Param message body cam.APIRefreshTokenRequest true "Refresh token request"
// @Success 200 {object} cam.APITokenResponse
// @Router /auth/refresh [post]
func (rth RefreshTokenHandler) Handle(c echo.Context) error {
	// Get context
	ctx := c.Request().Context()

	// Initialize logger
	logger := cotel.LoggerWithTrace(ctx, log.With().Str("Model", "Auth").Str("Handler", "RefreshToken").Logger())

	logger.Info().Msg("started API handler")

	// Create a child span and set the attributes for current request
	ctx, handlerSpan := cotel.StartSpan(ctx, "RefreshTokenHandler")
	c.SetRequest(c.Request().WithContext(ctx))
	defer handlerSpan.End()

	var req cam.APIRefreshTokenRequest

	// Bind request data into API model
	err := c.Bind(&req)
	if err != nil {
		logger.Error().Err(err).Msg("failed to bind request")
		return ccu.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid request format", nil)
	}

	// Validate request data
	verr := req.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating request data")
		return ccu.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate request data", verr)
	}

	logger.Info().Msg("refreshing access token")

	// Refresh access token
	tokens, err := rth.keycloakAuth.RefreshAccessToken(ctx, req.RefreshToken)
	if err != nil {
		logger.Error().Err(err).Msg("failed to refresh token")
		return ccu.NewAPIErrorResponse(c, http.StatusUnauthorized, "Invalid or expired refresh token", nil)
	}

	logger.Info().Msg("token refreshed successfully")

	return c.JSON(http.StatusOK, tokens)
}
