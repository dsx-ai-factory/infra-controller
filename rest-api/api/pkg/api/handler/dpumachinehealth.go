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

// GetAllDpuMachineHealthReportHandler lists the health reports of one DPU Machine for a Site via Core.
type GetAllDpuMachineHealthReportHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetAllDpuMachineHealthReportHandler returns a handler for listing DPU Machine health reports.
func NewGetAllDpuMachineHealthReportHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetAllDpuMachineHealthReportHandler {
	return GetAllDpuMachineHealthReportHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Retrieve all DPU Machine health reports
// @Description Retrieve the per source health reports of a DPU Machine.
// @Tags DPU Machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param dpuMachineId path string true "ID of DPU Machine"
// @Param siteId query string true "ID of Site"
// @Success 200 {array} model.APIMachineHealthReportEntry
// @Router /v2/org/{org}/nico/dpu/{dpuMachineId}/health-report [get]
func (gadmhrh GetAllDpuMachineHealthReportHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("DpuMachineHealthReport", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiRequest := model.APIGetAllDpuMachineHealthReportRequest{}
	err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating DPU Machine health report retrieval request data", err)
	}

	// Provider Admin only, matching GET /dpu/:id: resolveDpuMachine rejects
	// Provider Viewer and Tenant roles.
	dpuMachineID := c.Param("id")
	resolved, apiErr := resolveDpuMachine(resolveDpuMachineInput{
		Ctx:          ctx,
		Logger:       logger,
		DBSession:    gadmhrh.dbSession,
		SCP:          gadmhrh.scp,
		Org:          org,
		User:         dbUser,
		SiteID:       apiRequest.SiteID,
		DpuMachineID: dpuMachineID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	coreResponse := &corev1.ListHealthReportResponse{}
	apiErr = common.ExecuteCoreGRPC(ctx, resolved.stc, corev1.Forge_ListMachineHealthReports_FullMethodName, &corev1.MachineId{Id: dpuMachineID}, coreResponse, resolved.site.ID.String())
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to list DPU Machine health reports via Core gRPC proxy")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	apiHealthReports := make([]model.APIMachineHealthReportEntry, 0, len(coreResponse.GetHealthReportEntries()))
	for _, entry := range coreResponse.GetHealthReportEntries() {
		apiHealthReport := model.APIMachineHealthReportEntry{}
		apiHealthReport.FromProto(entry)
		apiHealthReports = append(apiHealthReports, apiHealthReport)
	}

	logger.Info().Str("dpuMachineId", dpuMachineID).Int("returned", len(apiHealthReports)).Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiHealthReports)
}
