// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"net/http"
	"sort"

	"github.com/labstack/echo/v4"
	"google.golang.org/protobuf/types/known/emptypb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// CreateAttestationProfileHandler creates the attestation profile for a
// hardware class.
type CreateAttestationProfileHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewCreateAttestationProfileHandler returns an attestation profile creation handler.
func NewCreateAttestationProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) CreateAttestationProfileHandler {
	return CreateAttestationProfileHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Create Attestation Profile
// @Description Create the SPDM attestation profile for a hardware class recorded on the Site, or the reserved `any` fallback profile.
// @Tags attestation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param request body model.APIAttestationProfileCreateRequest true "Attestation profile"
// @Success 201 {object} model.APIAttestationProfile
// @Router /v2/org/{org}/nico/attestation-profile [post]
func (h CreateAttestationProfileHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AttestationProfile", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiErr := admitAttestationQuery(c)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var apiReq model.APIAttestationProfileCreateRequest
	err := decodeClosedJSONObject(c, &apiReq)
	if err != nil {
		return sendAttestationAdmissionError(c, err)
	}
	err = apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Attestation Profile creation request data", err)
	}

	stc, siteID, apiErr := authorizeAttestationSite(ctx, logger, h.dbSession, h.scp, org, dbUser, apiReq.SiteID)
	if apiErr != nil {
		return apiErr.Send(c)
	}

	coreResp := &corev1.AttestationProfile{}
	apiErr = opCreateAttestationProfile.call(ctx, logger, stc, siteID, apiReq.ToProto(), coreResp)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var resp model.APIAttestationProfile
	err = resp.FromProtoForClass(apiReq.HardwareClass, coreResp)
	if err != nil {
		return opCreateAttestationProfile.invalidReplyError(logger, err).Send(c)
	}
	return c.JSON(http.StatusCreated, resp)
}

// GetAllAttestationProfileHandler lists a Site's attestation profiles.
type GetAllAttestationProfileHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetAllAttestationProfileHandler returns an attestation profile list handler.
func NewGetAllAttestationProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetAllAttestationProfileHandler {
	return GetAllAttestationProfileHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Retrieve all Attestation Profiles
// @Description List the Site's SPDM attestation profiles ordered by hardware class.
// @Tags attestation
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field; only HARDWARE_CLASS_ASC is supported"
// @Success 200 {array} model.APIAttestationProfile
// @Router /v2/org/{org}/nico/attestation-profile [get]
func (h GetAllAttestationProfileHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AttestationProfile", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIAttestationProfileListRequest
	apiErr := admitAttestationQuery(c, &apiReq.Site, &apiReq.Page)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	err := apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Attestation Profile list request data", err)
	}
	err = apiReq.ValidatePage()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}
	page := apiReq.Page

	stc, siteID, apiErr := authorizeAttestationSite(ctx, logger, h.dbSession, h.scp, org, dbUser, apiReq.Site.SiteID)
	if apiErr != nil {
		return apiErr.Send(c)
	}

	coreResp := &corev1.ListAttestationProfilesResponse{}
	apiErr = opListAttestationProfiles.call(ctx, logger, stc, siteID, &emptypb.Empty{}, coreResp)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	profiles := make([]model.APIAttestationProfile, 0, len(coreResp.GetProfiles()))
	for _, p := range coreResp.GetProfiles() {
		var profile model.APIAttestationProfile
		err = profile.FromProto(p)
		if err != nil {
			return opListAttestationProfiles.invalidReplyError(logger, err).Send(c)
		}
		profiles = append(profiles, profile)
	}
	sort.Slice(profiles, func(i, j int) bool { return profiles[i].HardwareClass < profiles[j].HardwareClass })

	total := len(profiles)
	start := min(*page.Offset, total)
	end := min(start+*page.Limit, total)

	pageHeader, err := json.Marshal(pagination.NewPageResponse(*page.PageNumber, *page.PageSize, total, page.OrderByStr))
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))
	return c.JSON(http.StatusOK, profiles[start:end])
}

// GetAttestationProfileHandler reads one attestation profile.
type GetAttestationProfileHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetAttestationProfileHandler returns an attestation profile read handler.
func NewGetAttestationProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetAttestationProfileHandler {
	return GetAttestationProfileHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Retrieve Attestation Profile
// @Description Read the SPDM attestation profile for a hardware class, including the version to pass as ifVersionMatch when changing it.
// @Tags attestation
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param hardwareClass path string true "Hardware class, or any"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APIAttestationProfile
// @Router /v2/org/{org}/nico/attestation-profile/{hardwareClass} [get]
func (h GetAttestationProfileHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AttestationProfile", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIAttestationProfileGetRequest
	apiErr := admitAttestationQuery(c, &apiReq)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	apiReq.HardwareClass = c.Param("hardwareClass")
	err := apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Attestation Profile request data", err)
	}

	stc, siteID, apiErr := authorizeAttestationSite(ctx, logger, h.dbSession, h.scp, org, dbUser, apiReq.SiteID)
	if apiErr != nil {
		return apiErr.Send(c)
	}

	coreResp := &corev1.AttestationProfile{}
	apiErr = opGetAttestationProfile.call(ctx, logger, stc, siteID, apiReq.ToProto(), coreResp)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var resp model.APIAttestationProfile
	err = resp.FromProtoForClass(apiReq.HardwareClass, coreResp)
	if err != nil {
		return opGetAttestationProfile.invalidReplyError(logger, err).Send(c)
	}
	return c.JSON(http.StatusOK, resp)
}

// UpdateAttestationProfileHandler replaces an attestation profile's selection.
type UpdateAttestationProfileHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewUpdateAttestationProfileHandler returns an attestation profile update handler.
func NewUpdateAttestationProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) UpdateAttestationProfileHandler {
	return UpdateAttestationProfileHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Update Attestation Profile
// @Description Replace an attestation profile's selection when ifVersionMatch equals its current version. Setting the any profile to None requires acknowledgeFallbackImpact=true.
// @Tags attestation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param hardwareClass path string true "Hardware class, or any"
// @Param request body model.APIAttestationProfileUpdateRequest true "Attestation profile update"
// @Success 200 {object} model.APIAttestationProfile
// @Router /v2/org/{org}/nico/attestation-profile/{hardwareClass} [patch]
func (h UpdateAttestationProfileHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AttestationProfile", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiErr := admitAttestationQuery(c)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var apiReq model.APIAttestationProfileUpdateRequest
	err := decodeClosedJSONObject(c, &apiReq)
	if err != nil {
		return sendAttestationAdmissionError(c, err)
	}
	apiReq.HardwareClass = c.Param("hardwareClass")
	err = apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Attestation Profile update request data", err)
	}

	stc, siteID, apiErr := authorizeAttestationSite(ctx, logger, h.dbSession, h.scp, org, dbUser, apiReq.SiteID)
	if apiErr != nil {
		return apiErr.Send(c)
	}

	coreResp := &corev1.AttestationProfile{}
	apiErr = opUpdateAttestationProfile.call(ctx, logger, stc, siteID, apiReq.ToProto(), coreResp)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var resp model.APIAttestationProfile
	err = resp.FromProtoForClass(apiReq.HardwareClass, coreResp)
	if err != nil {
		return opUpdateAttestationProfile.invalidReplyError(logger, err).Send(c)
	}
	return c.JSON(http.StatusOK, resp)
}

// DeleteAttestationProfileHandler deletes an attestation profile.
type DeleteAttestationProfileHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewDeleteAttestationProfileHandler returns an attestation profile deletion handler.
func NewDeleteAttestationProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) DeleteAttestationProfileHandler {
	return DeleteAttestationProfileHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Delete Attestation Profile
// @Description Delete an attestation profile when ifVersionMatch equals its current version. Deleting the any profile requires acknowledgeFallbackImpact=true. The request body must be empty.
// @Tags attestation
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param hardwareClass path string true "Hardware class, or any"
// @Param siteId query string true "ID of the Site"
// @Param ifVersionMatch query string true "Version the caller read"
// @Param acknowledgeFallbackImpact query boolean false "Required to delete the any profile"
// @Success 204
// @Router /v2/org/{org}/nico/attestation-profile/{hardwareClass} [delete]
func (h DeleteAttestationProfileHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AttestationProfile", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIAttestationProfileDeleteRequest
	apiErr := admitAttestationQuery(c, &apiReq)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	err := requireEmptyAttestationBody(c)
	if err != nil {
		return sendAttestationAdmissionError(c, err)
	}
	apiReq.HardwareClass = c.Param("hardwareClass")
	err = apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Attestation Profile deletion request data", err)
	}

	stc, siteID, apiErr := authorizeAttestationSite(ctx, logger, h.dbSession, h.scp, org, dbUser, apiReq.SiteID)
	if apiErr != nil {
		return apiErr.Send(c)
	}

	apiErr = opDeleteAttestationProfile.call(ctx, logger, stc, siteID, apiReq.ToProto(), &corev1.DeleteAttestationProfileResponse{})
	if apiErr != nil {
		return apiErr.Send(c)
	}
	return c.NoContent(http.StatusNoContent)
}

// GetAttestationCoverageHandler reports which profile covers each recorded
// hardware class.
type GetAttestationCoverageHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetAttestationCoverageHandler returns an attestation coverage handler.
func NewGetAttestationCoverageHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetAttestationCoverageHandler {
	return GetAttestationCoverageHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Retrieve Attestation Coverage
// @Description Report, for every hardware class recorded on the Site's explored endpoints, which attestation profile would decide attestation and which attester sets the class has reported.
// @Tags attestation
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APIAttestationCoverage
// @Router /v2/org/{org}/nico/attestation-coverage [get]
func (h GetAttestationCoverageHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AttestationCoverage", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var siteQuery model.APIAttestationSiteQuery
	apiErr := admitAttestationQuery(c, &siteQuery)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	err := siteQuery.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Attestation Coverage request data", err)
	}

	stc, siteID, apiErr := authorizeAttestationSite(ctx, logger, h.dbSession, h.scp, org, dbUser, siteQuery.SiteID)
	if apiErr != nil {
		return apiErr.Send(c)
	}

	coreResp := &corev1.GetAttestationCoverageResponse{}
	apiErr = opGetAttestationCoverage.call(ctx, logger, stc, siteID, &emptypb.Empty{}, coreResp)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var resp model.APIAttestationCoverage
	err = resp.FromProto(coreResp)
	if err != nil {
		return opGetAttestationCoverage.invalidReplyError(logger, err).Send(c)
	}
	return c.JSON(http.StatusOK, resp)
}
