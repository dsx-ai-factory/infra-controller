// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"

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

// authIssuerAPIUnavailableMessage is returned when Keycloak is enabled. Every handler
// here re-checks the conditions behind config.DynamicIssuersEnabled, so a handler
// reached without the startup route gate still refuses to write to the trust plane.
const (
	authIssuerAPIUnavailableMessage = "Auth Issuer management via the API is unavailable while Keycloak is enabled."
	authIssuerOrderByDefault        = "CREATED_AT_ASC"
)

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
	return nil
}

func authIssuerAPIErrorResponse(c echo.Context, apiErr *cutil.APIError) error {
	return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
}

func authIssuerFromDBModel(dbIssuer *cdbm.Issuer) *model.APIAuthIssuer {
	if dbIssuer == nil {
		return nil
	}
	issuer := &model.APIAuthIssuer{}
	issuer.FromDBModel(dbIssuer)
	return issuer
}

// ~~~~~ Create-or-update Handler ~~~~~ //

// claimMappingAudit flattens the orgs and roles an issuer grants so every trust
// mutation records what it changed, not just which issuer.
func claimMappingAudit(mappings []cdbm.ClaimMapping) (orgs []string, roles []string) {
	seenRoles := map[string]bool{}
	for _, cm := range mappings {
		orgs = append(orgs, cm.OrgName)
		for _, r := range cm.Roles {
			if !seenRoles[r] {
				seenRoles[r] = true
				roles = append(roles, r)
			}
		}
	}
	return orgs, roles
}

// CreateOrUpdateAuthIssuerHandler creates or fully replaces an Auth Issuer keyed
// by issuerUrl. API-managed Auth Issuers always have origin=custom, and mutation
// is rejected when the static ConfigMap issuers block contains keycloak,
// kas-legacy, or kas-ssa origins.
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
	changed bool
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

	if !cih.cfg.GetAuthAllowInternalIssuerHosts() {
		if verr := apiRequest.ValidateExternalHosts(); verr != nil {
			logger.Warn().Err(verr).Msg("Auth Issuer endpoints target an internal network address")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
				"Auth Issuer endpoints must not target internal network addresses", verr)
		}
	}

	// A custom Auth Issuer may not join a trust plane owned by a privileged IdP.
	if cih.cfg.HasPrivilegedStaticIssuerOrigins() {
		logger.Warn().Msg("static configuration defines privileged issuer origins, cannot manage custom Auth Issuer")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Cannot manage a custom Auth Issuer while keycloak, kas-legacy, or kas-ssa issuers are defined in the static configuration. "+
				"Remove those issuers from the ConfigMap issuers block first.", nil)
	}

	upsertInput := apiRequest.ToCreateOrUpdateInput(&dbUser.ID)

	// Static ConfigMap issuers always win.
	if cih.cfg.IsStaticIssuer(upsertInput.IssuerURL, upsertInput.JWKSUrl) {
		logger.Warn().Str("issuer", upsertInput.IssuerURL).Str("jwks", upsertInput.JWKSUrl).
			Msg("Issuer URL or JWKS URL is reserved by a statically-configured issuer")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict,
			"Issuer URL or JWKS URL is reserved by a statically-configured issuer", nil)
	}

	issuerDAO := cdbm.NewIssuerDAO(cih.dbSession)

	result, err := cdb.WithTxResult(ctx, cih.dbSession, func(tx *cdb.Tx) (*authIssuerUpsertResult, error) {
		// Acquire an advisory lock on the Auth Issuer organization mapping on which there could be contention.
		// this lock is released when the transaction commits or rollsback
		derr := tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(config.IssuerOrgMappingLockKey), true)
		if derr != nil {
			logger.Error().Err(derr).Msg("Failed to acquire advisory lock on Auth Issuer organization mapping")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to create or update Auth Issuer, could not acquire data store lock on Auth Issuers", nil)
		}

		existing, derr := issuerDAO.GetByIssuerURL(ctx, tx, upsertInput.IssuerURL)
		if derr != nil && !errors.Is(derr, cdb.ErrDoesNotExist) {
			logger.Error().Err(derr).Msg("error retrieving Auth Issuer before create or update")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Auth Issuer, DB error", nil)
		}
		if errors.Is(derr, cdb.ErrDoesNotExist) {
			existing = nil
		}

		// Validate the complete replacement against the combined static and DB
		// issuer set. Excluding the current row prevents an update from conflicting
		// with itself while retaining every cross-issuer uniqueness check.
		candidate := upsertInput.ToIssuer()
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
			return &authIssuerUpsertResult{issuer: createdIssuer, created: true, changed: true}, nil
		}

		// An identical PUT is a true no-op: keep timestamps and cached signing keys.
		if existing.Signature() == candidate.Signature() {
			return &authIssuerUpsertResult{issuer: existing}, nil
		}

		updatedIssuer, derr := issuerDAO.Update(
			ctx,
			tx,
			existing.ID,
			upsertInput,
			existing.JWKSUrl != candidate.JWKSUrl,
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating Auth Issuer record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Auth Issuer", nil)
		}
		return &authIssuerUpsertResult{issuer: updatedIssuer, changed: true}, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create or update Auth Issuer, DB transaction error")
	}

	if result.changed {
		// Apply to the live registry on this replica (read-your-write). Other
		// replicas converge on their next periodic reload.
		rerr := cih.cfg.ReloadDBIssuers(ctx, cih.dbSession)
		if rerr != nil {
			logger.Warn().Err(rerr).Msg("Auth Issuer changed but live registry reload failed, will converge on next reload")
		}
	}

	status := http.StatusOK
	if result.created {
		status = http.StatusCreated
	}
	orgs, roles := claimMappingAudit(result.issuer.ClaimMappings)
	logger.Info().Str("issuer", result.issuer.IssuerURL).Bool("created", result.created).Bool("changed", result.changed).
		Strs("orgs", orgs).Strs("roles", roles).
		Msg("finishing API handler")
	return c.JSON(status, authIssuerFromDBModel(result.issuer))
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

	issuerDAO := cdbm.NewIssuerDAO(gaih.dbSession)
	issuers, total, err := issuerDAO.GetPage(ctx, nil, cdbm.IssuerFilterInput{}, pageRequest.ConvertToDB())
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Auth Issuers from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Auth Issuers, DB error", nil)
	}

	// Create response
	apiIssuers := []*model.APIAuthIssuer{}
	for i := range issuers {
		apiIssuers = append(apiIssuers, authIssuerFromDBModel(&issuers[i]))
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

	// Send response
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, authIssuerFromDBModel(issuer))
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
// @Description Delete a runtime-managed external JWT issuer and withdraw its trust (Provider Admin only)
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

	// Withdraw trust from the live registry on this replica. Other replicas
	// converge on their next periodic reload.
	rerr := dih.cfg.ReloadDBIssuers(ctx, dih.dbSession)
	if rerr != nil {
		logger.Warn().Err(rerr).Msg("Auth Issuer deleted but live registry reload failed, will converge on next reload")
	}

	// Create response
	orgs, roles := claimMappingAudit(issuer.ClaimMappings)
	logger.Info().Str("issuer", issuer.IssuerURL).Strs("orgs", orgs).Strs("roles", roles).Msg("finishing API handler")
	return c.JSON(http.StatusOK, authIssuerFromDBModel(issuer))
}
