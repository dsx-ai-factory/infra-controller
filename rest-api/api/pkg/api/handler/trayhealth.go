// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"fmt"
	"net/http"
	"strings"

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

type trayHealthReportQuery struct {
	SiteID string `query:"siteId"`
	Type   string `query:"type"`
}

type trayHealthReportAction string

const (
	trayHealthReportList   trayHealthReportAction = "List"
	trayHealthReportInsert trayHealthReportAction = "Insert"
	trayHealthReportRemove trayHealthReportAction = "Remove"

	computeTrayIDPrefix    = "fm100"
	nvSwitchTrayIDPrefix   = "sw100"
	powerShelfTrayIDPrefix = "ps100"
)

type trayHealthReportHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// GetAllTrayHealthReportHandler lists Tray health reports.
type GetAllTrayHealthReportHandler struct {
	trayHealthReportHandler
}

// CreateOrUpdateTrayHealthReportHandler creates or updates a Tray health report.
type CreateOrUpdateTrayHealthReportHandler struct {
	trayHealthReportHandler
}

// DeleteTrayHealthReportHandler deletes a Tray health report.
type DeleteTrayHealthReportHandler struct {
	trayHealthReportHandler
}

func newTrayHealthReportHandler(dbSession *cdb.Session, scp *sc.ClientPool) trayHealthReportHandler {
	return trayHealthReportHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// NewGetAllTrayHealthReportHandler returns a Tray health report list handler.
func NewGetAllTrayHealthReportHandler(dbSession *cdb.Session, scp *sc.ClientPool, _ *config.Config) GetAllTrayHealthReportHandler {
	return GetAllTrayHealthReportHandler{newTrayHealthReportHandler(dbSession, scp)}
}

// NewCreateOrUpdateTrayHealthReportHandler returns a Tray health report insert handler.
func NewCreateOrUpdateTrayHealthReportHandler(dbSession *cdb.Session, scp *sc.ClientPool, _ *config.Config) CreateOrUpdateTrayHealthReportHandler {
	return CreateOrUpdateTrayHealthReportHandler{newTrayHealthReportHandler(dbSession, scp)}
}

// NewDeleteTrayHealthReportHandler returns a Tray health report remove handler.
func NewDeleteTrayHealthReportHandler(dbSession *cdb.Session, scp *sc.ClientPool, _ *config.Config) DeleteTrayHealthReportHandler {
	return DeleteTrayHealthReportHandler{newTrayHealthReportHandler(dbSession, scp)}
}

// Handle godoc
// @Summary Get all Tray Health Reports
// @Description Get all health report overrides for a Tray component.
// @Tags tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Tray"
// @Param siteId query string true "ID of Site" Format(uuid)
// @Param type query string true "Tray type" Enums(Compute,NVSwitch,PowerShelf)
// @Success 200 {array} model.APIMachineHealthReportEntry
// @Router /v2/org/{org}/nico/tray/{id}/health-report [get]
func (h GetAllTrayHealthReportHandler) Handle(c echo.Context) error {
	return handleTrayHealthReport(c, h.trayHealthReportHandler, trayHealthReportList)
}

// Handle godoc
// @Summary Insert Tray Health Report
// @Description Add or update a health report override for a Tray component.
// @Tags tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Tray"
// @Param request body model.APITrayHealthReportEntryRequest true "Tray health report"
// @Success 200 {object} model.APIMachineHealthReportEntry
// @Router /v2/org/{org}/nico/tray/{id}/health-report [put]
func (h CreateOrUpdateTrayHealthReportHandler) Handle(c echo.Context) error {
	return handleTrayHealthReport(c, h.trayHealthReportHandler, trayHealthReportInsert)
}

// Handle godoc
// @Summary Delete Tray Health Report
// @Description Delete a Tray component health report override by source.
// @Tags tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Tray"
// @Param source path string true "Health report source"
// @Param siteId query string true "ID of Site" Format(uuid)
// @Param type query string true "Tray type" Enums(Compute,NVSwitch,PowerShelf)
// @Success 204
// @Router /v2/org/{org}/nico/tray/{id}/health-report/{source} [delete]
func (h DeleteTrayHealthReportHandler) Handle(c echo.Context) error {
	return handleTrayHealthReport(c, h.trayHealthReportHandler, trayHealthReportRemove)
}

func handleTrayHealthReport(c echo.Context, h trayHealthReportHandler, action trayHealthReportAction) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TrayHealthReport", string(action), c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	query := trayHealthReportQuery{}
	apiReq := model.APIMachineHealthReportEntryRequest{}
	if action == trayHealthReportInsert {
		err := common.ValidateKnownQueryParams(c.QueryParams(), struct{}{})
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
		}
		request := model.APITrayHealthReportEntryRequest{}
		err = c.Bind(&request)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
		}
		err = request.Validate()
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
		}
		query.SiteID = request.SiteID
		query.Type = request.Type
		apiReq = request.APIMachineHealthReportEntryRequest
	} else {
		err := common.ValidateKnownQueryParams(c.QueryParams(), trayHealthReportQuery{})
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
		}
		query.SiteID = c.QueryParam("siteId")
		query.Type = c.QueryParam("type")
	}
	if query.SiteID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId is required", nil)
	}
	_, typeIsValid := model.APIToProtoComponentTypeName[query.Type]
	if !typeIsValid {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("type must be one of %s", strings.Join(model.ValidTrayTypeNames(), ", ")), nil)
	}

	resourceID := c.Param("id")
	if resourceID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tray ID was not specified in URL", nil)
	}
	if err := validateKnownTrayIDNamespace(resourceID, query.Type); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	source := c.Param("source")
	if action == trayHealthReportRemove && source == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tray health report source was not specified in URL", nil)
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

	method, coreReq, coreResp, insertedEntry, err := trayHealthReportCoreRequest(action, query.Type, resourceID, source, apiReq, dbUser)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to prepare Tray health report operation")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to prepare Tray health report operation", nil)
	}

	operationLogger := logger.With().
		Str("resource", "Tray").
		Str("action", string(action)).
		Str("resource_id", resourceID).
		Str("site_id", siteID).
		Str("core_method", method).
		Logger()
	logSource := source
	if action == trayHealthReportInsert {
		logSource = apiReq.Source
	}
	if logSource != "" {
		operationLogger = operationLogger.With().Str("source", logSource).Logger()
	}
	operationLogger.Info().Msg("Proxying Tray health report operation to Core")

	apiErr := common.ExecuteCoreGRPC(ctx, stc, method, coreReq, coreResp, siteID)
	if apiErr != nil {
		logAPIError(operationLogger, apiErr, "Failed to proxy Tray health report operation to Core")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	switch action {
	case trayHealthReportList:
		apiResp := []model.APIMachineHealthReportEntry{}
		for _, entry := range coreResp.(*corev1.ListHealthReportResponse).GetHealthReportEntries() {
			apiEntry := model.APIMachineHealthReportEntry{}
			apiEntry.FromProto(entry)
			apiResp = append(apiResp, apiEntry)
		}
		return c.JSON(http.StatusOK, apiResp)
	case trayHealthReportInsert:
		apiResp := model.APIMachineHealthReportEntry{}
		apiResp.FromProto(insertedEntry)
		return c.JSON(http.StatusOK, apiResp)
	case trayHealthReportRemove:
		return c.NoContent(http.StatusNoContent)
	default:
		operationLogger.Error().Msg("Unsupported Tray health report action reached response handling")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to process Tray health report operation", nil)
	}
}

func validateKnownTrayIDNamespace(resourceID, trayType string) error {
	var idType string
	switch {
	case strings.HasPrefix(resourceID, computeTrayIDPrefix):
		idType = "Compute"
	case strings.HasPrefix(resourceID, nvSwitchTrayIDPrefix):
		idType = "NVSwitch"
	case strings.HasPrefix(resourceID, powerShelfTrayIDPrefix):
		idType = "PowerShelf"
	}
	if idType != "" && idType != trayType {
		return fmt.Errorf("Tray ID namespace %s does not match type %s", idType, trayType)
	}
	return nil
}

func trayHealthReportCoreRequest(action trayHealthReportAction, trayType, resourceID, source string, apiReq model.APIMachineHealthReportEntryRequest, dbUser *cdbm.User) (string, proto.Message, proto.Message, *corev1.HealthReportEntry, error) {
	var entry *corev1.HealthReportEntry
	if action == trayHealthReportInsert {
		entry = apiReq.ToHealthReportEntryProto(dbUser)
	}

	switch trayType {
	case "Compute":
		switch action {
		case trayHealthReportList:
			return corev1.Forge_ListMachineHealthReports_FullMethodName, &corev1.MachineId{Id: resourceID}, &corev1.ListHealthReportResponse{}, nil, nil
		case trayHealthReportInsert:
			return corev1.Forge_InsertMachineHealthReport_FullMethodName, &corev1.InsertMachineHealthReportRequest{MachineId: &corev1.MachineId{Id: resourceID}, HealthReportEntry: entry}, nil, entry, nil
		case trayHealthReportRemove:
			return corev1.Forge_RemoveMachineHealthReport_FullMethodName, &corev1.RemoveMachineHealthReportRequest{MachineId: &corev1.MachineId{Id: resourceID}, Source: source}, nil, nil, nil
		default:
			return "", nil, nil, nil, fmt.Errorf("unsupported Tray health report action %q", action)
		}
	case "NVSwitch":
		switch action {
		case trayHealthReportList:
			return corev1.Forge_ListSwitchHealthReports_FullMethodName, &corev1.ListSwitchHealthReportsRequest{SwitchId: &corev1.SwitchId{Id: resourceID}}, &corev1.ListHealthReportResponse{}, nil, nil
		case trayHealthReportInsert:
			return corev1.Forge_InsertSwitchHealthReport_FullMethodName, &corev1.InsertSwitchHealthReportRequest{SwitchId: &corev1.SwitchId{Id: resourceID}, HealthReportEntry: entry}, nil, entry, nil
		case trayHealthReportRemove:
			return corev1.Forge_RemoveSwitchHealthReport_FullMethodName, &corev1.RemoveSwitchHealthReportRequest{SwitchId: &corev1.SwitchId{Id: resourceID}, Source: source}, nil, nil, nil
		default:
			return "", nil, nil, nil, fmt.Errorf("unsupported Tray health report action %q", action)
		}
	case "PowerShelf":
		switch action {
		case trayHealthReportList:
			return corev1.Forge_ListPowerShelfHealthReports_FullMethodName, &corev1.ListPowerShelfHealthReportsRequest{PowerShelfId: &corev1.PowerShelfId{Id: resourceID}}, &corev1.ListHealthReportResponse{}, nil, nil
		case trayHealthReportInsert:
			return corev1.Forge_InsertPowerShelfHealthReport_FullMethodName, &corev1.InsertPowerShelfHealthReportRequest{PowerShelfId: &corev1.PowerShelfId{Id: resourceID}, HealthReportEntry: entry}, nil, entry, nil
		case trayHealthReportRemove:
			return corev1.Forge_RemovePowerShelfHealthReport_FullMethodName, &corev1.RemovePowerShelfHealthReportRequest{PowerShelfId: &corev1.PowerShelfId{Id: resourceID}, Source: source}, nil, nil, nil
		default:
			return "", nil, nil, nil, fmt.Errorf("unsupported Tray health report action %q", action)
		}
	default:
		return "", nil, nil, nil, fmt.Errorf("unsupported Tray type %q", trayType)
	}
}
