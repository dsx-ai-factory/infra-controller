// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/url"
	"slices"
	"unicode/utf8"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	tclient "go.temporal.io/sdk/client"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// vpcRoutingProfileHandler provides shared dependencies and provider authorization
// for routing operations; ordinary VPC PATCH remains a separate tenant operation.
type vpcRoutingProfileHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// newVPCRoutingProfileHandler initializes the dependencies shared by routing handlers.
func newVPCRoutingProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) vpcRoutingProfileHandler {
	return vpcRoutingProfileHandler{dbSession: dbSession, scp: scp}
}

// GetVPCRoutingProfileHandler reads authoritative VPC routing and allocation state.
type GetVPCRoutingProfileHandler struct {
	vpcRoutingProfileHandler
}

// NewGetVPCRoutingProfileHandler returns a provider-only routing inspection handler.
func NewGetVPCRoutingProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetVPCRoutingProfileHandler {
	return GetVPCRoutingProfileHandler{newVPCRoutingProfileHandler(dbSession, scp)}
}

// Handle godoc
// @Summary Get VPC Routing Profile
// @Description Inspect persisted routing and allocations; this does not verify dataplane convergence.
// @Tags vpc
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of provider organization"
// @Param vpcId path string true "ID of VPC"
// @Success 200 {object} model.APIVpcRoutingState
// @Router /v2/org/{org}/nico/vpc/{vpcId}/routing-profile [get]
func (h GetVPCRoutingProfileHandler) Handle(c echo.Context) error {
	org, user, ctx, logger, span := common.SetupHandler("VPCRoutingProfile", "Get", c)
	if span != nil {
		defer span.End()
	}

	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	vpc, stc, apiErr := h.authorize(ctx, logger, org, user, c.Param("id"))
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	state, apiErr := getVPCRoutingState(ctx, stc, vpc.GetSiteID().String())
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to inspect VPC routing state")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	state.VpcID = vpc.ID.String()
	return c.JSON(http.StatusOK, state)
}

// UpdateVPCRoutingProfileHandler changes a profile with one internally observed version.
type UpdateVPCRoutingProfileHandler struct {
	vpcRoutingProfileHandler
}

// NewUpdateVPCRoutingProfileHandler returns a provider-only routing update handler.
func NewUpdateVPCRoutingProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) UpdateVPCRoutingProfileHandler {
	return UpdateVPCRoutingProfileHandler{newVPCRoutingProfileHandler(dbSession, scp)}
}

// Handle godoc
// @Summary Update VPC Routing Profile
// @Description Change the profile and active VNI, retaining the previous allocation until explicit release.
// @Tags vpc
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of provider organization"
// @Param vpcId path string true "ID of VPC"
// @Param request body model.APIVpcRoutingProfileUpdateRequest true "Destination profile and optional VNI"
// @Success 200 {object} model.APIVpcRoutingState
// @Router /v2/org/{org}/nico/vpc/{vpcId}/routing-profile [patch]
func (h UpdateVPCRoutingProfileHandler) Handle(c echo.Context) error {
	org, user, ctx, logger, span := common.SetupHandler("VPCRoutingProfile", "Update", c)
	if span != nil {
		defer span.End()
	}

	// Use one deadline for inspection and update so the second Core call
	// cannot restart the caller's timeout.
	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// First, authorize the provider administrator and resolve the VPC's site client.
	vpc, stc, apiErr := h.authorize(ctx, logger, org, user, c.Param("id"))
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	// Next, bind and validate the destination profile and optional VNI.
	var req model.APIVpcRoutingProfileUpdateRequest
	err := c.Bind(&req)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid request body", nil)
	}
	err = req.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	req.VpcID = vpc.GetSiteID().String()

	// Read the current state from Core, rather than REST inventory, and use
	// that version to guard against another operator changing the VPC.
	observed, apiErr := getVPCRoutingState(ctx, stc, req.VpcID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to inspect VPC before routing profile update")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	req.IfVersionMatch = observed.Version
	if ctx.Err() != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusGatewayTimeout, "Request expired before VPC routing profile update was submitted", nil)
	}

	// Now submit one change using the observed version. Retrying with a newer
	// version could overwrite another operator's change.
	var raw corev1.VpcRoutingState
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_ChangeVpcRoutingProfile_FullMethodName, req.ToProto(), &raw, "")
	if apiErr != nil {
		return vpcRoutingMutationError(c, logger, apiErr)
	}

	// Check that Core acknowledged the requested change before reporting success.
	err = req.ValidateResponse(&raw, observed)
	if err != nil {
		return vpcRoutingMutationError(c, logger, cutil.NewAPIError(http.StatusInternalServerError, "Invalid VPC routing profile update response", err))
	}

	// Finally, return the committed state using the caller's REST VPC ID.
	// This acknowledges the update, not network convergence.
	var state model.APIVpcRoutingState
	state.FromProto(&raw)
	logger.Info().Str("vpc_id", vpc.ID.String()).Str("site_id", vpc.SiteID.String()).Uint32("active_vni", state.ActiveVni).Msg("VPC routing profile update committed")
	state.VpcID = vpc.ID.String()
	return c.JSON(http.StatusOK, state)
}

// ReleaseVPCInactiveVniHandler releases an allocation after operator convergence checks.
type ReleaseVPCInactiveVniHandler struct {
	vpcRoutingProfileHandler
}

// NewReleaseVPCInactiveVniHandler returns a provider-only inactive VNI release handler.
func NewReleaseVPCInactiveVniHandler(dbSession *cdb.Session, scp *sc.ClientPool) ReleaseVPCInactiveVniHandler {
	return ReleaseVPCInactiveVniHandler{newVPCRoutingProfileHandler(dbSession, scp)}
}

// Handle godoc
// @Summary Release VPC Inactive VNI
// @Description Release the exact inactive allocation after independently verifying all affected consumers.
// @Tags vpc
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of provider organization"
// @Param vpcId path string true "ID of VPC"
// @Param request body model.APIVpcInactiveVniReleaseRequest true "Originally observed version and inactive VNI"
// @Success 200 {object} model.APIVpcInactiveVniReleaseResult
// @Router /v2/org/{org}/nico/vpc/{vpcId}/routing-profile/release-inactive-vni [post]
func (h ReleaseVPCInactiveVniHandler) Handle(c echo.Context) error {
	org, user, ctx, logger, span := common.SetupHandler("VPCRoutingProfile", "ReleaseInactiveVni", c)
	if span != nil {
		defer span.End()
	}

	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// First, authorize the provider administrator and resolve the VPC's site client.
	vpc, stc, apiErr := h.authorize(ctx, logger, org, user, c.Param("id"))
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	// Next, bind and validate the operator's original version and exact inactive VNI.
	var req model.APIVpcInactiveVniReleaseRequest
	err := c.Bind(&req)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid request body", nil)
	}
	err = req.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	req.VpcID = vpc.GetSiteID().String()

	// Now release the allocation using the version the operator checked.
	// Do not replace it with a fresh observation or retry an uncertain release.
	if ctx.Err() != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusGatewayTimeout, "Request expired before inactive VNI release was submitted", nil)
	}
	var raw corev1.VpcReleaseInactiveVniResult
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_ReleaseVpcInactiveVni_FullMethodName, req.ToProto(), &raw, "")
	if apiErr != nil {
		return vpcRoutingMutationError(c, logger, apiErr)
	}

	// Check that Core released the requested VNI and advanced the VPC version.
	err = req.ValidateResponse(&raw)
	if err != nil {
		return vpcRoutingMutationError(c, logger, cutil.NewAPIError(http.StatusInternalServerError, "Invalid inactive VNI release response", err))
	}

	// Finally, return the release acknowledgement using the caller's REST VPC ID.
	var result model.APIVpcInactiveVniReleaseResult
	result.FromProto(&raw)
	logger.Info().Str("vpc_id", vpc.ID.String()).Str("site_id", vpc.SiteID.String()).Uint32("released_inactive_vni", result.ReleasedInactiveVni).Msg("VPC inactive VNI released")
	result.VpcID = vpc.ID.String()
	return c.JSON(http.StatusOK, result)
}

// authorize requires a provider administrator and verifies that both the VPC and
// its registered site belong to that provider. It returns the VPC and its site's
// Temporal client for the Core calls that follow.
func (h vpcRoutingProfileHandler) authorize(ctx context.Context, logger zerolog.Logger, org string, user *cdbm.User, vpcID string) (*cdbm.Vpc, tclient.Client, *cutil.APIError) {
	if user == nil {
		return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	provider, apiErr := common.IsProvider(ctx, logger, h.dbSession, org, user, false)
	if apiErr != nil {
		return nil, nil, apiErr
	}
	vpc, err := common.GetVpcFromIDString(ctx, nil, vpcID, []string{cdbm.SiteRelationName}, h.dbSession)
	if err != nil {
		switch {
		case errors.Is(err, common.ErrInvalidID):
			return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Invalid VPC ID in request", nil)
		case errors.Is(err, cdb.ErrDoesNotExist):
			return nil, nil, cutil.NewAPIError(http.StatusNotFound, "Could not find VPC with specified ID", nil)
		default:
			logger.Error().Err(err).Msg("failed to retrieve VPC for routing operation")
			return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve VPC", nil)
		}
	}
	if vpc.InfrastructureProviderID != provider.ID {
		return nil, nil, cutil.NewAPIError(http.StatusForbidden, "VPC does not belong to current Provider", nil)
	}
	if vpc.Site == nil {
		return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Site for VPC", nil)
	}
	if vpc.Site.InfrastructureProviderID != provider.ID {
		return nil, nil, cutil.NewAPIError(http.StatusForbidden, "VPC Site does not belong to current Provider", nil)
	}
	if vpc.Site.Status != cdbm.SiteStatusRegistered {
		return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "VPC Site is not in Registered state", nil)
	}
	stc, err := h.scp.GetClientByID(vpc.SiteID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for VPC Site")
		return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}
	return vpc, stc, nil
}

// getVPCRoutingState reads authoritative routing state from Core and checks that
// the response identifies the requested controller VPC.
func getVPCRoutingState(ctx context.Context, stc tclient.Client, controllerID string) (*model.APIVpcRoutingState, *cutil.APIError) {
	var raw corev1.VpcRoutingState
	req := &corev1.VpcRoutingStateRequest{Id: &corev1.VpcId{Value: controllerID}}
	apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_GetVpcRoutingState_FullMethodName, req, &raw, "")
	if apiErr != nil {
		return nil, apiErr
	}
	var state model.APIVpcRoutingState
	state.FromProto(&raw)
	err := state.ValidateResponse(controllerID)
	if err != nil {
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Invalid VPC routing state response", err)
	}
	return &state, nil
}

// vpcRoutingMutationError logs a failed mutation and returns its HTTP error,
// warning the caller to inspect routing state when the change may have committed.
func vpcRoutingMutationError(c echo.Context, logger zerolog.Logger, apiErr *cutil.APIError) error {
	logAPIError(logger, apiErr, "VPC routing mutation failed")
	message := apiErr.Message
	if apiErr.Code >= http.StatusInternalServerError && apiErr.Code != http.StatusNotImplemented {
		message += "; the operation may have committed, inspect VPC routing state before submitting another request"
	}
	return cutil.NewAPIErrorResponse(c, apiErr.Code, message, nil)
}

// GetVPCRoutingProfileDefinitionHandler reads one or a page of named definitions.
type GetVPCRoutingProfileDefinitionHandler struct {
	vpcRoutingProfileHandler
}

// NewGetVPCRoutingProfileDefinitionHandler initializes both named-profile GET routes.
func NewGetVPCRoutingProfileDefinitionHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetVPCRoutingProfileDefinitionHandler {
	return GetVPCRoutingProfileDefinitionHandler{
		vpcRoutingProfileHandler: newVPCRoutingProfileHandler(dbSession, scp),
	}
}

// Handle returns full definitions visible to a Provider or privileged Tenant.
func (h GetVPCRoutingProfileDefinitionHandler) Handle(c echo.Context) error {
	org, user, ctx, logger, span := common.SetupHandler("VPCRoutingProfileDefinition", "Get", c)
	if span != nil {
		defer span.End()
	}
	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()
	if user == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	provider, tenant, apiErr := common.IsProviderOrTenant(ctx, logger, h.dbSession, org, user, true, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	var req model.APIVpcRoutingProfileGetRequest
	var page pagination.PageRequest
	profileName := c.Param("profileName")
	var err error
	if profileName == "" {
		err = common.ValidateKnownQueryParams(c.QueryParams(), req, page)
	} else {
		err = common.ValidateKnownQueryParams(c.QueryParams(), struct {
			SiteID string `query:"siteId"`
		}{})
	}
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	// Bind query parameters only: a GET body cannot replace the Site or filters.
	err = (&echo.DefaultBinder{}).BindQueryParams(c, &req)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid routing profile query", nil)
	}
	err = req.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if profileName == "" {
		err = (&echo.DefaultBinder{}).BindQueryParams(c, &page)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid pagination query", nil)
		}
		if page.OrderByStr == nil {
			page.OrderByStr = cutil.GetPtr("NAME_ASC")
		}
		err = page.Validate([]string{"name"})
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
		}
	} else {
		// Echo retains escaped path parameters. Decode once, preserving '+' and case.
		profileName, err = url.PathUnescape(profileName)
		if err != nil || !utf8.ValidString(profileName) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid routing profile name", nil)
		}
	}

	site, err := common.GetSiteFromIDString(ctx, nil, req.SiteID, h.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site specified in request", nil)
		}
		logger.Error().Err(err).Msg("failed to retrieve Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site", nil)
	}
	if provider != nil && site.InfrastructureProviderID != provider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site does not belong to org's Provider", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered || site.Config == nil || !site.Config.NativeNetworking {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site must be Registered with native networking enabled", nil)
	}
	if provider == nil {
		privileged, privilegeErr := common.TenantHasTargetedInstanceCreation(ctx, nil, h.dbSession, tenant, &common.TenantPrivilegeScope{
			SiteID: &site.ID,
		})
		if privilegeErr != nil {
			logger.Error().Err(privilegeErr).Msg("failed to resolve Tenant privilege for Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability, DB error", nil)
		}
		if !privileged {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have Targeted Instance Creation capability enabled for Site", nil)
		}
	}
	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}
	var minimumTier uint32
	if provider == nil {
		var current corev1.FindTenantResponse
		apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_FindTenant_FullMethodName, &corev1.FindTenantRequest{
			TenantOrganizationId: tenant.Org,
		}, &current, "")
		if apiErr != nil {
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}
		if current.GetTenant() == nil {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Tenant was not found on Site", nil)
		}
		if current.Tenant.GetOrganizationId() != tenant.Org || current.Tenant.GetRoutingProfileType() == "" {
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Site returned incomplete or mismatched Tenant data", nil)
		}
		defaultProfile, profileErr := getNamedRoutingProfile(ctx, stc, current.Tenant.GetRoutingProfileType())
		if profileErr != nil {
			if profileErr.Code == http.StatusNotFound {
				profileErr = cutil.NewAPIError(http.StatusInternalServerError, "Tenant default routing profile was not found on Site", nil)
			}
			return cutil.NewAPIErrorResponse(c, profileErr.Code, profileErr.Message, nil)
		}
		minimumTier = defaultProfile.Config.GetAccessTier()
		requestedMinimum := minimumTier
		if req.MinimumAccessTier != nil {
			requestedMinimum = *req.MinimumAccessTier
		}
		if requestedMinimum < minimumTier {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Cannot query routing profiles with access tiers lower than the Tenant's default routing profile", nil)
		}
		req.MinimumAccessTier = cutil.GetPtr(max(minimumTier, requestedMinimum))
	}

	if profileName != "" {
		profile, profileErr := getNamedRoutingProfile(ctx, stc, profileName)
		if profileErr != nil {
			return cutil.NewAPIErrorResponse(c, profileErr.Code, profileErr.Message, nil)
		}
		if provider == nil && profile.Config.GetAccessTier() < minimumTier {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Cannot view a routing profile with an access tier lower than the Tenant's default routing profile", nil)
		}
		var result model.APIVpcRoutingProfileDefinition
		result.FromProto(profile)
		return c.JSON(http.StatusOK, result)
	}

	var names corev1.RoutingProfileNameList
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_FindRoutingProfileNames_FullMethodName, req.ToProto(), &names, "")
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	slices.Sort(names.Names)
	names.Names = slices.Compact(names.Names)
	if *page.OrderByStr == "NAME_DESC" {
		slices.Reverse(names.Names)
	}
	total := len(names.Names)
	start := min(*page.Offset, total)
	selected := names.Names[start:min(start+*page.Limit, total)]
	profiles, apiErr := findNamedRoutingProfiles(ctx, stc, selected)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	result := make([]model.APIVpcRoutingProfileDefinition, 0, len(selected))
	for _, name := range selected {
		profile := profiles[name]
		// Configuration can change between name discovery and definition lookup.
		// Never return a definition that no longer meets the authorized filter.
		if profile == nil || (req.MinimumAccessTier != nil && profile.Config.GetAccessTier() < *req.MinimumAccessTier) || (req.Internal != nil && profile.Config.GetInternal() != *req.Internal) {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Routing profiles changed during the query; retry the request", nil)
		}
		var item model.APIVpcRoutingProfileDefinition
		item.FromProto(profile)
		result = append(result, item)
	}
	header, err := json.Marshal(pagination.NewPageResponse(*page.PageNumber, *page.PageSize, total, page.OrderByStr))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(header))
	return c.JSON(http.StatusOK, result)
}

// findNamedRoutingProfiles preserves exact configured names and rejects malformed replies.
func findNamedRoutingProfiles(ctx context.Context, stc tclient.Client, names []string) (map[string]*corev1.NamedRoutingProfile, *cutil.APIError) {
	profiles := make(map[string]*corev1.NamedRoutingProfile, len(names))
	if len(names) == 0 {
		return profiles, nil
	}
	var response corev1.NamedRoutingProfileList
	apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_FindRoutingProfilesByNames_FullMethodName, &corev1.RoutingProfilesByNamesRequest{
		Names: names,
	}, &response, "")
	if apiErr != nil {
		// Core's only nonempty-request validation is its configurable name-count
		// limit. Split oversized reads so REST pages also work on smaller sites.
		if apiErr.Code != http.StatusBadRequest || len(names) == 1 {
			return nil, apiErr
		}
		for _, batch := range [][]string{names[:len(names)/2], names[len(names)/2:]} {
			found, batchErr := findNamedRoutingProfiles(ctx, stc, batch)
			if batchErr != nil {
				return nil, batchErr
			}
			for name, profile := range found {
				profiles[name] = profile
			}
		}
		return profiles, nil
	}
	for _, profile := range response.Profiles {
		if profile.GetConfig() == nil || !slices.Contains(names, profile.GetName()) || profiles[profile.GetName()] != nil {
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Site returned incomplete or mismatched routing profile data", nil)
		}
		profiles[profile.Name] = profile
	}
	return profiles, nil
}

func getNamedRoutingProfile(ctx context.Context, stc tclient.Client, name string) (*corev1.NamedRoutingProfile, *cutil.APIError) {
	profiles, apiErr := findNamedRoutingProfiles(ctx, stc, []string{name})
	if apiErr != nil {
		return nil, apiErr
	}
	profile := profiles[name]
	if profile == nil {
		return nil, cutil.NewAPIError(http.StatusNotFound, "Routing profile was not found on Site", nil)
	}
	return profile, nil
}
