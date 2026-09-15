// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authentication

import (
	"net/http"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	commonConfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"

	"github.com/golang-jwt/jwt/v5"
	"github.com/rs/zerolog/log"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/processors"
	temporalClient "go.temporal.io/sdk/client"
)

// Auth middleware reviews request parameters and validates authentication
func Auth(dbSession *cdb.Session, tc temporalClient.Client, toCfg *config.TokenOriginConfig, encCfg *commonConfig.PayloadEncryptionConfig, kcfg *config.KeycloakConfig) echo.MiddlewareFunc {
	// Initialize processors once during middleware creation
	processors.InitializeProcessors(toCfg, dbSession, tc, encCfg, kcfg)

	return func(next echo.HandlerFunc) echo.HandlerFunc {
		return func(c echo.Context) error {
			apiErr := AuthProcessor(c, toCfg)
			if apiErr != nil {
				return util.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
			}

			return next(c)
		}
	}
}

// AuthProcessor validates auth header forwarded by NGC KAS and gets or creates/updates user record
func AuthProcessor(c echo.Context, toCfg *config.TokenOriginConfig) *util.APIError {
	logger := log.With().Str("Middleware", "Auth").Logger()

	orgName := c.Param("orgName")

	// we expect the org name to be in the path and as a parameter
	if orgName == "" {
		logger.Warn().Msg("request received without org name in request path, access denied")
		return util.NewAPIError(http.StatusBadRequest, "Organization name is required in request path", nil)
	}

	logger = logger.With().Str("Org", orgName).Logger()
	logger.Info().Msgf("Starting auth processing for org: %s, path: %s", orgName, c.Path())

	// Validate NGC token in auth header
	// Extract auth header
	authTypeAndToken := c.Request().Header.Get("Authorization")
	if authTypeAndToken == "" {
		logger.Warn().Msg("request received without Authorization header, access denied")
		return util.NewAPIError(http.StatusUnauthorized, "Request is missing authorization header", nil)
	}

	// Extract token from Authorization header more robustly
	parts := strings.Fields(authTypeAndToken)
	hasBearerScheme := len(parts) > 0 && strings.EqualFold(parts[0], "Bearer")
	if len(parts) != 2 || !hasBearerScheme {
		return util.NewAPIError(http.StatusUnauthorized, "Invalid Authorization header format", nil)
	}

	tokenStr := parts[1]

	// Parse the token without validating it yet to get the issuer
	unverifiedToken, _, uErr := new(jwt.Parser).ParseUnverified(tokenStr, jwt.MapClaims{})
	if uErr != nil {
		// A bearer that is not a JWT is an NGC API key when a kas origin issuer is
		// configured.
		if processor := toCfg.GetProcessorByOrigin(config.TokenOriginKas); processor != nil {
			_, apiErr := processor.ProcessToken(c, tokenStr, toCfg.GetFirstConfigByOrigin(config.TokenOriginKas), logger)
			return apiErr
		}

		logger.Error().Err(uErr).Msg("Error parsing the token claims")
		return util.NewAPIError(http.StatusUnauthorized, "Error parsing the token claims", nil)
	}

	unverifiedClaims, ok := unverifiedToken.Claims.(jwt.MapClaims)
	if !ok {
		logger.Error().Msg("Failed to cast token claims to MapClaims")
		return util.NewAPIError(http.StatusInternalServerError, "Internal error processing token claims", nil)
	}

	// Get issuer from unverified claims
	issuer, err := unverifiedClaims.GetIssuer()
	if err != nil || issuer == "" {
		logger.Error().Err(err).Msg("No issuer found in token")
		return util.NewAPIError(http.StatusUnauthorized, "Invalid authorization token in request", nil)
	}

	// Get the appropriate processor for this issuer
	processor := toCfg.GetProcessorByIssuer(issuer)
	if processor == nil {
		logger.Error().Str("issuer", issuer).Msg("No processor found for token issuer")
		return util.NewAPIError(http.StatusUnauthorized, "Invalid authorization token in request", nil)
	}

	// Use the processor to process the token
	_, apiErr := processor.ProcessToken(c, tokenStr, toCfg.GetConfig(issuer), logger)
	if apiErr != nil {
		return apiErr
	}

	return nil
}
