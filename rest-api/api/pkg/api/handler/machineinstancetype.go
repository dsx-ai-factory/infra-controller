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

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"

	"github.com/google/uuid"

	"github.com/labstack/echo/v4"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateMachineInstanceTypeHandler is the API Handler for creating new Machine/InstanceType association
type CreateMachineInstanceTypeHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateMachineInstanceTypeHandler initializes and returns a new handler for creating Machine/Instance Type association
func NewCreateMachineInstanceTypeHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) CreateMachineInstanceTypeHandler {
	return CreateMachineInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create an association between Machine and Instance Type
// @Description Create an association between Machine and Instance Type. Only Infrastructure Providers who own both the Machine and the Instance Type can create the association.
// @Tags machineinstancetype
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param instance_type_id query string true "ID of Instance Type"
// @Param message body model.APIMachineInstanceTypeCreateRequest true "Instance Type create request"
// @Success 201 {object} model.APIMachineInstanceType
// @Router /v2/org/{org}/nico/instance/type/{instance_type_id}/machine [post]
func (cmith CreateMachineInstanceTypeHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineInstanceType", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to create Machine/InstanceType associations
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Instance Type ID
	itStrID := c.Param("instanceTypeId")

	cotel.SetAttribute(handlerSpan, attribute.String("instancetype_id", itStrID))

	itID, err := uuid.Parse(itStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Instance Type ID in URL", nil)
	}

	// Check if org has an Infrastructure Provider
	ipDAO := cdbm.NewInfrastructureProviderDAO(cmith.dbSession)

	ips, serr := ipDAO.GetAllByOrg(ctx, nil, org, nil)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Infrastructure Provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to to retrieve Org entities to check Instance Type association", nil)
	}

	if len(ips) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have an Infrastructure Provider", nil)
	}

	orgIP := &ips[0]

	// Get Instance Type
	itDAO := cdbm.NewInstanceTypeDAO(cmith.dbSession)

	it, err := itDAO.GetByID(ctx, nil, itID, []string{"Site"})
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Instance Type not found", nil)
		}

		logger.Error().Err(err).Msg("error retrieving Instance Type from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance Type", nil)
	}

	// Check if Instance Type is associated with the Org's Provider
	if orgIP.ID != it.InfrastructureProviderID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Instance Type is not associated with org's Infrastructure Provider", nil)
	}

	// Check that the DB data is sane and that the InstanceType is associated with a site.
	if it.SiteID == nil {
		logger.Error().Msg("InstanceType is not associated with a site")
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Failed to associate Machines with Instance Type because Instance Type is not associated with a Site.", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIMachineInstanceTypeCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Machine/Instance Type Association creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Error validating Machine/Instance Type Association creation request data", verr)
	}

	// Verify if Capabilities of Machine matches with Instance Type's Capabilities.
	// This is a pure validation read; keep it outside the tx so it does not pin
	// a DB connection while the workflow runs.
	isMatch, badMachineID, apiErr := common.MatchInstanceTypeCapabilitiesForMachines(ctx, logger, cmith.dbSession, it.ID, apiRequest.MachineIDs)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	if !isMatch {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, fmt.Sprintf("Capabilities for Machine: %v do not match Instance Type's Capabilities", *badMachineID), nil)
	}

	// Get the temporal client for the site we are working with.
	// SiteID was checked early on in this handler.
	stc, err := cmith.scp.GetClientByID(*it.SiteID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Values populated inside the transaction closure that are needed for the response.
	amits := []model.APIMachineInstanceType{}

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, cmith.dbSession, func(tx *cdb.Tx) error {
		// Iterate through Machine IDs in request and create associations
		mDAO := cdbm.NewMachineDAO(cmith.dbSession)
		mitDAO := cdbm.NewMachineInstanceTypeDAO(cmith.dbSession)

		for _, machineID := range apiRequest.MachineIDs {
			slogger := logger.With().Str("MachineID", machineID).Logger()

			m, derr := mDAO.GetByID(ctx, tx, machineID, nil, false)
			if derr != nil {
				if derr == cdb.ErrDoesNotExist {
					return cutil.NewAPIError(http.StatusNotFound, fmt.Sprintf("Machine with ID: %v does not exist", machineID), nil)
				}

				slogger.Error().Err(derr).Msg("error retrieving Machine from DB")
				return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve details for Machine: %v", machineID), nil)
			}

			if m.Status != cdbm.MachineStatusReady && m.Status != cdbm.MachineStatusReset {
				return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Machine: %v is in %v state. Instance Type can only be assigned to a Machine in `Ready` or `Reset` status", m.ID, m.Status), nil)
			}

			// Check if Machine is associated with the Org's Provider
			if orgIP.ID != m.InfrastructureProviderID {
				return cutil.NewAPIError(http.StatusForbidden, fmt.Sprintf("Machine: %v is not associated with org's Infrastructure Provider", machineID), nil)
			}

			// check for association with any instance type
			emits, _, derr := mitDAO.GetAll(ctx, tx, cdbm.MachineInstanceTypeFilterInput{MachineID: &machineID}, paginator.PageInput{}, nil)
			if derr != nil {
				slogger.Error().Err(derr).Msg("error retrieving Machine/InstanceType association from DB")
				return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to check for existing Instance Type association for Machine: %v", machineID), nil)
			}

			// If association exists with any instance type already, return error
			if len(emits) > 0 {
				return cutil.NewAPIError(http.StatusConflict, fmt.Sprintf("Machine: %v is already associated with Instance Type %v", machineID, emits[0].InstanceTypeID), nil)
			}

			// Create Machine/InstanceType association
			mit, derr := mitDAO.Create(ctx, tx, cdbm.MachineInstanceTypeCreateInput{
				MachineID:      machineID,
				InstanceTypeID: itID,
			})
			if derr != nil {
				slogger.Error().Err(derr).Msg("error creating Machine/InstanceType association")
				return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to create Instance Type association for Machine: %v", machineID), nil)
			}

			// Set Machine's Instance Type ID
			_, derr = mDAO.Update(ctx, tx, cdbm.MachineUpdateInput{MachineID: m.ID, InstanceTypeID: &it.ID})
			if derr != nil {
				slogger.Error().Err(derr).Msg("error updating Instance Type ID for Machine")
				return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to update Instance Type for Machine: %v", machineID), nil)
			}

			amit := model.NewAPIMachineInstanceType(mit)
			amits = append(amits, *amit)
		}

		// Send the machine association update to NICo
		associateMachinesRequest := apiRequest.ToProto(it)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "associate-machines-with-instance-type-" + it.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering AssociateMachinesWithInstanceType workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "AssociateMachinesWithInstanceType", associateMachinesRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to associate Machines with InstanceType")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to associate Machines with Instance Type on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous AssociateMachinesWithInstanceType workflow")

		// Block until the workflow has completed and returned success/error.
		wferr = we.Get(wfCtx, nil)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to associate Machines with Instance Type, timeout occurred executing workflow on Site.")
				timeoutCause := wferr
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, timeoutCause, "MachineInstanceType", "AssociateMachinesWithInstanceType")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "AssociateMachinesWithInstanceType workflow timed out", nil)
			}

			code, wferr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(wferr).Msg("failed to synchronously execute Temporal workflow to associate Machines with InstanceType")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to associate Machines with Instance Type on Site: %s", wferr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous AssociateMachinesWithInstanceType workflow")

		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to associate Machines with Instance Type, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Return response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusCreated, amits)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllMachineInstanceTypeHandler is the API Handler for getting all Instance Types
type GetAllMachineInstanceTypeHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllMachineInstanceTypeHandler initializes and returns a new handler for getting all Instance Types
func NewGetAllMachineInstanceTypeHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllMachineInstanceTypeHandler {
	return GetAllMachineInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all Machine/Instance Types associations
// @Description Get all Machine/Instance Types associations for Instance Type
// @Tags machineinstancetype
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param instance_type_id query string true "ID of Instance Type"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIMachineInstanceType
// @Router /v2/org/{org}/nico/instance/type/{instance_type_id}/machine [get]
func (gamith GetAllMachineInstanceTypeHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineInstanceType", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to retrieve Machine/InstanceType associations
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Validate paginantion request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.MachineInstanceTypeOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get Instance Type ID
	itStrID := c.Param("instanceTypeId")

	cotel.SetAttribute(handlerSpan, attribute.String("instancetype_id", itStrID))

	itID, err := uuid.Parse(itStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Instance Type ID in URL", nil)
	}

	// Check if org has an Infrastructure Provider
	ipDAO := cdbm.NewInfrastructureProviderDAO(gamith.dbSession)

	ips, serr := ipDAO.GetAllByOrg(ctx, nil, org, nil)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Infrastructure Provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to to retrieve Org entities to check Instance Type association", nil)
	}

	if len(ips) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have an Infrastructure Provider", nil)
	}

	orgIP := &ips[0]

	// Get Instance Type
	itDAO := cdbm.NewInstanceTypeDAO(gamith.dbSession)

	it, err := itDAO.GetByID(ctx, nil, itID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Instance Type not found", nil)
		}

		logger.Error().Err(err).Msg("error retrieving Instance Type from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance Type", nil)
	}

	// Check if Instance Type is associated with the Org's Provider
	if orgIP.ID != it.InfrastructureProviderID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Instance Type is not associated with org's Infrastructure Provider", nil)
	}

	// Get all Machine/InstanceType associations
	mitDAO := cdbm.NewMachineInstanceTypeDAO(gamith.dbSession)

	emits, total, err := mitDAO.GetAll(ctx, nil, cdbm.MachineInstanceTypeFilterInput{
		InstanceTypeIDs: []uuid.UUID{itID},
	}, paginator.PageInput{
		Offset:  pageRequest.Offset,
		Limit:   pageRequest.Limit,
		OrderBy: pageRequest.OrderBy,
	}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Machine/InstanceType associations from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Machine/Instance Type associations", nil)
	}

	// Return response
	amits := []*model.APIMachineInstanceType{}

	for _, mit := range emits {
		amits = append(amits, model.NewAPIMachineInstanceType(&mit))
	}

	// Create pagination response header
	pageReponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageReponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}

	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, amits)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteMachineInstanceTypeHandler is the API Handler for deleting a Machine/InstanceType association
type DeleteMachineInstanceTypeHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteMachineInstanceTypeHandler initializes and returns a new handler for deleting a Machine/InstanceType association
func NewDeleteMachineInstanceTypeHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) DeleteMachineInstanceTypeHandler {
	return DeleteMachineInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete a Machine/InstanceType association
// @Description Delete a Machine/InstanceType association for Instance Type. The `{id}` path parameter accepts either a `machineId` or the deprecated Machine/InstanceType association ID, which will be removed on July 9th, 2026 0:00 UTC.
// @Tags machineinstancetype
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param instance_type_id path string true "ID of Instance Type"
// @Param id path string true "Machine ID or deprecated ID of Machine/Instance Type association"
// @Success 204
// @Router /v2/org/{org}/nico/instance/type/{instance_type_id}/machine/{id} [delete]
func (dmith DeleteMachineInstanceTypeHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineInstanceType", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to delete Machine/InstanceType associations
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Instance Type ID
	itStrID := c.Param("instanceTypeId")
	itID, err := uuid.Parse(itStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Instance Type ID in URL", nil)
	}

	// Check if org has an Infrastructure Provider
	ipDAO := cdbm.NewInfrastructureProviderDAO(dmith.dbSession)

	ips, serr := ipDAO.GetAllByOrg(ctx, nil, org, nil)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Infrastructure Provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to to retrieve Org entities to check Instance Type association", nil)
	}

	if len(ips) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have an Infrastructure Provider", nil)
	}

	orgIP := &ips[0]

	// Get Instance Type
	itDAO := cdbm.NewInstanceTypeDAO(dmith.dbSession)

	it, err := itDAO.GetByID(ctx, nil, itID, []string{"Site"})
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Instance Type not found", nil)
		}

		logger.Error().Err(err).Msg("error retrieving Instance Type from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance Type", nil)
	}

	// Check if Instance Type is associated with the Org's Provider
	if orgIP.ID != it.InfrastructureProviderID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Instance Type is not associated with org's Infrastructure Provider", nil)
	}

	// Check that the DB data is sane and that the InstanceType is associated with a site.
	if it.SiteID == nil {
		logger.Error().Msg("InstanceType is not associated with a site")
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Failed to remove associate Machines with Instance Type because Instance Type is not associated with a Site.", nil)
	}

	// Resolve the delete identifier from either the machine ID or the deprecated association ID.
	machineOrAssociationID := c.Param("id")
	cotel.SetAttribute(handlerSpan, attribute.String("machineinstancetype_identifier", machineOrAssociationID))

	// Look up the association first by deprecated association ID and then by machine ID.
	mitDAO := cdbm.NewMachineInstanceTypeDAO(dmith.dbSession)

	var mit *cdbm.MachineInstanceType

	associationID, err := uuid.Parse(machineOrAssociationID)
	if err == nil {
		mit, err = mitDAO.GetByID(ctx, nil, associationID, nil)
		if err != nil && err != cdb.ErrDoesNotExist {
			logger.Error().Err(err).Msg("error retrieving Machine/InstanceType association by deprecated association ID from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Machine/Instance Type associations", nil)
		}
		if err == cdb.ErrDoesNotExist {
			mit = nil
		}
	}

	if mit == nil {
		mits, _, err := mitDAO.GetAll(ctx, nil, cdbm.MachineInstanceTypeFilterInput{
			MachineID:       &machineOrAssociationID,
			InstanceTypeIDs: []uuid.UUID{itID},
		}, paginator.PageInput{}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Machine/InstanceType association by Machine ID from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Machine/Instance Type associations", nil)
		}
		if len(mits) > 0 {
			mit = &mits[0]
		}
	}

	if mit == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Machine/Instance Type association with ID specified in URL", nil)
	}

	// Check if Machine/InstanceType association belongs to the Instance Type
	if mit.InstanceTypeID != itID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Machine/Instance Type association does not belong to Instance Type", nil)
	}

	// Check that the Machine is not in use
	insDAO := cdbm.NewInstanceDAO(dmith.dbSession)
	_, insCount, err := insDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{MachineIDs: []string{mit.MachineID}}, paginator.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Instances from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instances for Machine", nil)
	}
	if insCount > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Machine is currently in use by an Instance and cannot be dissociated from Instance Type", nil)
	}

	// Get the temporal client for the site we are working with.
	// SiteID was checked early on in this handler.
	stc, err := dmith.scp.GetClientByID(*it.SiteID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, dmith.dbSession, func(tx *cdb.Tx) error {
		// take an advisory lock - this is needed because
		// of the accounting checks below on allocation constraint satisfaction across all tenants
		// after machine instance type deletion
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(it.ID.String()), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to take advisory lock")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Machine/Instance Type association due to db error", nil)
		}

		// Delete Machine/InstanceType association
		derr = mitDAO.Delete(ctx, tx, mit.ID, false)
		if derr != nil {
			logger.Error().Err(derr).Msg("error deleting Machine/InstanceType association from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Machine/Instance Type association", nil)
		}

		// check if the available machines violates the allocation constraint requirement
		ok, derr := common.CheckMachinesForInstanceTypeAllocation(ctx, tx, dmith.dbSession, logger, mit.InstanceTypeID, 0)
		if derr != nil {
			logger.Error().Err(derr).Str("resourceId", mit.InstanceTypeID.String()).Msg("error checking available machines for instance type allocation")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error checking machine availability for the instance type allocation", nil)
		}
		if !ok {
			logger.Warn().Str("resourceId", mit.InstanceTypeID.String()).Msg("Deletion of machine instance type is not allowed because of existing allocation constraints")
			return cutil.NewAPIError(http.StatusBadRequest, "Deletion of Machine/Instance type association is not allowed because of existing Allocation Constraints", nil)
		}

		// Clear Machine's Instance Type
		mDAO := cdbm.NewMachineDAO(dmith.dbSession)
		_, derr = mDAO.Clear(ctx, tx, cdbm.MachineClearInput{MachineID: mit.MachineID, InstanceTypeID: true})
		if derr != nil {
			logger.Error().Err(derr).Msg("error clearing Machine's Instance Type")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Machine/Instance Type association", nil)
		}

		// Send the machine association update to NICo

		// Now that machine data is "versioned" in NICo, a future update will likely
		// allow us to send in IfVersion here to protect against concurrent updates.
		removeAssociationRequest := mit.ToRemoveAssociationRequestProto()

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "remove-machine-instance-type-association" + it.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering RemoveMachineInstanceTypeAssociation workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "RemoveMachineInstanceTypeAssociation", removeAssociationRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to remove Machine association with InstanceType")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to remove Machine association with Instance Type on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous RemoveMachineInstanceTypeAssociation workflow")

		// Block until the workflow has completed and returned success/error.
		wferr = we.Get(wfCtx, nil)

		// Handle skippable errors
		if wferr != nil {
			// If this was a 404 back from NICo, we can treat the object as already having been deleted and allow things to proceed.
			var applicationErr *tp.ApplicationError
			if errors.As(wferr, &applicationErr) && slices.Contains(swe.ObjectNotFoundErrTypes(), applicationErr.Type()) {
				logger.Warn().Msg(swe.ErrTypeNICoObjectNotFound + " received from Site")
				// Reset error to nil
				wferr = nil
			}
		}

		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to remove Machine association with Instance Type, timeout occurred executing workflow on Site.")
				timeoutCause := wferr
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, timeoutCause, "MachineInstanceType", "RemoveMachineInstanceTypeAssociation")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "RemoveMachineInstanceTypeAssociation workflow timed out", nil)
			}

			code, wferr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(wferr).Msg("failed to synchronously execute Temporal workflow to remove Machine association with InstanceType")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to remove Machine association with Instance Type on Site: %s", wferr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous RemoveMachineInstanceTypeAssociation workflow")

		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to remove Machine/Instance Type association, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Return response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
