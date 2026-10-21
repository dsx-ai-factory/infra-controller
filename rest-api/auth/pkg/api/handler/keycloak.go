// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
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
// @Description Authenticate using either email-based OAuth2 flow (returns AuthFlowResponse) or client credentials flow (returns TokenResponse)
// @Tags auth
// @Accept json
// @Produce json
// @Param message body cam.APILoginRequest true "Login request with either email+redirectURI or clientId+clientSecret"
// @Success 200 {object} cam.APILoginResponse "OAuth2 flow initiated successfully (for email-based auth)"
// @Success 200 {object} cam.APITokenResponse "Client credentials authentication successful"
// @Router /auth/login [post]
func (lh LoginHandler) Handle(c echo.Context) error {
	// Get context
	ctx := c.Request().Context()

	// Initialize logger
	logger := log.With().Str("Model", "Auth").Str("Handler", "Login").Logger()

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

		// Check if service accounts are enabled
		if !lh.keycloakAuth.IsServiceAccountEnabled() {
			logger.Error().Str("client_id", *req.ClientID).Msg("Client credentials requested but service accounts are not enabled")
			return ccu.NewAPIErrorResponse(c, http.StatusUnauthorized, "Service accounts are not enabled", nil)
		}

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
// @Description Exchange authorization code for tokens
// @Tags auth
// @Accept json
// @Produce json
// @Param message body cam.APICallbackRequest true "Callback request with code and code_verifier from BFF"
// @Success 200 {object} cam.APITokenResponse "Token exchange successful"
// @Router /auth/callback [post]
func (ch CallbackHandler) Handle(c echo.Context) error {
	// Get context
	ctx := c.Request().Context()

	// Initialize logger
	logger := log.With().Str("Model", "Auth").Str("Handler", "Callback").Logger()

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

	// Exchange code for tokens
	tokens, err := ch.keycloakAuth.ExchangeCodeForTokens(ctx, req.Code, req.RedirectURI, "")
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
	logger := log.With().Str("Model", "Auth").Str("Handler", "Logout").Logger()

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
	logger := log.With().Str("Model", "Auth").Str("Handler", "RefreshToken").Logger()

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
