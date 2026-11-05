// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"errors"
	"fmt"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	temporalEnums "go.temporal.io/api/enums/v1"
	"net/http"
)

type resetScope string

const resetTray resetScope = "tray"
const resetDomain resetScope = "domain"

// NVOSFactoryResetHandler shares authorization and site checks for tray/domain
// reset. Scope is fixed by route construction, never taken from request data.
type NVOSFactoryResetHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	scope     resetScope
}

func NewResetTrayNVOSHandler(dbSession *cdb.Session, scp *sc.ClientPool) NVOSFactoryResetHandler {
	return NVOSFactoryResetHandler{dbSession: dbSession, scp: scp, scope: resetTray}
}
func NewResetNVLinkDomainNVOSHandler(dbSession *cdb.Session, scp *sc.ClientPool) NVOSFactoryResetHandler {
	return NVOSFactoryResetHandler{dbSession: dbSession, scp: scp, scope: resetDomain}
}

// Handle authorizes a Provider Admin, then submits through the existing Flow
// proxy. A 202 means tasks were created; GET task observes terminal outcomes.
func (h NVOSFactoryResetHandler) Handle(c echo.Context) error {
	org, user, ctx, logger, span := common.SetupHandler("NVOS", "FactoryReset", c)
	if span != nil {
		defer span.End()
	}
	if user == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	member, _ := auth.ValidateOrgMembership(user, org)
	if !member || !auth.ValidateUserRoles(user, org, nil, auth.ProviderAdminRole) {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Provider Admin membership is required", nil)
	}
	var request model.APINVOSFactoryResetRequest
	err := c.Bind(&request)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	err = request.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid reset request", err)
	}
	provider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}
	site, err := common.GetSiteFromIDString(ctx, nil, request.SiteID, h.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) || errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist or is invalid", nil)
		}
		logger.Error().Err(err).Msg("failed to retrieve reset site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site", nil)
	}
	if site.InfrastructureProviderID != provider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site does not belong to current org's Provider", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site must be Registered", nil)
	}
	target, err := resetTarget(h.scope, c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	client, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}
	var response flowv1.SubmitTaskResponse
	proxyErr := common.ProxyFlowGRPC(ctx, logger, client, flowv1.Flow_ResetSwitches_FullMethodName,
		&flowv1.ResetSwitchesRequest{TargetSpec: target, Description: "API NVOS factory reset"}, &response,
		fmt.Sprintf("nvos-factory-reset-%s-%s", h.scope, uuid.NewString()), temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}
	if len(response.TaskIds) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadGateway, "Flow returned no reset tasks", nil)
	}
	result := &model.APINVOSFactoryResetResponse{}
	result.FromProto(&response)
	return c.JSON(http.StatusAccepted, result)
}

// resetTarget restricts Domain expansion to switches; Core IDs for trays are
// opaque, while domain IDs use the existing domain API validation.
func resetTarget(scope resetScope, id string) (*flowv1.OperationTargetSpec, error) {
	if strings.TrimSpace(id) == "" {
		return nil, fmt.Errorf("target ID is required")
	}
	switch scope {
	case resetDomain:
		err := model.ValidateNVLinkDomainID(id)
		if err != nil {
			return nil, err
		}
		target := model.NVLinkDomainTargetSpec([]string{id})
		target.GetNvlDomains().Targets[0].ComponentTypes = []flowv1.ComponentType{flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH}
		return target, nil
	case resetTray:
		return &flowv1.OperationTargetSpec{Targets: &flowv1.OperationTargetSpec_Components{Components: &flowv1.ComponentTargets{Targets: []*flowv1.ComponentTarget{{Identifier: &flowv1.ComponentTarget_External{External: &flowv1.ExternalRef{Id: id}}}}}}}, nil
	default:
		return nil, fmt.Errorf("unsupported reset scope")
	}
}
