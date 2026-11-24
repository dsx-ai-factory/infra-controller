// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"errors"
	"net/http"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	tclient "go.temporal.io/sdk/client"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// attestationMachine is an owned machine on a Registered Site.
type attestationMachine struct {
	machine *cdbm.Machine
	stc     tclient.Client
	siteID  string
}

// resolveAttestationMachine requires a Provider Admin whose provider owns the
// machine, which must be present on a Registered Site. A missing or foreign
// machine is 404 so ownership is not disclosed. checkMachine runs before the
// Site client is acquired.
func resolveAttestationMachine(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, scp *sc.ClientPool, org string, user *cdbm.User, machineID string, checkMachine func(*cdbm.Machine) *cutil.APIError) (*attestationMachine, *cutil.APIError) {
	if user == nil {
		logger.Error().Msg("invalid User object found in request context")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	provider, apiErr := common.IsProvider(ctx, logger, dbSession, org, user, false)
	if apiErr != nil {
		return nil, apiErr
	}
	machine, err := cdbm.NewMachineDAO(dbSession).GetByID(ctx, nil, machineID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return nil, cutil.NewAPIError(http.StatusNotFound, "Could not find Machine with specified ID", nil)
		}
		logger.Error().Err(err).Msg("failed to retrieve Machine for attestation")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Machine details, DB error", nil)
	}
	if machine.InfrastructureProviderID != provider.ID {
		return nil, cutil.NewAPIError(http.StatusNotFound, "Could not find Machine with specified ID", nil)
	}
	if machine.Site == nil || machine.Site.ID != machine.SiteID {
		logger.Error().Msg("Machine has no consistent Site relation")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Site details for Machine, DB error", nil)
	}
	if machine.IsMissingOnSite {
		return nil, cutil.NewAPIError(http.StatusBadRequest, "Machine is missing on Site", nil)
	}
	if checkMachine != nil {
		apiErr = checkMachine(machine)
		if apiErr != nil {
			return nil, apiErr
		}
	}
	stc, apiErr := common.GetRegisteredSiteClient(logger, scp, machine.Site)
	if apiErr != nil {
		return nil, apiErr
	}
	return &attestationMachine{machine: machine, stc: stc, siteID: machine.Site.ID.String()}, nil
}

// TriggerMachineAttestationHandler schedules attestation of a machine's
// devices selected by its profile.
type TriggerMachineAttestationHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewTriggerMachineAttestationHandler returns a machine attestation trigger handler.
func NewTriggerMachineAttestationHandler(dbSession *cdb.Session, scp *sc.ClientPool) TriggerMachineAttestationHandler {
	return TriggerMachineAttestationHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Trigger Machine Attestation
// @Description Schedule SPDM attestation of the devices the machine's attestation profile selects. The receipt reports the job, or why nothing was scheduled. Machines assigned to an Instance are rejected.
// @Tags attestation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param machineId path string true "ID of Machine"
// @Param request body model.APIMachineAttestationTriggerRequest true "Trigger options"
// @Success 200 {object} model.APIMachineAttestationTrigger
// @Router /v2/org/{org}/nico/machine/{machineId}/attestation [post]
func (h TriggerMachineAttestationHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineAttestation", "Trigger", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiErr := admitAttestationQuery(c)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	machineID, apiErr := attestationMachineID(c)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var apiReq model.APIMachineAttestationTriggerRequest
	err := decodeClosedJSONObject(c, &apiReq)
	if err != nil {
		return sendAttestationAdmissionError(c, err)
	}
	apiReq.MachineID = machineID
	err = apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Machine Attestation trigger request data", err)
	}

	target, apiErr := resolveAttestationMachine(ctx, logger, h.dbSession, h.scp, org, dbUser, apiReq.MachineID, func(m *cdbm.Machine) *cutil.APIError {
		if m.IsAssigned {
			return cutil.NewAPIError(http.StatusBadRequest, "Machine is assigned to an Instance and cannot be attested on demand", nil)
		}
		return nil
	})
	if apiErr != nil {
		return apiErr.Send(c)
	}

	coreResp := &corev1.SpdmMachineAttestationTriggerResponse{}
	apiErr = opTriggerMachineAttestation.call(ctx, logger, target.stc, target.siteID, apiReq.ToProto(), coreResp)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var resp model.APIMachineAttestationTrigger
	err = resp.FromProto(apiReq.MachineID, coreResp)
	if err != nil {
		return opTriggerMachineAttestation.invalidReplyError(logger, err).Send(c)
	}
	return c.JSON(http.StatusOK, resp)
}

// GetMachineAttestationStatusHandler assesses one machine's attestation live.
type GetMachineAttestationStatusHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetMachineAttestationStatusHandler returns a live machine attestation status handler.
func NewGetMachineAttestationStatusHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetMachineAttestationStatusHandler {
	return GetMachineAttestationStatusHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Retrieve Machine Attestation Status
// @Description Assess the machine's attestation using its current profile and BMC component inventory. The assessment is read-only.
// @Tags attestation
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param machineId path string true "ID of Machine"
// @Param redfishTimeoutSecs query integer false "Bound on the whole assessment in seconds, 1-35; 30 when omitted"
// @Success 200 {object} model.APIMachineAttestationStatus
// @Router /v2/org/{org}/nico/machine/{machineId}/attestation/status [get]
func (h GetMachineAttestationStatusHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineAttestation", "GetStatus", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIMachineAttestationStatusRequest
	apiErr := admitAttestationQuery(c, &apiReq)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	apiReq.MachineID, apiErr = attestationMachineID(c)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	err := apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Machine Attestation status request data", err)
	}

	target, apiErr := resolveAttestationMachine(ctx, logger, h.dbSession, h.scp, org, dbUser, apiReq.MachineID, nil)
	if apiErr != nil {
		return apiErr.Send(c)
	}

	coreResp := &corev1.ListMachineAttestationStatusesResponse{}
	apiErr = opGetMachineAttestationStatus.call(ctx, logger, target.stc, target.siteID, apiReq.ToProto(), coreResp)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var resp model.APIMachineAttestationStatus
	err = resp.FromProto(apiReq.MachineID, coreResp)
	if err != nil {
		return opGetMachineAttestationStatus.invalidReplyError(logger, err).Send(c)
	}
	return c.JSON(http.StatusOK, resp)
}

// GetAllMachineAttestationStatusHandler lists predicted attestation status
// per machine across a Site.
type GetAllMachineAttestationStatusHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetAllMachineAttestationStatusHandler returns a fleet attestation status handler.
func NewGetAllMachineAttestationStatusHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetAllMachineAttestationStatusHandler {
	return GetAllMachineAttestationStatusHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Retrieve all Machine Attestation Statuses
// @Description List one page of per-machine attestation status predicted from each machine's latest retained controller job, ordered by machine ID. Pass nextPageToken as pageToken, with the same filters, for the next page.
// @Tags attestation
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param status query string false "Keep machines with this status"
// @Param machineType query string false "Host or Dpu"
// @Param pageSize query integer false "Maximum machines per page, 1-100"
// @Param pageToken query string false "nextPageToken from the previous page"
// @Param orderBy query string false "Only MACHINE_ID_ASC is supported"
// @Success 200 {object} model.APIAttestationFleetPage
// @Router /v2/org/{org}/nico/attestation-status [get]
func (h GetAllMachineAttestationStatusHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineAttestation", "GetAllStatus", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIAttestationFleetRequest
	apiErr := admitAttestationQuery(c, &apiReq)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	err := apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Machine Attestation status list request data", err)
	}

	stc, siteID, apiErr := authorizeAttestationSite(ctx, logger, h.dbSession, h.scp, org, dbUser, apiReq.SiteID)
	if apiErr != nil {
		return apiErr.Send(c)
	}

	coreResp := &corev1.ListMachineAttestationStatusesResponse{}
	apiErr = opListMachineAttestationStatuses.call(ctx, logger, stc, siteID, apiReq.ToProto(), coreResp)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var resp model.APIAttestationFleetPage
	err = resp.FromProto(coreResp)
	if err != nil {
		return opListMachineAttestationStatuses.invalidReplyError(logger, err).Send(c)
	}
	return c.JSON(http.StatusOK, resp)
}
