// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"crypto/sha1"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	temporalEnums "go.temporal.io/api/enums/v1"
	tclient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// payloadHash returns a deterministic SHA1 hex digest of the proto message,
// used as a content-addressable suffix on idempotent PUT workflow IDs.
func payloadHash(m proto.Message) (string, error) {
	b, err := proto.MarshalOptions{Deterministic: true}.Marshal(m)
	if err != nil {
		return "", err
	}
	sum := sha1.Sum(b)
	return hex.EncodeToString(sum[:]), nil
}

// ~~~~~ Create Or Update Handler ~~~~~ //

// CreateOrUpdateTenantIdentityConfigHandler handles PUT /tenant-identity/config.
type CreateOrUpdateTenantIdentityConfigHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewCreateOrUpdateTenantIdentityConfigHandler returns a new CreateOrUpdateTenantIdentityConfigHandler.
func NewCreateOrUpdateTenantIdentityConfigHandler(dbSession *cdb.Session, scp *sc.ClientPool) CreateOrUpdateTenantIdentityConfigHandler {
	return CreateOrUpdateTenantIdentityConfigHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create Or Update Tenant Identity Config
// @Description Create or update the per-tenant identity (JWT-SVID) config. First call for a tenant generates the signing keypair.
// @Tags TenantIdentity
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteID path string true "ID of Site"
// @Param message body model.APITenantIdentityConfigCreateOrUpdateRequest true "Tenant Identity Config create/update request"
// @Success 201 {object} model.APITenantIdentityConfig "Config created on first call"
// @Success 200 {object} model.APITenantIdentityConfig "Config replaced/updated"
// @Failure 503 {object} util.APIError
// @Router /v2/org/{org}/nico/site/{siteID}/tenant-identity/config [put]
func (umich CreateOrUpdateTenantIdentityConfigHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantIdentity", "CreateOrUpdate", c, umich.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	siteID := c.Param("siteID")
	if siteID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Missing siteID path parameter", nil)
	}

	if ok, err := auth.ValidateOrgMembership(dbUser, org); !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}
	if ok := auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole); !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	if _, err := common.GetTenantForOrg(ctx, nil, umich.dbSession, org); err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, siteID, umich.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) || errors.Is(err, common.ErrInvalidID) {
			logger.Warn().Err(err).Str("Site ID", siteID).Msg("site not found in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site with ID specified in request data", nil)
		}
		logger.Error().Err(err).Str("Site ID", siteID).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	temporalClient, err := umich.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	apiRequest := model.APITenantIdentityConfigCreateOrUpdateRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating Tenant Identity Config create/update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Tenant Identity Config create/update request data", verr)
	}

	protoRequest := apiRequest.ToProto(org)

	hash, err := payloadHash(protoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to hash request payload for workflow ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to hash request payload", nil)
	}
	workflowOptions := tclient.StartWorkflowOptions{
		ID:                       "tenant-identity-config-create-or-update-" + org + "-" + site.ID.String() + "-" + hash,
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
		WorkflowIDConflictPolicy: temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	}

	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	we, err := temporalClient.ExecuteWorkflow(ctx, workflowOptions, "CreateOrUpdateTenantIdentityConfiguration", protoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to create or update Tenant Identity Config")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to start workflow to create or update Tenant Identity Config", nil)
	}

	wid := we.GetID()
	logger.Info().Str("Workflow ID", wid).Msg("executed synchronous create or update Tenant Identity Config workflow")

	var protoResponse corev1.TenantIdentityConfigResponse
	err = we.Get(ctx, &protoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || err == context.DeadlineExceeded || ctx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, wid, err, "TenantIdentity", "CreateOrUpdateTenantIdentityConfiguration")
		}

		code, unwrapped := common.UnwrapWorkflowError(err)
		logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to create or update Tenant Identity Config")
		return cutil.NewAPIErrorResponse(c, code, "Failed to create or update Tenant Identity Config", nil)
	}

	apiConfig := &model.APITenantIdentityConfig{}
	apiConfig.FromResponseProto(&protoResponse)
	status := http.StatusOK
	if apiConfig.IsCreated() {
		status = http.StatusCreated
	}
	return c.JSON(status, apiConfig)
}

// ~~~~~ Get Handler ~~~~~ //

// GetTenantIdentityConfigHandler handles GET /tenant-identity/config.
type GetTenantIdentityConfigHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewGetTenantIdentityConfigHandler returns a new GetTenantIdentityConfigHandler.
func NewGetTenantIdentityConfigHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetTenantIdentityConfigHandler {
	return GetTenantIdentityConfigHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve Tenant Identity Config for an org
// @Description Retrieve the current per-org tenant identity configuration and signing keys.
// @Tags TenantIdentity
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteID path string true "ID of Site"
// @Success 200 {object} model.APITenantIdentityConfig
// @Router /v2/org/{org}/nico/site/{siteID}/tenant-identity/config [get]
func (gmich GetTenantIdentityConfigHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantIdentity", "Get", c, gmich.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	siteID := c.Param("siteID")
	if siteID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Missing siteID path parameter", nil)
	}

	if ok, err := auth.ValidateOrgMembership(dbUser, org); !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}
	if ok := auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole); !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	if _, err := common.GetTenantForOrg(ctx, nil, gmich.dbSession, org); err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, siteID, gmich.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) || errors.Is(err, common.ErrInvalidID) {
			logger.Warn().Err(err).Str("Site ID", siteID).Msg("site not found in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site with ID specified in request data", nil)
		}
		logger.Error().Err(err).Str("Site ID", siteID).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	temporalClient, err := gmich.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	protoRequest := &corev1.GetTenantIdentityConfigRequest{OrganizationId: org}

	workflowOptions := tclient.StartWorkflowOptions{
		ID:                       "tenant-identity-config-get-" + org + "-" + site.ID.String(),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
		WorkflowIDConflictPolicy: temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	}

	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	we, err := temporalClient.ExecuteWorkflow(ctx, workflowOptions, "GetTenantIdentityConfiguration", protoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to get Tenant Identity Config")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to start workflow to get Tenant Identity Config", nil)
	}

	wid := we.GetID()
	logger.Info().Str("Workflow ID", wid).Msg("executed synchronous get Tenant Identity Config workflow")

	var protoResponse corev1.TenantIdentityConfigResponse
	err = we.Get(ctx, &protoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || err == context.DeadlineExceeded || ctx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, wid, err, "TenantIdentity", "GetTenantIdentityConfiguration")
		}

		code, unwrapped := common.UnwrapWorkflowError(err)
		logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to get Tenant Identity Config")
		return cutil.NewAPIErrorResponse(c, code, "Failed to get Tenant Identity Config", nil)
	}

	apiConfig := &model.APITenantIdentityConfig{}
	apiConfig.FromResponseProto(&protoResponse)
	return c.JSON(http.StatusOK, apiConfig)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteTenantIdentityConfigHandler handles DELETE /tenant-identity/config.
type DeleteTenantIdentityConfigHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewDeleteTenantIdentityConfigHandler returns a new DeleteTenantIdentityConfigHandler.
func NewDeleteTenantIdentityConfigHandler(dbSession *cdb.Session, scp *sc.ClientPool) DeleteTenantIdentityConfigHandler {
	return DeleteTenantIdentityConfigHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete Tenant Identity Config
// @Description Remove the per-org tenant identity configuration and signing keys. In-flight tokens remain valid until natural expiry.
// @Tags TenantIdentity
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteID path string true "ID of Site"
// @Success 204
// @Router /v2/org/{org}/nico/site/{siteID}/tenant-identity/config [delete]
func (dmich DeleteTenantIdentityConfigHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantIdentity", "Delete", c, dmich.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	siteID := c.Param("siteID")
	if siteID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Missing siteID path parameter", nil)
	}

	if ok, err := auth.ValidateOrgMembership(dbUser, org); !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}
	if ok := auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole); !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	if _, err := common.GetTenantForOrg(ctx, nil, dmich.dbSession, org); err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, siteID, dmich.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) || errors.Is(err, common.ErrInvalidID) {
			logger.Warn().Err(err).Str("Site ID", siteID).Msg("site not found in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site with ID specified in request data", nil)
		}
		logger.Error().Err(err).Str("Site ID", siteID).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	temporalClient, err := dmich.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	protoRequest := &corev1.GetTenantIdentityConfigRequest{OrganizationId: org}

	workflowOptions := tclient.StartWorkflowOptions{
		ID:                       "tenant-identity-config-delete-" + org + "-" + site.ID.String(),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
		WorkflowIDConflictPolicy: temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	}

	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	we, err := temporalClient.ExecuteWorkflow(ctx, workflowOptions, "DeleteTenantIdentityConfiguration", protoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to delete Tenant Identity Config")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to start workflow to delete Tenant Identity Config", nil)
	}

	wid := we.GetID()
	logger.Info().Str("Workflow ID", wid).Msg("executed synchronous delete Tenant Identity Config workflow")

	err = we.Get(ctx, nil)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || err == context.DeadlineExceeded || ctx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, wid, err, "TenantIdentity", "DeleteTenantIdentityConfiguration")
		}

		code, unwrapped := common.UnwrapWorkflowError(err)
		logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to delete Tenant Identity Config")
		return cutil.NewAPIErrorResponse(c, code, "Failed to delete Tenant Identity Config", nil)
	}

	return c.NoContent(http.StatusNoContent)
}

// ~~~~~ Create Or Update Token Delegation Handler ~~~~~ //

// CreateOrUpdateTenantIdentityTokenDelegationHandler handles PUT /tenant-identity/token-delegation.
type CreateOrUpdateTenantIdentityTokenDelegationHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewCreateOrUpdateTenantIdentityTokenDelegationHandler returns a new CreateOrUpdateTenantIdentityTokenDelegationHandler.
func NewCreateOrUpdateTenantIdentityTokenDelegationHandler(dbSession *cdb.Session, scp *sc.ClientPool) CreateOrUpdateTenantIdentityTokenDelegationHandler {
	return CreateOrUpdateTenantIdentityTokenDelegationHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create Or Update Token Delegation
// @Description Register an RFC 8693 token exchange callback URL for the org. Requires tenant-identity/config to exist first.
// @Tags TenantIdentity
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteID path string true "ID of Site"
// @Param message body model.APITenantIdentityTokenDelegationCreateOrUpdateRequest true "Token Delegation create/update request"
// @Success 201 {object} model.APITenantIdentityTokenDelegation "Token delegation created on first call"
// @Success 200 {object} model.APITenantIdentityTokenDelegation "Token delegation replaced/updated"
// @Failure 503 {object} util.APIError
// @Router /v2/org/{org}/nico/site/{siteID}/tenant-identity/token-delegation [put]
func (utdh CreateOrUpdateTenantIdentityTokenDelegationHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantIdentityTokenDelegation", "CreateOrUpdate", c, utdh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	siteID := c.Param("siteID")
	if siteID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Missing siteID path parameter", nil)
	}

	if ok, err := auth.ValidateOrgMembership(dbUser, org); !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}
	if ok := auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole); !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	if _, err := common.GetTenantForOrg(ctx, nil, utdh.dbSession, org); err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, siteID, utdh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) || errors.Is(err, common.ErrInvalidID) {
			logger.Warn().Err(err).Str("Site ID", siteID).Msg("site not found in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site with ID specified in request data", nil)
		}
		logger.Error().Err(err).Str("Site ID", siteID).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	temporalClient, err := utdh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	apiRequest := model.APITenantIdentityTokenDelegationCreateOrUpdateRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating Token Delegation update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Token Delegation create/update request data", verr)
	}

	protoRequest := apiRequest.ToProto(org)

	hash, err := payloadHash(protoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to hash request payload for workflow ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to hash request payload", nil)
	}
	workflowOptions := tclient.StartWorkflowOptions{
		ID:                       "tenant-identity-token-delegation-create-or-update-" + org + "-" + site.ID.String() + "-" + hash,
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
		WorkflowIDConflictPolicy: temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	}

	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	we, err := temporalClient.ExecuteWorkflow(ctx, workflowOptions, "CreateOrUpdateTenantIdentityTokenDelegation", protoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to create or update Token Delegation")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to start workflow to create or update Token Delegation", nil)
	}

	wid := we.GetID()
	logger.Info().Str("Workflow ID", wid).Msg("executed synchronous create or update Token Delegation workflow")

	var protoResponse corev1.TokenDelegationResponse
	err = we.Get(ctx, &protoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || err == context.DeadlineExceeded || ctx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, wid, err, "TenantIdentityTokenDelegation", "CreateOrUpdateTenantIdentityTokenDelegation")
		}

		code, unwrapped := common.UnwrapWorkflowError(err)
		logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to create or update Token Delegation")
		return cutil.NewAPIErrorResponse(c, code, "Failed to create or update Token Delegation", nil)
	}

	apiDelegation := &model.APITenantIdentityTokenDelegation{}
	apiDelegation.FromResponseProto(&protoResponse)
	status := http.StatusOK
	if apiDelegation.IsCreated() {
		status = http.StatusCreated
	}
	return c.JSON(status, apiDelegation)
}

// ~~~~~ Get Token Delegation Handler ~~~~~ //

// GetTenantIdentityTokenDelegationHandler handles GET /tenant-identity/token-delegation.
type GetTenantIdentityTokenDelegationHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewGetTenantIdentityTokenDelegationHandler returns a new GetTenantIdentityTokenDelegationHandler.
func NewGetTenantIdentityTokenDelegationHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetTenantIdentityTokenDelegationHandler {
	return GetTenantIdentityTokenDelegationHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve Token Delegation for an org
// @Description Retrieve the currently registered token exchange endpoint. The raw secret is never returned; only its SHA-256 hash.
// @Tags TenantIdentity
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteID path string true "ID of Site"
// @Success 200 {object} model.APITenantIdentityTokenDelegation
// @Router /v2/org/{org}/nico/site/{siteID}/tenant-identity/token-delegation [get]
func (gtdh GetTenantIdentityTokenDelegationHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantIdentityTokenDelegation", "Get", c, gtdh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	siteID := c.Param("siteID")
	if siteID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Missing siteID path parameter", nil)
	}

	if ok, err := auth.ValidateOrgMembership(dbUser, org); !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}
	if ok := auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole); !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	if _, err := common.GetTenantForOrg(ctx, nil, gtdh.dbSession, org); err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, siteID, gtdh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) || errors.Is(err, common.ErrInvalidID) {
			logger.Warn().Err(err).Str("Site ID", siteID).Msg("site not found in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site with ID specified in request data", nil)
		}
		logger.Error().Err(err).Str("Site ID", siteID).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	temporalClient, err := gtdh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	protoRequest := &corev1.GetTokenDelegationRequest{OrganizationId: org}

	workflowOptions := tclient.StartWorkflowOptions{
		ID:                       "tenant-identity-token-delegation-get-" + org + "-" + site.ID.String(),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
		WorkflowIDConflictPolicy: temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	}

	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	we, err := temporalClient.ExecuteWorkflow(ctx, workflowOptions, "GetTenantIdentityTokenDelegation", protoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to get Token Delegation")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to start workflow to get Token Delegation", nil)
	}

	wid := we.GetID()
	logger.Info().Str("Workflow ID", wid).Msg("executed synchronous get Token Delegation workflow")

	var protoResponse corev1.TokenDelegationResponse
	err = we.Get(ctx, &protoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || err == context.DeadlineExceeded || ctx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, wid, err, "TenantIdentityTokenDelegation", "GetTenantIdentityTokenDelegation")
		}

		code, unwrapped := common.UnwrapWorkflowError(err)
		logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to get Token Delegation")
		return cutil.NewAPIErrorResponse(c, code, "Failed to get Token Delegation", nil)
	}

	apiDelegation := &model.APITenantIdentityTokenDelegation{}
	apiDelegation.FromResponseProto(&protoResponse)
	return c.JSON(http.StatusOK, apiDelegation)
}

// ~~~~~ Delete Token Delegation Handler ~~~~~ //

// DeleteTenantIdentityTokenDelegationHandler handles DELETE /tenant-identity/token-delegation.
type DeleteTenantIdentityTokenDelegationHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewDeleteTenantIdentityTokenDelegationHandler returns a new DeleteTenantIdentityTokenDelegationHandler.
func NewDeleteTenantIdentityTokenDelegationHandler(dbSession *cdb.Session, scp *sc.ClientPool) DeleteTenantIdentityTokenDelegationHandler {
	return DeleteTenantIdentityTokenDelegationHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete Token Delegation
// @Description Remove the RFC 8693 token exchange callback. Subsequent Instance Metadata Service requests revert to direct token issuance.
// @Tags TenantIdentity
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteID path string true "ID of Site"
// @Success 204
// @Router /v2/org/{org}/nico/site/{siteID}/tenant-identity/token-delegation [delete]
func (dtdh DeleteTenantIdentityTokenDelegationHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantIdentityTokenDelegation", "Delete", c, dtdh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	siteID := c.Param("siteID")
	if siteID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Missing siteID path parameter", nil)
	}

	if ok, err := auth.ValidateOrgMembership(dbUser, org); !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}
	if ok := auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole); !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	if _, err := common.GetTenantForOrg(ctx, nil, dtdh.dbSession, org); err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, siteID, dtdh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) || errors.Is(err, common.ErrInvalidID) {
			logger.Warn().Err(err).Str("Site ID", siteID).Msg("site not found in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site with ID specified in request data", nil)
		}
		logger.Error().Err(err).Str("Site ID", siteID).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	temporalClient, err := dtdh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	protoRequest := &corev1.GetTokenDelegationRequest{OrganizationId: org}

	workflowOptions := tclient.StartWorkflowOptions{
		ID:                       "tenant-identity-token-delegation-delete-" + org + "-" + site.ID.String(),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
		WorkflowIDConflictPolicy: temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	}

	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	we, err := temporalClient.ExecuteWorkflow(ctx, workflowOptions, "DeleteTenantIdentityTokenDelegation", protoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to delete Token Delegation")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to start workflow to delete Token Delegation", nil)
	}

	wid := we.GetID()
	logger.Info().Str("Workflow ID", wid).Msg("executed synchronous delete Token Delegation workflow")

	err = we.Get(ctx, nil)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || err == context.DeadlineExceeded || ctx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, wid, err, "TenantIdentityTokenDelegation", "DeleteTenantIdentityTokenDelegation")
		}

		code, unwrapped := common.UnwrapWorkflowError(err)
		logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to delete Token Delegation")
		return cutil.NewAPIErrorResponse(c, code, "Failed to delete Token Delegation", nil)
	}

	return c.NoContent(http.StatusNoContent)
}

// ~~~~~ Get JWKS Handler ~~~~~ //

// GetJWKSHandler handles GET /.well-known/jwks.json and the SPIFFE variant.
type GetJWKSHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
	kind       corev1.JwksKind
}

// NewGetJWKSHandler returns a new GetJWKSHandler.
func NewGetJWKSHandler(dbSession *cdb.Session, scp *sc.ClientPool, kind corev1.JwksKind) GetJWKSHandler {
	return GetJWKSHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
		kind:       kind,
	}
}

// Handle godoc
// @Summary Retrieve JWKS
// @Description Public JSON Web Key Set for JWT-SVID signature verification. No authentication required. Returns 404 when no identity configuration exists for this org/site.
// @Tags TenantIdentity
// @Produce json
// @Param org path string true "Name of NGC organization"
// @Param siteID path string true "ID of Site"
// @Success 200 {string} string "JWKS JSON document"
// @Router /v2/org/{org}/nico/site/{siteID}/.well-known/jwks.json [get]
func (gjwksh GetJWKSHandler) Handle(c echo.Context) error {
	ctx := c.Request().Context()
	org := strings.ToLower(c.Param("orgName"))
	siteID := c.Param("siteID")
	logger := zerolog.Ctx(ctx).With().
		Str("Model", "TenantIdentity").
		Str("Handler", "GetJWKS").
		Str("Org", org).
		Str("Site ID", siteID).
		Logger()

	if org == "" || siteID == "" {
		logger.Warn().Msg("missing orgName or siteID path parameter")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "JWKS not available", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, siteID, gjwksh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) || errors.Is(err, common.ErrInvalidID) {
			logger.Warn().Err(err).Str("Site ID", siteID).Msg("site not found in request")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "JWKS not available", nil)
		}
		logger.Error().Err(err).Str("Site ID", siteID).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	if _, err := common.GetTenantForOrg(ctx, nil, gjwksh.dbSession, org); err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Info().Msg("Org does not have a Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "JWKS not available", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for Org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	temporalClient, err := gjwksh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	kind := gjwksh.kind
	protoRequest := &corev1.JwksRequest{OrganizationId: org, Kind: &kind}

	workflowOptions := tclient.StartWorkflowOptions{
		ID:                       "tenant-identity-jwks-get-" + org + "-" + site.ID.String() + "-" + kind.String(),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
		WorkflowIDConflictPolicy: temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	}

	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	we, err := temporalClient.ExecuteWorkflow(ctx, workflowOptions, "GetJWKS", protoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to get JWKS")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to start workflow to get JWKS", nil)
	}

	wid := we.GetID()
	logger.Info().Str("Workflow ID", wid).Msg("executed synchronous get JWKS workflow")

	var protoResponse corev1.Jwks
	err = we.Get(ctx, &protoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || err == context.DeadlineExceeded || ctx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, wid, err, "TenantIdentity", "GetJWKS")
		}

		code, unwrapped := common.UnwrapWorkflowError(err)
		if code == http.StatusNotFound {
			logger.Info().Err(unwrapped).Str("orgName", org).Msg("public JWKS: Core gRPC API reported NOT_FOUND")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "JWKS not available", nil)
		}
		logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to get JWKS")
		return cutil.NewAPIErrorResponse(c, code, "Failed to get JWKS", nil)
	}

	raw := protoResponse.GetJwks()
	if strings.TrimSpace(raw) == "" {
		logger.Info().Str("orgName", org).
			Msg("public JWKS: Core gRPC API returned empty body")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "JWKS not available", nil)
	}

	var jwks model.APITenantIdentityJWKS
	if err := json.Unmarshal([]byte(raw), &jwks); err != nil || jwks.Keys == nil {
		logger.Error().Err(err).Str("orgName", org).
			Msg("public JWKS: Core gRPC API returned malformed JWKS")
		return cutil.NewAPIErrorResponse(c, http.StatusBadGateway, "Invalid JWKS retrieved from Site", nil)
	}

	return c.JSON(http.StatusOK, &jwks)
}

// ~~~~~ Get OpenID Configuration Handler ~~~~~ //

// GetOpenIDConfigurationHandler handles GET /.well-known/openid-configuration.
type GetOpenIDConfigurationHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewGetOpenIDConfigurationHandler returns a new GetOpenIDConfigurationHandler.
func NewGetOpenIDConfigurationHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetOpenIDConfigurationHandler {
	return GetOpenIDConfigurationHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve OpenID Configuration
// @Description Public OIDC discovery document pointing at this org's JWKS URIs. No authentication required. Returns 404 when no identity material exists for this org/site.
// @Tags TenantIdentity
// @Produce json
// @Param org path string true "Name of NGC organization"
// @Param siteID path string true "ID of Site"
// @Success 200 {object} model.APIOpenIDConfiguration
// @Router /v2/org/{org}/nico/site/{siteID}/.well-known/openid-configuration [get]
func (goidch GetOpenIDConfigurationHandler) Handle(c echo.Context) error {
	ctx := c.Request().Context()
	org := strings.ToLower(c.Param("orgName"))
	siteID := c.Param("siteID")
	logger := zerolog.Ctx(ctx).With().
		Str("Model", "TenantIdentity").
		Str("Handler", "GetOpenIDConfiguration").
		Str("Org", org).
		Str("Site ID", siteID).
		Logger()

	if org == "" || siteID == "" {
		logger.Warn().Msg("missing orgName or siteID path parameter")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "OpenID Configuration not available", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, siteID, goidch.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) || errors.Is(err, common.ErrInvalidID) {
			logger.Warn().Err(err).Str("Site ID", siteID).Msg("site not found in request")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "OpenID Configuration not available", nil)
		}
		logger.Error().Err(err).Str("Site ID", siteID).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	if _, err := common.GetTenantForOrg(ctx, nil, goidch.dbSession, org); err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Info().Msg("Org does not have a Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "OpenID Configuration not available", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for Org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	temporalClient, err := goidch.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	protoRequest := &corev1.OpenIdConfigRequest{OrganizationId: org}

	workflowOptions := tclient.StartWorkflowOptions{
		ID:                       "tenant-identity-openid-configuration-get-" + org + "-" + site.ID.String(),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
		WorkflowIDConflictPolicy: temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	}

	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	we, err := temporalClient.ExecuteWorkflow(ctx, workflowOptions, "GetOpenIDConfiguration", protoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to get OpenID Configuration")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to start workflow to get OpenID Configuration", nil)
	}

	wid := we.GetID()
	logger.Info().Str("Workflow ID", wid).Msg("executed synchronous get OpenID Configuration workflow")

	var protoResponse corev1.OpenIdConfiguration
	err = we.Get(ctx, &protoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || err == context.DeadlineExceeded || ctx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, wid, err, "TenantIdentity", "GetOpenIDConfiguration")
		}

		code, unwrapped := common.UnwrapWorkflowError(err)
		logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to get OpenID Configuration")
		return cutil.NewAPIErrorResponse(c, code, "Failed to get OpenID Configuration", nil)
	}

	apiOpenIDConf := &model.APIOpenIDConfiguration{}
	apiOpenIDConf.FromResponseProto(&protoResponse)
	return c.JSON(http.StatusOK, apiOpenIDConf)
}

// ~~~~~ Reencrypt Secrets Handler ~~~~~ //

// ReencryptTenantIdentitySecretsHandler handles POST /tenant-identity/reencrypt.
type ReencryptTenantIdentitySecretsHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewReencryptTenantIdentitySecretsHandler returns a new ReencryptTenantIdentitySecretsHandler.
func NewReencryptTenantIdentitySecretsHandler(dbSession *cdb.Session, scp *sc.ClientPool) ReencryptTenantIdentitySecretsHandler {
	return ReencryptTenantIdentitySecretsHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Reencrypt Tenant Identity Secrets
// @Description Re-wrap tenant_identity_config ciphertext with the site's current master encryption key (KEK rotation). Provider-admin scoped; the URL org identifies the provider. Omit organizationId to target all orgs, or set it to a tenant org that has an allocation and tenant identity configuration on the Site.
// @Tags TenantIdentity
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of provider organization"
// @Param siteID path string true "ID of target Site"
// @Param message body model.APIReencryptTenantIdentitySecretsRequest true "Reencrypt Tenant Identity Secrets request"
// @Success 200 {object} model.APIReencryptTenantIdentitySecretsResponse
// @Failure 503 {object} util.APIError
// @Router /v2/org/{org}/nico/site/{siteID}/tenant-identity/reencrypt [post]
func (rtish ReencryptTenantIdentitySecretsHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantIdentity", "ReencryptSecrets", c, rtish.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	siteID := c.Param("siteID")
	if siteID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Missing siteID path parameter", nil)
	}

	apiRequest := model.APIReencryptTenantIdentitySecretsRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	validationErr := apiRequest.Validate()
	if validationErr != nil {
		logger.Warn().Err(validationErr).Msg("error validating Reencrypt Tenant Identity Secrets request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Reencrypt Tenant Identity Secrets request data", validationErr)
	}

	temporalClient, resolvedSiteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: rtish.dbSession,
		SCP:       rtish.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    siteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	if apiRequest.OrganizationID != nil {
		tenant, err := common.GetTenantForOrg(ctx, nil, rtish.dbSession, *apiRequest.OrganizationID)
		if err != nil {
			if errors.Is(err, common.ErrOrgTenantNotFound) {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Tenant for organizationId specified in request data", nil)
			}
			logger.Error().Err(err).Str("Tenant Org", *apiRequest.OrganizationID).Msg("error retrieving Tenant for re-encryption scope")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
		}

		resolvedSiteUUID, err := uuid.Parse(resolvedSiteID)
		if err != nil {
			logger.Error().Err(err).Str("Site ID", resolvedSiteID).Msg("resolved Site has an invalid ID")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate Tenant access to Site", nil)
		}

		_, err = cdbm.NewTenantSiteDAO(rtish.dbSession).GetByTenantIDAndSiteID(ctx, nil, tenant.ID, resolvedSiteUUID, nil)
		if err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant organization does not have an allocation on the Site", nil)
			}
			logger.Error().Err(err).Str("Tenant Org", tenant.Org).Str("Site ID", resolvedSiteID).Msg("error retrieving Tenant Site association")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate Tenant access to Site", nil)
		}
	}

	var protoResponse corev1.ReencryptTenantIdentitySecretsResponse
	apiErr = common.ExecuteCoreGRPC(
		ctx,
		temporalClient,
		corev1.Forge_ReencryptTenantIdentitySecrets_FullMethodName,
		apiRequest.ToProto(),
		&protoResponse,
		resolvedSiteID,
	)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to reencrypt Tenant Identity secrets via Core proxy")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	apiResponse := &model.APIReencryptTenantIdentitySecretsResponse{}
	apiResponse.FromResponseProto(&protoResponse)
	return c.JSON(http.StatusOK, apiResponse)
}
