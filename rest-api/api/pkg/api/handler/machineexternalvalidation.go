// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"errors"
	"fmt"
	"net/http"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

const (
	machineExternalValidationHoldOrderByFieldCreated = "created"
	machineExternalValidationHoldOrderByCreatedAsc   = "CREATED_ASC"

	machineExternalValidationNotImplementedMessage = "Machine external validation is not yet implemented"
)

// GetAllMachineExternalValidationHoldHandler lists the active external validation holds at a Site
type GetAllMachineExternalValidationHoldHandler struct {
	dbSession *cdb.Session
}

// NewGetAllMachineExternalValidationHoldHandler returns a new GetAllMachineExternalValidationHoldHandler
func NewGetAllMachineExternalValidationHoldHandler(dbSession *cdb.Session) GetAllMachineExternalValidationHoldHandler {
	return GetAllMachineExternalValidationHoldHandler{
		dbSession: dbSession,
	}
}

// Handle godoc
// @Summary Retrieve all Machine external validation holds
// @Description Retrieve the active external validation holds at a Site.
// @Tags Machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site"
// @Param machineId query string false "ID of Machine"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Hold creation time ordering" Enums(CREATED_ASC, CREATED_DESC) default(CREATED_ASC)
// @Success 200 {array} model.APIMachineExternalValidationHold
// @Router /v2/org/{org}/nico/machine/external-validation/hold [get]
func (gamevhh GetAllMachineExternalValidationHoldHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineExternalValidationHold", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiError := validateMachineExternalValidationOrgMembership(logger, dbUser, org)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	apiRequest := model.APIGetAllMachineExternalValidationHoldRequest{}
	err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pagination.PageRequest{})
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Machine external validation hold retrieval request data", err)
	}

	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	if pageRequest.OrderByStr == nil {
		pageRequest.OrderByStr = cutil.GetPtr(machineExternalValidationHoldOrderByCreatedAsc)
	}
	err = pageRequest.Validate([]string{machineExternalValidationHoldOrderByFieldCreated})
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, gamevhh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site with ID specified in query", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in query, DB error", nil)
	}

	// A Tenant is only returned when it has targeted Instance creation at the Site.
	provider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gamevhh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}
	if tenant == nil && site.InfrastructureProviderID != provider.ID {
		logger.Warn().Msg("Site doesn't belong to org's Infrastructure Provider")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in query doesn't belong to current org's Provider", nil)
	}

	// TODO: List the holds through Core once the REST proto mirror has the external validation RPCs.
	return cutil.NewAPIErrorResponse(c, http.StatusNotImplemented, machineExternalValidationNotImplementedMessage, nil)
}

// CreateMachineExternalValidationAttemptHandler starts an external validation attempt for a held Machine
type CreateMachineExternalValidationAttemptHandler struct {
	dbSession *cdb.Session
}

// NewCreateMachineExternalValidationAttemptHandler returns a new CreateMachineExternalValidationAttemptHandler
func NewCreateMachineExternalValidationAttemptHandler(dbSession *cdb.Session) CreateMachineExternalValidationAttemptHandler {
	return CreateMachineExternalValidationAttemptHandler{
		dbSession: dbSession,
	}
}

// Handle godoc
// @Summary Start a Machine external validation attempt
// @Description Start an external validation attempt for a Machine with an active external validation hold.
// @Tags Machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param machineId path string true "ID of Machine"
// @Param message body model.APIMachineExternalValidationAttemptCreateRequest true "External validation attempt start request"
// @Success 201 {object} model.APIMachineExternalValidationAttempt
// @Router /v2/org/{org}/nico/machine/{machineId}/external-validation/attempt [post]
func (cmevah CreateMachineExternalValidationAttemptHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineExternalValidationAttempt", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiError := validateMachineExternalValidationOrgMembership(logger, dbUser, org)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	apiRequest := model.APIMachineExternalValidationAttemptCreateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Machine external validation attempt start request data", err)
	}

	apiError = authorizeMachineExternalValidationTenant(ctx, logger, cmevah.dbSession, org, dbUser, c.Param("id"))
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// TODO: Start the attempt through Core once the REST proto mirror has the external validation RPCs.
	return cutil.NewAPIErrorResponse(c, http.StatusNotImplemented, machineExternalValidationNotImplementedMessage, nil)
}

// CompleteMachineExternalValidationAttemptHandler records the result of an external validation attempt
type CompleteMachineExternalValidationAttemptHandler struct {
	dbSession *cdb.Session
}

// NewCompleteMachineExternalValidationAttemptHandler returns a new CompleteMachineExternalValidationAttemptHandler
func NewCompleteMachineExternalValidationAttemptHandler(dbSession *cdb.Session) CompleteMachineExternalValidationAttemptHandler {
	return CompleteMachineExternalValidationAttemptHandler{
		dbSession: dbSession,
	}
}

// Handle godoc
// @Summary Complete a Machine external validation attempt
// @Description Record the result of an external validation attempt whose validation Instance was allocated.
// @Tags Machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param machineId path string true "ID of Machine"
// @Param attemptId path string true "ID of external validation attempt"
// @Param message body model.APIMachineExternalValidationAttemptCompleteRequest true "External validation attempt completion request"
// @Success 200 {object} model.APIMachineExternalValidationAttempt
// @Router /v2/org/{org}/nico/machine/{machineId}/external-validation/attempt/{attemptId}/complete [post]
func (cmevah CompleteMachineExternalValidationAttemptHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineExternalValidationAttempt", "Complete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiError := validateMachineExternalValidationOrgMembership(logger, dbUser, org)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	_, err := uuid.Parse(c.Param("attemptId"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Machine external validation attempt ID in URL", nil)
	}

	apiRequest := model.APIMachineExternalValidationAttemptCompleteRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Machine external validation attempt completion request data", err)
	}

	apiError = authorizeMachineExternalValidationTenant(ctx, logger, cmevah.dbSession, org, dbUser, c.Param("id"))
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// TODO: Complete the attempt through Core once the REST proto mirror has the external validation RPCs.
	return cutil.NewAPIErrorResponse(c, http.StatusNotImplemented, machineExternalValidationNotImplementedMessage, nil)
}

// CancelMachineExternalValidationAttemptHandler cancels an external validation attempt before its Instance is allocated
type CancelMachineExternalValidationAttemptHandler struct {
	dbSession *cdb.Session
}

// NewCancelMachineExternalValidationAttemptHandler returns a new CancelMachineExternalValidationAttemptHandler
func NewCancelMachineExternalValidationAttemptHandler(dbSession *cdb.Session) CancelMachineExternalValidationAttemptHandler {
	return CancelMachineExternalValidationAttemptHandler{
		dbSession: dbSession,
	}
}

// Handle godoc
// @Summary Cancel a Machine external validation attempt
// @Description Cancel an external validation attempt before its validation Instance is allocated.
// @Tags Machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param machineId path string true "ID of Machine"
// @Param attemptId path string true "ID of external validation attempt"
// @Param message body model.APIMachineExternalValidationAttemptCancelRequest true "External validation attempt cancellation request"
// @Success 200 {object} model.APIMachineExternalValidationAttempt
// @Router /v2/org/{org}/nico/machine/{machineId}/external-validation/attempt/{attemptId}/cancel [post]
func (cmevah CancelMachineExternalValidationAttemptHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineExternalValidationAttempt", "Cancel", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiError := validateMachineExternalValidationOrgMembership(logger, dbUser, org)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	_, err := uuid.Parse(c.Param("attemptId"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Machine external validation attempt ID in URL", nil)
	}

	apiRequest := model.APIMachineExternalValidationAttemptCancelRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Machine external validation attempt cancellation request data", err)
	}

	apiError = authorizeMachineExternalValidationTenant(ctx, logger, cmevah.dbSession, org, dbUser, c.Param("id"))
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// TODO: Cancel the attempt through Core once the REST proto mirror has the external validation RPCs.
	return cutil.NewAPIErrorResponse(c, http.StatusNotImplemented, machineExternalValidationNotImplementedMessage, nil)
}

// DeleteMachineExternalValidationHoldHandler removes an external validation hold from a Machine
type DeleteMachineExternalValidationHoldHandler struct {
	dbSession *cdb.Session
}

// NewDeleteMachineExternalValidationHoldHandler returns a new DeleteMachineExternalValidationHoldHandler
func NewDeleteMachineExternalValidationHoldHandler(dbSession *cdb.Session) DeleteMachineExternalValidationHoldHandler {
	return DeleteMachineExternalValidationHoldHandler{
		dbSession: dbSession,
	}
}

// Handle godoc
// @Summary Delete a Machine external validation hold
// @Description Remove an external validation hold from a Machine without a passing validation.
// @Tags Machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param machineId path string true "ID of Machine"
// @Param holdId path string true "ID of external validation hold"
// @Param message body model.APIMachineExternalValidationHoldDeleteRequest true "External validation hold removal request"
// @Success 204
// @Router /v2/org/{org}/nico/machine/{machineId}/external-validation/hold/{holdId} [delete]
func (dmevhh DeleteMachineExternalValidationHoldHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineExternalValidationHold", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiError := validateMachineExternalValidationOrgMembership(logger, dbUser, org)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	_, err := uuid.Parse(c.Param("holdId"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Machine external validation hold ID in URL", nil)
	}

	apiRequest := model.APIMachineExternalValidationHoldDeleteRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Machine external validation hold removal request data", err)
	}

	provider, err := common.GetInfrastructureProviderForOrg(ctx, nil, dmevhh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	_, apiError = getMachineForValidation(ctx, logger, dmevhh.dbSession, provider, c.Param("id"))
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// TODO: Remove the hold through Core once the REST proto mirror has the external validation RPCs.
	return cutil.NewAPIErrorResponse(c, http.StatusNotImplemented, machineExternalValidationNotImplementedMessage, nil)
}

func validateMachineExternalValidationOrgMembership(logger zerolog.Logger, dbUser *cdbm.User, org string) *cutil.APIError {
	if dbUser == nil {
		logger.Error().Msg("Invalid User object found in request context")
		return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("Error validating org membership for User in request")
		} else {
			logger.Warn().Msg("Could not validate org membership for user, access denied")
		}
		return cutil.NewAPIError(http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	return nil
}

// authorizeMachineExternalValidationTenant ensures the org's Tenant can run external validation on
// the Machine. Core only accepts attempts from the Tenant configured for the Site, which needs
// targeted Instance creation there to create the validation Instance.
func authorizeMachineExternalValidationTenant(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, org string, dbUser *cdbm.User, machineID string) *cutil.APIError {
	machine, err := cdbm.NewMachineDAO(dbSession).GetByID(ctx, nil, machineID, nil, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIError(http.StatusNotFound, "Could not find Machine with specified ID", nil)
		}
		logger.Error().Err(err).Msg("failed to retrieve Machine details from DB")
		return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Machine details, DB error", nil)
	}

	_, apiError := common.IsTenant(ctx, logger, dbSession, org, dbUser, &common.TenantPrivilegeScope{SiteID: &machine.SiteID})
	return apiError
}
