// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"slices"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/rs/zerolog"
)

const (
	// authIssuerAPIUnavailableMessage is returned when Keycloak is enabled.
	authIssuerAPIUnavailableMessage = "Auth Issuer management via the API is unavailable while Keycloak is enabled."
	// authIssuerOrgNotAnchoredMessage is returned when the path org holds
	// PROVIDER_ADMIN only through something the API itself can grant.
	authIssuerOrgNotAnchoredMessage = "Auth Issuer management requires an org that a ConfigMap issuer grants Provider Admin"
	authIssuerOrderByDefault        = "CREATED_AT_ASC"
)

// authorizeAuthIssuerAdmin requires PROVIDER_ADMIN in org, and a ConfigMap issuer
// that grants org PROVIDER_ADMIN.
func authorizeAuthIssuerAdmin(cfg *config.Config, dbUser *cdbm.User, org string, logger zerolog.Logger) *cutil.APIError {
	if dbUser == nil {
		return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	if !cfg.GetEnvDisconnected() {
		logger.Warn().Msg("Auth Issuer API is only supported in disconnected mode")
		return cutil.NewAPIError(http.StatusBadRequest, "Auth Issuer management via the API is only supported in disconnected mode", nil)
	}
	if cfg.GetKeycloakEnabled() {
		logger.Warn().Msg("Auth Issuer API is unavailable, static configuration owns issuer trust")
		return cutil.NewAPIError(http.StatusBadRequest, authIssuerAPIUnavailableMessage, nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIError(http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIError(http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	if !cfg.StaticIssuers().GrantsProviderAdmin(org) {
		logger.Warn().Msg("org is not granted Provider Admin by a ConfigMap issuer, cannot manage Auth Issuers")
		return cutil.NewAPIError(http.StatusForbidden, authIssuerOrgNotAnchoredMessage, nil)
	}
	return nil
}

func authIssuerAPIErrorResponse(c echo.Context, apiErr *cutil.APIError) error {
	return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
}

// getStatusDetailsByEntityID loads the recent status_detail rows for the given
// entities, keyed by entity ID.
func getStatusDetailsByEntityID(ctx context.Context, dbSession *cdb.Session, ids []string) (map[string][]cdbm.StatusDetail, error) {
	rows, err := cdbm.NewStatusDetailDAO(dbSession).GetRecentByEntityIDs(ctx, nil, ids, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		return nil, err
	}
	byID := make(map[string][]cdbm.StatusDetail, len(ids))
	for _, row := range rows {
		byID[row.EntityID] = append(byID[row.EntityID], row)
	}
	return byID, nil
}

// ~~~~~ Create-or-update Handler ~~~~~ //

// CreateOrUpdateAuthIssuerHandler is the API Handler for creating or replacing an
// Auth Issuer
type CreateOrUpdateAuthIssuerHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewCreateOrUpdateAuthIssuerHandler initializes an Auth Issuer upsert handler.
func NewCreateOrUpdateAuthIssuerHandler(dbSession *cdb.Session, cfg *config.Config) CreateOrUpdateAuthIssuerHandler {
	return CreateOrUpdateAuthIssuerHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

type authIssuerUpsertResult struct {
	issuer  *cdbm.Issuer
	created bool
}

// Handle godoc
// @Summary Create or replace an external JWT issuer
// @Description Create or fully replace a runtime-managed external JWT issuer selected by issuerUrl (Provider Admin only)
// @Tags auth-issuer
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param authIssuer body model.APIAuthIssuerCreateOrUpdateRequest true "Complete Auth Issuer configuration"
// @Success 200 {object} model.APIAuthIssuer
// @Success 201 {object} model.APIAuthIssuer
// @Router /v2/org/{org}/nico/auth-issuer [put]
func (cih CreateOrUpdateAuthIssuerHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AuthIssuer", "CreateOrUpdate", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if apiErr := authorizeAuthIssuerAdmin(cih.cfg, dbUser, org, logger); apiErr != nil {
		return authIssuerAPIErrorResponse(c, apiErr)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIAuthIssuerCreateOrUpdateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Auth Issuer create-or-update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Auth Issuer create-or-update request data", verr)
	}

	// A custom Auth Issuer may not join a trust plane owned by a privileged IdP.
	if cih.cfg.HasPrivilegedConfigMapIssuer() {
		logger.Warn().Msg("static configuration defines privileged issuer origins, cannot manage custom Auth Issuer")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Cannot manage a custom Auth Issuer while keycloak, kas-legacy, or kas-ssa issuers are defined in the static configuration. "+
				"Remove those issuers from the ConfigMap issuers block first.", nil)
	}

	issuerDAO := cdbm.NewIssuerDAO(cih.dbSession)
	userID := dbUser.ID.String()

	result, err := cdb.WithTxResult(ctx, cih.dbSession, func(tx *cdb.Tx) (*authIssuerUpsertResult, error) {
		// Acquire an advisory lock on the Auth Issuer organization mapping on which there could be contention.
		// this lock is released when the transaction commits or rollsback
		derr := tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(config.IssuerOrgMappingLockKey), true)
		if derr != nil {
			logger.Error().Err(derr).Msg("Failed to acquire advisory lock on Auth Issuer organization mapping")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to create or update Auth Issuer, could not acquire data store lock on Auth Issuers", nil)
		}

		existing, derr := issuerDAO.GetByIssuerURL(ctx, tx, apiRequest.IssuerURL)
		if derr != nil && !errors.Is(derr, cdb.ErrDoesNotExist) {
			logger.Error().Err(derr).Msg("error retrieving Auth Issuer before create or update")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Auth Issuer, DB error", nil)
		}
		if errors.Is(derr, cdb.ErrDoesNotExist) {
			existing = nil
		}

		// A ConfigMap issuer needs a config row, and a config row a ConfigMap entry.
		_, static := cih.cfg.StaticIssuers().Issuer(apiRequest.IssuerURL)
		if static != (existing != nil && existing.Source == cdbm.IssuerSourceConfig) {
			logger.Error().Str("issuer", apiRequest.IssuerURL).Bool("configmap", static).Msg("ConfigMap and issuer table disagree about this issuer, refusing")
			return nil, cutil.NewAPIError(http.StatusConflict, "Auth Issuer is defined by the ConfigMap but its record is missing or stale; restart the API to restore it", nil)
		}

		upsertInput := apiRequest.ToCreateOrUpdateInput(&dbUser.ID)
		if static {
			// The row defines every field except the api claim mappings.
			upsertInput.Source = cdbm.IssuerSourceConfig
			upsertInput.ConfigRevision = existing.ConfigRevision
			if apiRequest.JWKSUrl == "" {
				upsertInput.JWKSUrl = existing.JWKSUrl
			}
			if upsertInput.JWKSTimeout == "" {
				upsertInput.JWKSTimeout = existing.JWKSTimeout
			}
			if len(upsertInput.Audiences) == 0 {
				upsertInput.Audiences = existing.Audiences
			}
			if len(upsertInput.Scopes) == 0 {
				upsertInput.Scopes = existing.Scopes
			}
			upsertInput.ServiceAccount = existing.ServiceAccount
			requested := upsertInput.ToIssuer()
			field := ""
			switch {
			case requested.JWKSUrl != existing.JWKSUrl:
				field = "jwksUrl"
			case requested.JWKSTimeout != existing.JWKSTimeout:
				field = "jwksTimeout"
			case !slices.Equal(requested.Audiences, existing.Audiences):
				field = "audiences"
			case !slices.Equal(requested.Scopes, existing.Scopes):
				field = "scopes"
			}
			if field != "" {
				logger.Warn().Str("issuer", apiRequest.IssuerURL).Str("field", field).Msg("request cannot change a ConfigMap issuer")
				return nil, cutil.NewAPIError(http.StatusConflict, fmt.Sprintf("%s is defined by the ConfigMap issuer and cannot be changed", field), nil)
			}
			var merged []cdbm.ClaimMapping
			configKeys := map[string]bool{}
			for _, cm := range existing.ClaimMappings {
				if cm.Source == cdbm.IssuerSourceConfig {
					merged = append(merged, cm)
					configKeys[config.ClaimMappingKey(cm)] = true
				}
			}
			for _, cm := range upsertInput.ClaimMappings {
				if !configKeys[config.ClaimMappingKey(cm)] {
					merged = append(merged, cm)
				}
			}
			upsertInput.ClaimMappings = merged
		}

		// Checked after the ConfigMap's own mappings are set aside.
		candidate := upsertInput.ToIssuer()
		merr := config.ValidateAPIClaimMappings(candidate.ClaimMappings)
		if merr != nil {
			logger.Warn().Err(merr).Msg("Auth Issuer claim mappings break the API's rules")
			return nil, cutil.NewAPIError(http.StatusBadRequest, "Invalid claim mappings: "+merr.Error()+
				"; the API writes static orgName mappings granting Tenant roles, everything else comes from the ConfigMap", nil)
		}

		// Validate against every other issuer, ConfigMap rows included.
		var excludeID *uuid.UUID
		if existing != nil {
			candidate.ID = existing.ID
			excludeID = &existing.ID
		}
		derr = cih.cfg.ValidateCombinedIssuers(ctx, cih.dbSession, tx, &candidate, excludeID)
		if derr != nil {
			if errors.Is(derr, config.ErrIssuerStoreUnavailable) {
				logger.Error().Err(derr).Msg("Could not validate Auth Issuer against the combined issuer set")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to validate Auth Issuer, DB error", nil)
			}
			logger.Warn().Err(derr).Msg("Auth Issuer is not valid against the combined issuer set")
			// A claimed identity field is a conflict, not a malformed request.
			if errors.Is(derr, config.ErrIssuerIdentityConflict) {
				return nil, cutil.NewAPIError(http.StatusConflict, fmt.Sprintf("Issuer conflicts with an existing issuer: %s", derr), nil)
			}
			return nil, cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Invalid issuer configuration: %s", derr), nil)
		}

		if existing == nil {
			createdIssuer, createErr := issuerDAO.Create(ctx, tx, upsertInput)
			if createErr != nil {
				logger.Error().Err(createErr).Msg("error creating Auth Issuer record in DB")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Auth Issuer", nil)
			}
			if herr := config.RecordIssuerHistory(ctx, cih.dbSession, tx, createdIssuer.ID, createdIssuer.Status,
				config.IssuerChangeSummary("PUT by user "+userID, cdbm.Issuer{Source: createdIssuer.Source}, *createdIssuer)); herr != nil {
				logger.Error().Err(herr).Msg("error recording Auth Issuer status history")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Auth Issuer", nil)
			}
			return &authIssuerUpsertResult{issuer: createdIssuer, created: true}, nil
		}

		// An identical PUT is a true no-op: keep timestamps and cached signing keys.
		if existing.Signature() == candidate.Signature() {
			return &authIssuerUpsertResult{issuer: existing}, nil
		}

		jwksChanged := existing.JWKSUrl != candidate.JWKSUrl
		updatedIssuer, derr := issuerDAO.Update(ctx, tx, existing.ID, upsertInput, jwksChanged)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating Auth Issuer record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Auth Issuer", nil)
		}
		// A new key source has no keys yet.
		if jwksChanged {
			if _, serr := issuerDAO.UpdateStatus(ctx, tx, existing.ID, cdbm.IssuerStatusPending, nil); serr != nil {
				logger.Error().Err(serr).Msg("error resetting Auth Issuer status")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Auth Issuer", nil)
			}
			updatedIssuer.Status, updatedIssuer.StatusMessage = cdbm.IssuerStatusPending, nil
		}
		if herr := config.RecordIssuerHistory(ctx, cih.dbSession, tx, existing.ID, updatedIssuer.Status,
			config.IssuerChangeSummary("PUT by user "+userID, *existing, candidate)); herr != nil {
			logger.Error().Err(herr).Msg("error recording Auth Issuer status history")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Auth Issuer", nil)
		}
		return &authIssuerUpsertResult{issuer: updatedIssuer}, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create or update Auth Issuer, DB transaction error")
	}

	// Apply to the live registry on this replica (read-your-write). Other replicas
	// converge on their next periodic reload.
	cih.cfg.PublishIssuerMutation(ctx, cih.dbSession, result.issuer, false)

	history, err := getStatusDetailsByEntityID(ctx, cih.dbSession, []string{result.issuer.ID.String()})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Auth Issuer status history")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Auth Issuer status history, DB error", nil)
	}

	status := http.StatusOK
	if result.created {
		status = http.StatusCreated
	}
	orgs, roles := config.GrantedOrgsAndRoles(result.issuer.ClaimMappings)
	logger.Info().Str("issuer", result.issuer.IssuerURL).Bool("created", result.created).
		Str("source", result.issuer.Source).Strs("orgs", orgs).Strs("roles", roles).
		Msg("finishing API handler")
	return c.JSON(status, model.NewAPIAuthIssuer(result.issuer, history[result.issuer.ID.String()]))
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllAuthIssuerHandler is the API Handler for listing the provider's Auth Issuers.
type GetAllAuthIssuerHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetAllAuthIssuerHandler initializes a handler for listing Auth Issuers.
func NewGetAllAuthIssuerHandler(dbSession *cdb.Session, cfg *config.Config) GetAllAuthIssuerHandler {
	return GetAllAuthIssuerHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary List external JWT issuers
// @Description List the provider's runtime-managed external JWT issuers (Provider Admin only)
// @Tags auth-issuer
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Success 200 {object} []model.APIAuthIssuer
// @Router /v2/org/{org}/nico/auth-issuer [get]
func (gaih GetAllAuthIssuerHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AuthIssuer", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if apiErr := authorizeAuthIssuerAdmin(gaih.cfg, dbUser, org, logger); apiErr != nil {
		return authIssuerAPIErrorResponse(c, apiErr)
	}

	pageRequest := pagination.PageRequest{}
	if err := c.Bind(&pageRequest); err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	if err := pageRequest.Validate(cdbm.IssuerOrderByFields); err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}
	if pageRequest.OrderByStr == nil {
		pageRequest.OrderByStr = cutil.GetPtr(authIssuerOrderByDefault)
	}

	filter := cdbm.IssuerFilterInput{}
	if status := c.QueryParam("status"); status != "" {
		switch model.APIAuthIssuerStatus(status) {
		case model.AuthIssuerStatusPending, model.AuthIssuerStatusReady, model.AuthIssuerStatusDegraded:
			filter.Status = &status
		default:
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid status filter, must be Pending, Ready, or Degraded", nil)
		}
	}

	issuerDAO := cdbm.NewIssuerDAO(gaih.dbSession)
	issuers, total, err := issuerDAO.GetPage(ctx, nil, filter, pageRequest.ConvertToDB())
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Auth Issuers from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Auth Issuers, DB error", nil)
	}

	ids := make([]string, len(issuers))
	for i := range issuers {
		ids[i] = issuers[i].ID.String()
	}
	history, err := getStatusDetailsByEntityID(ctx, gaih.dbSession, ids)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Auth Issuer status history")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Auth Issuer status history, DB error", nil)
	}

	// Create response
	apiIssuers := []*model.APIAuthIssuer{}
	for i := range issuers {
		apiIssuers = append(apiIssuers, model.NewAPIAuthIssuer(&issuers[i], history[ids[i]]))
	}

	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiIssuers)
}

// ~~~~~ Get Handler ~~~~~ //

// GetAuthIssuerHandler is the API Handler for retrieving an Auth Issuer.
type GetAuthIssuerHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetAuthIssuerHandler initializes a handler for retrieving an Auth Issuer.
func NewGetAuthIssuerHandler(dbSession *cdb.Session, cfg *config.Config) GetAuthIssuerHandler {
	return GetAuthIssuerHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve an external JWT issuer
// @Description Retrieve a runtime-managed external JWT issuer by ID (Provider Admin only)
// @Tags auth-issuer
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param authIssuerId path string true "ID of the Auth Issuer"
// @Success 200 {object} model.APIAuthIssuer
// @Router /v2/org/{org}/nico/auth-issuer/{authIssuerId} [get]
func (gih GetAuthIssuerHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AuthIssuer", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if apiErr := authorizeAuthIssuerAdmin(gih.cfg, dbUser, org, logger); apiErr != nil {
		return authIssuerAPIErrorResponse(c, apiErr)
	}

	// Get the Auth Issuer ID from the URL parameter.
	issuerStrID := c.Param("authIssuerId")
	issuerID, err := uuid.Parse(issuerStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Auth Issuer ID in URL", nil)
	}

	issuerDAO := cdbm.NewIssuerDAO(gih.dbSession)
	issuer, err := issuerDAO.GetByID(ctx, nil, issuerID)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Auth Issuer not found", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Auth Issuer DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Auth Issuer, DB error", nil)
	}
	history, err := getStatusDetailsByEntityID(ctx, gih.dbSession, []string{issuer.ID.String()})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Auth Issuer status history")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Auth Issuer status history, DB error", nil)
	}

	// Send response
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIAuthIssuer(issuer, history[issuer.ID.String()]))
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteAuthIssuerHandler is the API Handler for deleting an Auth Issuer.
type DeleteAuthIssuerHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewDeleteAuthIssuerHandler initializes a handler for deleting an Auth Issuer.
func NewDeleteAuthIssuerHandler(dbSession *cdb.Session, cfg *config.Config) DeleteAuthIssuerHandler {
	return DeleteAuthIssuerHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete an external JWT issuer
// @Description Delete a runtime-managed external JWT issuer and withdraw its trust, or withdraw the api claim mappings of a ConfigMap issuer (Provider Admin only)
// @Tags auth-issuer
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param authIssuerId path string true "ID of the Auth Issuer"
// @Success 200 {object} model.APIAuthIssuer
// @Router /v2/org/{org}/nico/auth-issuer/{authIssuerId} [delete]
func (dih DeleteAuthIssuerHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AuthIssuer", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if apiErr := authorizeAuthIssuerAdmin(dih.cfg, dbUser, org, logger); apiErr != nil {
		return authIssuerAPIErrorResponse(c, apiErr)
	}

	// Get the Auth Issuer ID from the URL parameter.
	issuerStrID := c.Param("authIssuerId")
	issuerID, err := uuid.Parse(issuerStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Auth Issuer ID in URL", nil)
	}

	issuerDAO := cdbm.NewIssuerDAO(dih.dbSession)

	issuer, err := cdb.WithTxResult(ctx, dih.dbSession, func(tx *cdb.Tx) (*cdbm.Issuer, error) {
		// Acquire an advisory lock on the Auth Issuer organization mapping on which there could be contention.
		// this lock is released when the transaction commits or rollsback
		derr := tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(config.IssuerOrgMappingLockKey), true)
		if derr != nil {
			logger.Error().Err(derr).Msg("Failed to acquire advisory lock on Auth Issuer organization mapping")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Auth Issuer, could not acquire data store lock on Auth Issuers", nil)
		}

		// Check that the Auth Issuer exists. It is read under the lock so a concurrent
		// delete or update cannot slip in between the check and the write
		existing, derr := issuerDAO.GetByID(ctx, tx, issuerID)
		if derr != nil {
			if errors.Is(derr, cdb.ErrDoesNotExist) {
				return nil, cutil.NewAPIError(http.StatusNotFound, "Auth Issuer not found", nil)
			}
			logger.Error().Err(derr).Msg("error retrieving Auth Issuer DB entity")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Auth Issuer to delete, DB error", nil)
		}

		// For a ConfigMap issuer, DELETE removes only its api claim mappings.
		if existing.Source == cdbm.IssuerSourceConfig {
			var kept []cdbm.ClaimMapping
			for _, cm := range existing.ClaimMappings {
				if cm.Source == cdbm.IssuerSourceConfig {
					kept = append(kept, cm)
				}
			}
			if len(kept) == len(existing.ClaimMappings) {
				return existing, nil
			}
			input := cdbm.IssuerCreateOrUpdateInput{
				Origin:         existing.Origin,
				IssuerURL:      existing.IssuerURL,
				JWKSUrl:        existing.JWKSUrl,
				JWKSTimeout:    existing.JWKSTimeout,
				ServiceAccount: existing.ServiceAccount,
				Audiences:      existing.Audiences,
				Scopes:         existing.Scopes,
				ClaimMappings:  kept,
				Source:         cdbm.IssuerSourceConfig,
				ConfigRevision: existing.ConfigRevision,
				CreatedBy:      existing.CreatedBy,
				UpdatedBy:      &dbUser.ID,
			}
			updated, derr := issuerDAO.Update(ctx, tx, existing.ID, input, false)
			if derr != nil {
				logger.Error().Err(derr).Msg("error withdrawing api claim mappings from ConfigMap Auth Issuer")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Auth Issuer api claim mappings", nil)
			}
			if herr := config.RecordIssuerHistory(ctx, dih.dbSession, tx, existing.ID, updated.Status,
				config.IssuerChangeSummary("DELETE by user "+dbUser.ID.String(), *existing, *updated)); herr != nil {
				logger.Error().Err(herr).Msg("error recording Auth Issuer status history")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Auth Issuer api claim mappings", nil)
			}
			return updated, nil
		}

		// The final history entry is written before the soft delete hides the row.
		if herr := config.RecordIssuerHistory(ctx, dih.dbSession, tx, existing.ID, existing.Status, "DELETE by user "+dbUser.ID.String()); herr != nil {
			logger.Error().Err(herr).Msg("error recording Auth Issuer status history")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Auth Issuer", nil)
		}
		// The remaining set is not revalidated: a removal can only shrink the trust
		// plane, so the check could only refuse a delete over some other invalid row.
		derr = issuerDAO.Delete(ctx, tx, issuerID)
		if derr != nil {
			logger.Error().Err(derr).Msg("error deleting Auth Issuer record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Auth Issuer", nil)
		}
		return existing, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete Auth Issuer, DB transaction error")
	}

	// Withdraw trust on this replica; others converge on their next reload.
	dih.cfg.PublishIssuerMutation(ctx, dih.dbSession, issuer, issuer.Source != cdbm.IssuerSourceConfig)

	// Create response
	orgs, roles := config.GrantedOrgsAndRoles(issuer.ClaimMappings)
	logger.Info().Str("issuer", issuer.IssuerURL).Str("source", issuer.Source).Strs("orgs", orgs).Strs("roles", roles).Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIAuthIssuer(issuer, nil))
}
