// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"fmt"
	"net/http"

	"github.com/labstack/echo/v4"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type rackHealthReportQuery struct {
	SiteID string `query:"siteId"`
}

type rackHealthReportAction string

const (
	rackHealthReportList   rackHealthReportAction = "List"
	rackHealthReportInsert rackHealthReportAction = "Insert"
	rackHealthReportRemove rackHealthReportAction = "Remove"
)

type rackHealthReportHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// GetAllRackHealthReportHandler lists Rack health reports.
type GetAllRackHealthReportHandler struct {
	rackHealthReportHandler
}

// CreateOrUpdateRackHealthReportHandler creates or updates a Rack health report.
type CreateOrUpdateRackHealthReportHandler struct {
	rackHealthReportHandler
}

// DeleteRackHealthReportHandler deletes a Rack health report.
type DeleteRackHealthReportHandler struct {
	rackHealthReportHandler
}

func newRackHealthReportHandler(dbSession *cdb.Session, scp *sc.ClientPool) rackHealthReportHandler {
	return rackHealthReportHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// NewGetAllRackHealthReportHandler returns a Rack health report list handler.
func NewGetAllRackHealthReportHandler(dbSession *cdb.Session, scp *sc.ClientPool, _ *config.Config) GetAllRackHealthReportHandler {
	return GetAllRackHealthReportHandler{newRackHealthReportHandler(dbSession, scp)}
}

// NewCreateOrUpdateRackHealthReportHandler returns a Rack health report insert handler.
func NewCreateOrUpdateRackHealthReportHandler(dbSession *cdb.Session, scp *sc.ClientPool, _ *config.Config) CreateOrUpdateRackHealthReportHandler {
	return CreateOrUpdateRackHealthReportHandler{newRackHealthReportHandler(dbSession, scp)}
}

// NewDeleteRackHealthReportHandler returns a Rack health report remove handler.
func NewDeleteRackHealthReportHandler(dbSession *cdb.Session, scp *sc.ClientPool, _ *config.Config) DeleteRackHealthReportHandler {
	return DeleteRackHealthReportHandler{newRackHealthReportHandler(dbSession, scp)}
}

// Handle godoc
// @Summary Get all Rack Health Reports
// @Description Get all health report overrides for a Rack.
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Rack"
// @Param siteId query string true "ID of Site" Format(uuid)
// @Success 200 {array} model.APIMachineHealthReportEntry
// @Router /v2/org/{org}/nico/rack/{id}/health-report [get]
func (h GetAllRackHealthReportHandler) Handle(c echo.Context) error {
	return handleRackHealthReport(c, h.rackHealthReportHandler, rackHealthReportList)
}

// Handle godoc
// @Summary Insert Rack Health Report
// @Description Add or update a Rack health report override.
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Rack"
// @Param request body model.APIRackHealthReportEntryRequest true "Rack health report"
// @Success 200 {object} model.APIMachineHealthReportEntry
// @Router /v2/org/{org}/nico/rack/{id}/health-report [put]
func (h CreateOrUpdateRackHealthReportHandler) Handle(c echo.Context) error {
	return handleRackHealthReport(c, h.rackHealthReportHandler, rackHealthReportInsert)
}

// Handle godoc
// @Summary Delete Rack Health Report
// @Description Delete a Rack health report override by source.
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Rack"
// @Param source path string true "Health report source"
// @Param siteId query string true "ID of Site" Format(uuid)
// @Success 204
// @Router /v2/org/{org}/nico/rack/{id}/health-report/{source} [delete]
func (h DeleteRackHealthReportHandler) Handle(c echo.Context) error {
	return handleRackHealthReport(c, h.rackHealthReportHandler, rackHealthReportRemove)
}

func handleRackHealthReport(c echo.Context, h rackHealthReportHandler, action rackHealthReportAction) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("RackHealthReport", string(action), c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	query := rackHealthReportQuery{}
	apiReq := model.APIMachineHealthReportEntryRequest{}
	if action == rackHealthReportInsert {
		err := common.ValidateKnownQueryParams(c.QueryParams(), struct{}{})
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
		}
		request := model.APIRackHealthReportEntryRequest{}
		err = c.Bind(&request)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
		}
		err = request.Validate()
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
		}
		query.SiteID = request.SiteID
		apiReq = request.APIMachineHealthReportEntryRequest
	} else {
		err := common.ValidateKnownQueryParams(c.QueryParams(), rackHealthReportQuery{})
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
		}
		query.SiteID = c.QueryParam("siteId")
	}
	if query.SiteID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId is required", nil)
	}

	resourceID := c.Param("id")
	if resourceID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Rack ID was not specified in URL", nil)
	}
	source := c.Param("source")
	if action == rackHealthReportRemove && source == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Rack health report source was not specified in URL", nil)
	}

	stc, siteID, authErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: h.dbSession,
		SCP:       h.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    query.SiteID,
	})
	if authErr != nil {
		return cutil.NewAPIErrorResponse(c, authErr.Code, authErr.Message, authErr.Data)
	}

	method, coreReq, coreResp, insertedEntry, err := rackHealthReportCoreRequest(action, resourceID, source, apiReq, dbUser)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to prepare Rack health report operation")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to prepare Rack health report operation", nil)
	}

	operationLogger := logger.With().
		Str("resource", "Rack").
		Str("action", string(action)).
		Str("resource_id", resourceID).
		Str("site_id", siteID).
		Str("core_method", method).
		Logger()
	logSource := source
	if action == rackHealthReportInsert {
		logSource = apiReq.Source
	}
	if logSource != "" {
		operationLogger = operationLogger.With().Str("source", logSource).Logger()
	}
	operationLogger.Info().Msg("Proxying Rack health report operation to Core")

	apiErr := common.ExecuteCoreGRPC(ctx, stc, method, coreReq, coreResp, siteID)
	if apiErr != nil {
		logAPIError(operationLogger, apiErr, "Failed to proxy Rack health report operation to Core")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	switch action {
	case rackHealthReportList:
		apiResp := []model.APIMachineHealthReportEntry{}
		for _, entry := range coreResp.(*corev1.ListHealthReportResponse).GetHealthReportEntries() {
			apiEntry := model.APIMachineHealthReportEntry{}
			apiEntry.FromProto(entry)
			apiResp = append(apiResp, apiEntry)
		}
		return c.JSON(http.StatusOK, apiResp)
	case rackHealthReportInsert:
		apiResp := model.APIMachineHealthReportEntry{}
		apiResp.FromProto(insertedEntry)
		return c.JSON(http.StatusOK, apiResp)
	case rackHealthReportRemove:
		return c.NoContent(http.StatusNoContent)
	default:
		operationLogger.Error().Msg("Unsupported Rack health report action reached response handling")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to process Rack health report operation", nil)
	}
}

func rackHealthReportCoreRequest(action rackHealthReportAction, resourceID, source string, apiReq model.APIMachineHealthReportEntryRequest, dbUser *cdbm.User) (string, proto.Message, proto.Message, *corev1.HealthReportEntry, error) {
	switch action {
	case rackHealthReportList:
		return corev1.Forge_ListRackHealthReports_FullMethodName, &corev1.ListRackHealthReportsRequest{RackId: &corev1.RackId{Id: resourceID}}, &corev1.ListHealthReportResponse{}, nil, nil
	case rackHealthReportInsert:
		entry := apiReq.ToHealthReportEntryProto(dbUser)
		return corev1.Forge_InsertRackHealthReport_FullMethodName, &corev1.InsertRackHealthReportRequest{RackId: &corev1.RackId{Id: resourceID}, HealthReportEntry: entry}, nil, entry, nil
	case rackHealthReportRemove:
		return corev1.Forge_RemoveRackHealthReport_FullMethodName, &corev1.RemoveRackHealthReportRequest{RackId: &corev1.RackId{Id: resourceID}, Source: source}, nil, nil, nil
	default:
		return "", nil, nil, nil, fmt.Errorf("unsupported Rack health report action %q", action)
	}
}
