// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"errors"
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// GetMachineSpectrumXMultiplaneHandler retrieves configuration and independent DPU observations.
type GetMachineSpectrumXMultiplaneHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetMachineSpectrumXMultiplaneHandler returns the machine multiplane read handler.
func NewGetMachineSpectrumXMultiplaneHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetMachineSpectrumXMultiplaneHandler {
	return GetMachineSpectrumXMultiplaneHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Get Machine Spectrum-X Multiplane
// @Description Retrieve desired rail/plane maintenance configuration and separate observations for each BlueField-4 DPU. For Infrastructure Providers: Org must have an Infrastructure Provider entity and own the Site that the Machine belongs to. User must have authorization role with `PROVIDER_ADMIN` or `PROVIDER_VIEWER` suffix.
// @Tags machine
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param machineId path string true "ID of Machine"
// @Success 200 {object} model.APIMachineSpectrumXMultiplane
// @Router /v2/org/{org}/nico/machine/{machineId}/spectrumx-multiplane [get]
func (h GetMachineSpectrumXMultiplaneHandler) Handle(c echo.Context) error {
	org, user, ctx, logger, span := common.SetupHandler("MachineSpectrumXMultiplane", "Get", c)
	if span != nil {
		defer span.End()
	}
	if user == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	provider, _, apiErr := common.IsProviderOrTenant(ctx, logger, h.dbSession, org, user, true, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if provider == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Machine Spectrum-X multiplane API operations require a Provider role", nil)
	}
	err := common.ValidateKnownQueryParams(c.QueryParams(), struct{}{})
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	machineID := c.Param("id")
	if machineID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Machine ID was not specified in URL", nil)
	}
	machine, err := cdbm.NewMachineDAO(h.dbSession).GetByID(ctx, nil, machineID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Machine with specified ID", nil)
		}
		logger.Error().Err(err).Msg("Failed to retrieve Machine for Spectrum-X multiplane")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Machine details, DB error", nil)
	}
	if machine.InfrastructureProviderID != provider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Machine with specified ID", nil)
	}
	if machine.IsMissingOnSite {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Machine is missing on site", nil)
	}
	if machine.Site == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Machine, DB error", nil)
	}
	if machine.Site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state", nil)
	}
	stc, err := h.scp.GetClientByID(machine.Site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to retrieve workflow client for Machine site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve workflow client for Site", nil)
	}
	coreResp := &corev1.MachineSpxMultiplane{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_GetMachineSpxMultiplane_FullMethodName,
		&corev1.MachineSpxMultiplaneRequest{MachineId: &corev1.MachineId{Id: machineID}}, coreResp, machine.Site.ID.String())
	if apiErr != nil {
		logAPIError(logger, apiErr, "Failed to proxy Machine Spectrum-X multiplane request")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	var response model.APIMachineSpectrumXMultiplane
	response.FromProto(coreResp)
	return c.JSON(http.StatusOK, response)
}

// UpdateMachineSpectrumXMultiplaneHandler patches desired maintenance configuration.
type UpdateMachineSpectrumXMultiplaneHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewUpdateMachineSpectrumXMultiplaneHandler returns the machine multiplane update handler.
func NewUpdateMachineSpectrumXMultiplaneHandler(dbSession *cdb.Session, scp *sc.ClientPool) UpdateMachineSpectrumXMultiplaneHandler {
	return UpdateMachineSpectrumXMultiplaneHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Update Machine Spectrum-X Multiplane
// @Description Atomically change selected rails and planes. Assigned Machines require acknowledgeAttachedInstance=true. True maintenanceMode disables the selected links. The response confirms saved configuration; DPU observations converge asynchronously. For Infrastructure Providers: Org must have an Infrastructure Provider entity and own the Site that the Machine belongs to. User must have authorization role with `PROVIDER_ADMIN` suffix.
// @Tags machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param machineId path string true "ID of Machine"
// @Param request body model.APIMachineSpectrumXMultiplaneUpdateRequest true "Maintenance selection"
// @Success 200 {object} model.APIMachineSpectrumXMultiplane
// @Router /v2/org/{org}/nico/machine/{machineId}/spectrumx-multiplane [patch]
func (h UpdateMachineSpectrumXMultiplaneHandler) Handle(c echo.Context) error {
	org, user, ctx, logger, span := common.SetupHandler("MachineSpectrumXMultiplane", "Update", c)
	if span != nil {
		defer span.End()
	}
	if user == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	provider, _, apiErr := common.IsProviderOrTenant(ctx, logger, h.dbSession, org, user, false, nil)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if provider == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Machine Spectrum-X multiplane API operations require a Provider role", nil)
	}
	err := common.ValidateKnownQueryParams(c.QueryParams(), struct{}{})
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	machineID := c.Param("id")
	if machineID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Machine ID was not specified in URL", nil)
	}
	var req model.APIMachineSpectrumXMultiplaneUpdateRequest
	err = c.Bind(&req)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	req.MachineID = machineID
	err = req.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	machine, err := cdbm.NewMachineDAO(h.dbSession).GetByID(ctx, nil, machineID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Machine with specified ID", nil)
		}
		logger.Error().Err(err).Msg("Failed to retrieve Machine for Spectrum-X multiplane")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Machine details, DB error", nil)
	}
	if machine.InfrastructureProviderID != provider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Machine with specified ID", nil)
	}
	if machine.IsMissingOnSite {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Machine is missing on site", nil)
	}
	if machine.IsAssigned && (req.AcknowledgeAttachedInstance == nil || !*req.AcknowledgeAttachedInstance) {
		logger.Error().Msg("Machine is currently in use by an Instance and cannot update Spectrum-X multiplane maintenance without acknowledging the attached Instance")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Machine is currently in use by an Instance, set acknowledgeAttachedInstance to true to proceed", nil)
	}
	if machine.Site == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Machine, DB error", nil)
	}
	if machine.Site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state", nil)
	}
	stc, err := h.scp.GetClientByID(machine.Site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to retrieve workflow client for Machine site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve workflow client for Site", nil)
	}
	coreResp := &corev1.MachineSpxMultiplane{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_UpdateMachineSpxMultiplaneMaintenance_FullMethodName,
		req.ToProto(), coreResp, machine.Site.ID.String())
	if apiErr != nil {
		logAPIError(logger, apiErr, "Failed to proxy Machine Spectrum-X multiplane request")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	var response model.APIMachineSpectrumXMultiplane
	response.FromProto(coreResp)
	return c.JSON(http.StatusOK, response)
}
