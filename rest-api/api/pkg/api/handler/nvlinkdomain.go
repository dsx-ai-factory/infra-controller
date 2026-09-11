// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"errors"
	"fmt"
	"net/http"
	"slices"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"go.opentelemetry.io/otel/attribute"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

type nvLinkDomainOperationWorkflowIdentity struct {
	SiteID                 string   `json:"siteId"`
	NVLinkDomainIDs        []string `json:"domainIds"`
	State                  string   `json:"state,omitempty"`
	Version                *string  `json:"version,omitempty"`
	RuleID                 *string  `json:"ruleId,omitempty"`
	OverrideReadinessCheck bool     `json:"overrideReadinessCheck,omitempty"`
}

func newNVLinkDomainOperationWorkflowIdentity(
	siteID string,
	nvLinkDomainIDs []string,
	ruleID *string,
) (nvLinkDomainOperationWorkflowIdentity, error) {
	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		return nvLinkDomainOperationWorkflowIdentity{}, errors.New("site ID must be a UUID")
	}

	canonicalNVLinkDomainIDs := make([]string, len(nvLinkDomainIDs))
	for i, nvLinkDomainID := range nvLinkDomainIDs {
		parsedNVLinkDomainID, err := uuid.Parse(nvLinkDomainID)
		if err != nil || parsedNVLinkDomainID == uuid.Nil {
			return nvLinkDomainOperationWorkflowIdentity{}, fmt.Errorf("NVLink Domain ID at index %d must be a non-zero UUID", i)
		}
		canonicalNVLinkDomainIDs[i] = parsedNVLinkDomainID.String()
	}
	slices.Sort(canonicalNVLinkDomainIDs)

	var canonicalRuleID *string
	if ruleID != nil && *ruleID != "" {
		parsedRuleID, err := uuid.Parse(*ruleID)
		if err != nil {
			return nvLinkDomainOperationWorkflowIdentity{}, errors.New("rule ID must be a UUID")
		}
		canonical := parsedRuleID.String()
		canonicalRuleID = &canonical
	}

	return nvLinkDomainOperationWorkflowIdentity{
		SiteID:          parsedSiteID.String(),
		NVLinkDomainIDs: canonicalNVLinkDomainIDs,
		RuleID:          canonicalRuleID,
	}, nil
}

func (identity nvLinkDomainOperationWorkflowIdentity) powerWorkflowID(
	state string,
	overrideReadinessCheck bool,
) string {
	identity.State = state
	identity.OverrideReadinessCheck = overrideReadinessCheck

	return fmt.Sprintf("nvlink-domain-power-state-update-%s-%s", state, common.RequestHash(identity))
}

func (identity nvLinkDomainOperationWorkflowIdentity) firmwareWorkflowID(
	version *string,
	overrideReadinessCheck bool,
) string {
	if version != nil && *version == "" {
		version = nil
	}
	identity.Version = version
	identity.OverrideReadinessCheck = overrideReadinessCheck

	return fmt.Sprintf("nvlink-domain-firmware-update-%s", common.RequestHash(identity))
}

// UpdateNVLinkDomainPowerStateHandler power controls one NVLink Domain identified by UUID.
type UpdateNVLinkDomainPowerStateHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewUpdateNVLinkDomainPowerStateHandler initializes an NVLink Domain power-control handler.
func NewUpdateNVLinkDomainPowerStateHandler(
	dbSession *cdb.Session,
	scp *sc.ClientPool,
) UpdateNVLinkDomainPowerStateHandler {
	return UpdateNVLinkDomainPowerStateHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Power control an NVLink Domain
// @Description Power control an NVLink Domain identified by UUID.
// @Tags nvlink-domain
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the NVLink Domain"
// @Param body body model.APIUpdatePowerStateRequest true "NVLink Domain power control request"
// @Success 200 {object} model.APIUpdatePowerStateResponse
// @Router /v2/org/{org}/nico/domain/nvlink/{id}/power [patch]
func (h UpdateNVLinkDomainPowerStateHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkDomain", "PowerControl", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	nvLinkDomainID := c.Param("id")
	if err := model.ValidateNVLinkDomainID(nvLinkDomainID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	request := model.APIUpdatePowerStateRequest{}
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := request.Validate(); err != nil {
		logger.Warn().Err(err).Msg("error validating NVLink Domain power control request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate NVLink Domain power control request", err)
	}

	identity, err := newNVLinkDomainOperationWorkflowIdentity(request.SiteID, []string{nvLinkDomainID}, request.RuleID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	cotel.SetAttribute(handlerSpan, attribute.String("nvlink_domain_id", identity.NVLinkDomainIDs[0]))

	site, err := common.GetSiteFromIDString(ctx, nil, identity.SiteID, h.dbSession)
	if err != nil {
		switch {
		case errors.Is(err, common.ErrInvalidID):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		case errors.Is(err, cdb.ErrDoesNotExist):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		default:
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
		}
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state, unable to execute operation on Site", nil)
	}

	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowResp, err := common.ExecutePowerControlWorkflow(
		ctx,
		c,
		logger,
		stc,
		model.NVLinkDomainTargetSpec(identity.NVLinkDomainIDs),
		request.State,
		identity.RuleID,
		request.OverrideReadinessCheck,
		identity.powerWorkflowID(request.State, request.OverrideReadinessCheck),
		"NVLink Domain",
	)
	if err != nil {
		return err
	}
	if c.Response().Committed {
		return nil
	}

	logger.Info().Str("State", request.State).Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdatePowerStateResponse(flowResp))
}

// BatchUpdateNVLinkDomainPowerStateHandler power controls one or more NVLink Domains.
type BatchUpdateNVLinkDomainPowerStateHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewBatchUpdateNVLinkDomainPowerStateHandler initializes a batch NVLink Domain
// power-control handler.
func NewBatchUpdateNVLinkDomainPowerStateHandler(
	dbSession *cdb.Session,
	scp *sc.ClientPool,
) BatchUpdateNVLinkDomainPowerStateHandler {
	return BatchUpdateNVLinkDomainPowerStateHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Power control NVLink Domains
// @Description Power control one or more NVLink Domains identified by UUID.
// @Tags nvlink-domain
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param body body model.APIBatchUpdateNVLinkDomainPowerStateRequest true "Batch NVLink Domain power control request"
// @Success 200 {object} model.APIUpdatePowerStateResponse
// @Router /v2/org/{org}/nico/domain/nvlink/power [patch]
func (h BatchUpdateNVLinkDomainPowerStateHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkDomain", "PowerControlBatch", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	request := model.APIBatchUpdateNVLinkDomainPowerStateRequest{}
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := request.Validate(); err != nil {
		logger.Warn().Err(err).Msg("error validating batch NVLink Domain power control request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate batch NVLink Domain power control request", err)
	}
	identity, err := newNVLinkDomainOperationWorkflowIdentity(request.SiteID, request.NVLinkDomainIDs, request.RuleID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	cotel.SetAttribute(handlerSpan, attribute.StringSlice("nvlink_domain_ids", identity.NVLinkDomainIDs))

	site, err := common.GetSiteFromIDString(ctx, nil, identity.SiteID, h.dbSession)
	if err != nil {
		switch {
		case errors.Is(err, common.ErrInvalidID):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		case errors.Is(err, cdb.ErrDoesNotExist):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		default:
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
		}
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state, unable to execute operation on Site", nil)
	}

	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowResp, err := common.ExecutePowerControlWorkflow(
		ctx,
		c,
		logger,
		stc,
		model.NVLinkDomainTargetSpec(identity.NVLinkDomainIDs),
		request.State,
		identity.RuleID,
		request.OverrideReadinessCheck,
		identity.powerWorkflowID(request.State, request.OverrideReadinessCheck),
		"NVLink Domains",
	)
	if err != nil {
		return err
	}
	if c.Response().Committed {
		return nil
	}

	logger.Info().Str("State", request.State).Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdatePowerStateResponse(flowResp))
}

// UpdateNVLinkDomainFirmwareHandler updates firmware on one NVLink Domain identified by
// UUID.
type UpdateNVLinkDomainFirmwareHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewUpdateNVLinkDomainFirmwareHandler initializes an NVLink Domain firmware-update handler.
func NewUpdateNVLinkDomainFirmwareHandler(
	dbSession *cdb.Session,
	scp *sc.ClientPool,
) UpdateNVLinkDomainFirmwareHandler {
	return UpdateNVLinkDomainFirmwareHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Firmware update an NVLink Domain
// @Description Update firmware on an NVLink Domain identified by UUID.
// @Tags nvlink-domain
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the NVLink Domain"
// @Param body body model.APINVLinkDomainFirmwareUpdateRequest true "NVLink Domain firmware update request"
// @Success 200 {object} model.APIUpdateFirmwareResponse
// @Router /v2/org/{org}/nico/domain/nvlink/{id}/firmware [patch]
func (h UpdateNVLinkDomainFirmwareHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkDomain", "FirmwareUpdate", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	nvLinkDomainID := c.Param("id")
	if err := model.ValidateNVLinkDomainID(nvLinkDomainID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	request := model.APINVLinkDomainFirmwareUpdateRequest{}
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := request.Validate(); err != nil {
		logger.Warn().Err(err).Msg("error validating NVLink Domain firmware update request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate NVLink Domain firmware update request", err)
	}
	if request.Version != nil && *request.Version == "" {
		request.Version = nil
	}

	identity, err := newNVLinkDomainOperationWorkflowIdentity(request.SiteID, []string{nvLinkDomainID}, request.RuleID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	cotel.SetAttribute(handlerSpan, attribute.String("nvlink_domain_id", identity.NVLinkDomainIDs[0]))

	site, err := common.GetSiteFromIDString(ctx, nil, identity.SiteID, h.dbSession)
	if err != nil {
		switch {
		case errors.Is(err, common.ErrInvalidID):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		case errors.Is(err, cdb.ErrDoesNotExist):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		default:
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
		}
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state, unable to execute operation on Site", nil)
	}

	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowResp, err := common.ExecuteFirmwareUpdateWorkflow(
		ctx,
		c,
		logger,
		stc,
		model.NVLinkDomainTargetSpec(identity.NVLinkDomainIDs),
		request.Version,
		nil,
		nil,
		identity.SiteID,
		identity.RuleID,
		request.OverrideReadinessCheck,
		identity.firmwareWorkflowID(request.Version, request.OverrideReadinessCheck),
		"NVLink Domain",
	)
	if err != nil {
		return err
	}
	if c.Response().Committed {
		return nil
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdateFirmwareResponse(flowResp))
}

// BatchUpdateNVLinkDomainFirmwareHandler updates firmware on one or more NVLink Domains.
type BatchUpdateNVLinkDomainFirmwareHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewBatchUpdateNVLinkDomainFirmwareHandler initializes a batch NVLink Domain
// firmware-update handler.
func NewBatchUpdateNVLinkDomainFirmwareHandler(
	dbSession *cdb.Session,
	scp *sc.ClientPool,
) BatchUpdateNVLinkDomainFirmwareHandler {
	return BatchUpdateNVLinkDomainFirmwareHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Firmware update NVLink Domains
// @Description Update firmware on one or more NVLink Domains identified by UUID.
// @Tags nvlink-domain
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param body body model.APIBatchNVLinkDomainFirmwareUpdateRequest true "Batch NVLink Domain firmware update request"
// @Success 200 {object} model.APIUpdateFirmwareResponse
// @Router /v2/org/{org}/nico/domain/nvlink/firmware [patch]
func (h BatchUpdateNVLinkDomainFirmwareHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkDomain", "FirmwareUpdateBatch", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	request := model.APIBatchNVLinkDomainFirmwareUpdateRequest{}
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := request.Validate(); err != nil {
		logger.Warn().Err(err).Msg("error validating batch NVLink Domain firmware update request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate batch NVLink Domain firmware update request", err)
	}
	if request.Version != nil && *request.Version == "" {
		request.Version = nil
	}
	identity, err := newNVLinkDomainOperationWorkflowIdentity(request.SiteID, request.NVLinkDomainIDs, request.RuleID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	cotel.SetAttribute(handlerSpan, attribute.StringSlice("nvlink_domain_ids", identity.NVLinkDomainIDs))

	site, err := common.GetSiteFromIDString(ctx, nil, identity.SiteID, h.dbSession)
	if err != nil {
		switch {
		case errors.Is(err, common.ErrInvalidID):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		case errors.Is(err, cdb.ErrDoesNotExist):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		default:
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
		}
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state, unable to execute operation on Site", nil)
	}

	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowResp, err := common.ExecuteFirmwareUpdateWorkflow(
		ctx,
		c,
		logger,
		stc,
		model.NVLinkDomainTargetSpec(identity.NVLinkDomainIDs),
		request.Version,
		nil,
		nil,
		identity.SiteID,
		identity.RuleID,
		request.OverrideReadinessCheck,
		identity.firmwareWorkflowID(request.Version, request.OverrideReadinessCheck),
		"NVLink Domains",
	)
	if err != nil {
		return err
	}
	if c.Response().Committed {
		return nil
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdateFirmwareResponse(flowResp))
}
