// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// CreateMeasuredBootTrustedMachineHandler creates a machine trust approval.
type CreateMeasuredBootTrustedMachineHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewCreateMeasuredBootTrustedMachineHandler returns a machine trust approval creation handler.
func NewCreateMeasuredBootTrustedMachineHandler(dbSession *cdb.Session, scp *sc.ClientPool) CreateMeasuredBootTrustedMachineHandler {
	return CreateMeasuredBootTrustedMachineHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle creates a machine trust approval.
func (cmbtmh CreateMeasuredBootTrustedMachineHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MeasuredBootTrustedMachine", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIMeasuredBootTrustedMachineCreateRequest
	err := c.Bind(&apiReq)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Measured Boot Trusted Machine creation request data", err)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: cmbtmh.dbSession, SCP: cmbtmh.scp, Org: org, User: dbUser, SiteID: apiReq.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	coreResp := &corev1.AddMeasurementTrustedMachineResponse{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_AddMeasurementTrustedMachine_FullMethodName, apiReq.ToProto(), coreResp, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to create machine trust approval")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	return c.JSON(http.StatusCreated, model.NewAPIMeasuredBootTrustedMachine(coreResp.GetApprovalRecord()))
}

// GetAllMeasuredBootTrustedMachineHandler lists machine trust approvals.
type GetAllMeasuredBootTrustedMachineHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetAllMeasuredBootTrustedMachineHandler returns a machine trust approval list handler.
func NewGetAllMeasuredBootTrustedMachineHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetAllMeasuredBootTrustedMachineHandler {
	return GetAllMeasuredBootTrustedMachineHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle lists machine trust approvals.
func (gambtmh GetAllMeasuredBootTrustedMachineHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MeasuredBootTrustedMachine", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: gambtmh.dbSession, SCP: gambtmh.scp, Org: org, User: dbUser, SiteID: c.QueryParam("siteId"),
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	coreResp := &corev1.ListMeasurementTrustedMachinesResponse{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_ListMeasurementTrustedMachines_FullMethodName, &corev1.ListMeasurementTrustedMachinesRequest{}, coreResp, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to list machine trust approvals")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	var resp model.APIMeasuredBootTrustedMachines
	resp.FromProto(coreResp.GetApprovalRecords())
	return c.JSON(http.StatusOK, resp)
}

// DeleteMeasuredBootTrustedMachineHandler deletes a machine trust approval.
type DeleteMeasuredBootTrustedMachineHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewDeleteMeasuredBootTrustedMachineHandler returns a machine trust approval deletion handler.
func NewDeleteMeasuredBootTrustedMachineHandler(dbSession *cdb.Session, scp *sc.ClientPool) DeleteMeasuredBootTrustedMachineHandler {
	return DeleteMeasuredBootTrustedMachineHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle deletes a machine trust approval.
func (dmbtmh DeleteMeasuredBootTrustedMachineHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MeasuredBootTrustedMachine", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiReq := model.APIMeasuredBootTrustedMachineDeleteRequest{Selector: c.QueryParam("selector"), ID: c.Param("id")}
	err := apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Measured Boot Trusted Machine deletion request data", err)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: dmbtmh.dbSession, SCP: dmbtmh.scp, Org: org, User: dbUser, SiteID: c.QueryParam("siteId"),
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	coreResp := &corev1.RemoveMeasurementTrustedMachineResponse{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_RemoveMeasurementTrustedMachine_FullMethodName, apiReq.ToProto(), coreResp, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to delete machine trust approval")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	return c.JSON(http.StatusOK, model.NewAPIMeasuredBootTrustedMachine(coreResp.GetApprovalRecord()))
}

// CreateMeasuredBootTrustedProfileHandler creates a profile trust approval.
type CreateMeasuredBootTrustedProfileHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewCreateMeasuredBootTrustedProfileHandler returns a profile trust approval creation handler.
func NewCreateMeasuredBootTrustedProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) CreateMeasuredBootTrustedProfileHandler {
	return CreateMeasuredBootTrustedProfileHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle creates a profile trust approval.
func (cmbtph CreateMeasuredBootTrustedProfileHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MeasuredBootTrustedProfile", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIMeasuredBootTrustedProfileCreateRequest
	err := c.Bind(&apiReq)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Measured Boot Trusted Profile creation request data", err)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: cmbtph.dbSession, SCP: cmbtph.scp, Org: org, User: dbUser, SiteID: apiReq.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	coreResp := &corev1.AddMeasurementTrustedProfileResponse{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_AddMeasurementTrustedProfile_FullMethodName, apiReq.ToProto(), coreResp, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to create profile trust approval")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	return c.JSON(http.StatusCreated, model.NewAPIMeasuredBootTrustedProfile(coreResp.GetApprovalRecord()))
}

// GetAllMeasuredBootTrustedProfileHandler lists profile trust approvals.
type GetAllMeasuredBootTrustedProfileHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetAllMeasuredBootTrustedProfileHandler returns a profile trust approval list handler.
func NewGetAllMeasuredBootTrustedProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetAllMeasuredBootTrustedProfileHandler {
	return GetAllMeasuredBootTrustedProfileHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle lists profile trust approvals.
func (gambtph GetAllMeasuredBootTrustedProfileHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MeasuredBootTrustedProfile", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: gambtph.dbSession, SCP: gambtph.scp, Org: org, User: dbUser, SiteID: c.QueryParam("siteId"),
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	coreResp := &corev1.ListMeasurementTrustedProfilesResponse{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_ListMeasurementTrustedProfiles_FullMethodName, &corev1.ListMeasurementTrustedProfilesRequest{}, coreResp, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to list profile trust approvals")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	var resp model.APIMeasuredBootTrustedProfiles
	resp.FromProto(coreResp.GetApprovalRecords())
	return c.JSON(http.StatusOK, resp)
}

// DeleteMeasuredBootTrustedProfileHandler deletes a profile trust approval.
type DeleteMeasuredBootTrustedProfileHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewDeleteMeasuredBootTrustedProfileHandler returns a profile trust approval deletion handler.
func NewDeleteMeasuredBootTrustedProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) DeleteMeasuredBootTrustedProfileHandler {
	return DeleteMeasuredBootTrustedProfileHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle deletes a profile trust approval.
func (dmbtph DeleteMeasuredBootTrustedProfileHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MeasuredBootTrustedProfile", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiReq := model.APIMeasuredBootTrustedProfileDeleteRequest{Selector: c.QueryParam("selector"), ID: c.Param("id")}
	err := apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Measured Boot Trusted Profile deletion request data", err)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx: ctx, Logger: logger, DBSession: dmbtph.dbSession, SCP: dmbtph.scp, Org: org, User: dbUser, SiteID: c.QueryParam("siteId"),
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	coreResp := &corev1.RemoveMeasurementTrustedProfileResponse{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_RemoveMeasurementTrustedProfile_FullMethodName, apiReq.ToProto(), coreResp, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to delete profile trust approval")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	return c.JSON(http.StatusOK, model.NewAPIMeasuredBootTrustedProfile(coreResp.GetApprovalRecord()))
}
