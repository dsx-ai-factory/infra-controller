// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"

	"github.com/labstack/echo/v4"
	"google.golang.org/protobuf/types/known/emptypb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
)

// attestationJobAction is a job-wide waive or fail request. Core applies it to
// every retained device row of the job, across every machine the job covers.
type attestationJobAction struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	// name is the handler name for tracing, such as "Waive"
	name string
	op   attestationOperation
}

// handle admits, authorizes and proxies one job action.
func (a attestationJobAction) handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AttestationJob", a.name, c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiErr := admitAttestationQuery(c)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var apiReq model.APIAttestationJobActionRequest
	err := decodeClosedJSONObject(c, &apiReq)
	if err != nil {
		return sendAttestationAdmissionError(c, err)
	}
	apiReq.JobID = c.Param("jobId")
	err = apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Attestation Job action request data", err)
	}

	stc, siteID, apiErr := authorizeAttestationSite(ctx, logger, a.dbSession, a.scp, org, dbUser, apiReq.SiteID)
	if apiErr != nil {
		return apiErr.Send(c)
	}

	apiErr = a.op.call(ctx, logger, stc, siteID, apiReq.ToProto(), &emptypb.Empty{})
	if apiErr != nil {
		return apiErr.Send(c)
	}
	return c.JSON(http.StatusAccepted, model.APIMessageResponse{Message: a.op.accepted})
}

// WaiveAttestationJobHandler waives every retained device of an attestation job.
type WaiveAttestationJobHandler struct {
	action attestationJobAction
}

// NewWaiveAttestationJobHandler returns an attestation job waive handler.
func NewWaiveAttestationJobHandler(dbSession *cdb.Session, scp *sc.ClientPool) WaiveAttestationJobHandler {
	return WaiveAttestationJobHandler{action: attestationJobAction{dbSession: dbSession, scp: scp, name: "Waive", op: opWaiveAttestationJob}}
}

// Handle godoc
// @Summary Waive Attestation Job
// @Description Request that every retained device of the job, on every machine it covers, end as Waived. Unfinished devices finish after BMC cleanup; completed devices keep their historical phase while the latest request decides the effective result.
// @Tags attestation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param jobId path string true "ID of the attestation job"
// @Param request body model.APIAttestationJobActionRequest true "Site of the job"
// @Success 202 {object} model.APIMessageResponse
// @Router /v2/org/{org}/nico/attestation-job/{jobId}/waive [post]
func (h WaiveAttestationJobHandler) Handle(c echo.Context) error {
	return h.action.handle(c)
}

// FailAttestationJobHandler fails every retained device of an attestation job.
type FailAttestationJobHandler struct {
	action attestationJobAction
}

// NewFailAttestationJobHandler returns an attestation job fail handler.
func NewFailAttestationJobHandler(dbSession *cdb.Session, scp *sc.ClientPool) FailAttestationJobHandler {
	return FailAttestationJobHandler{action: attestationJobAction{dbSession: dbSession, scp: scp, name: "Fail", op: opFailAttestationJob}}
}

// Handle godoc
// @Summary Fail Attestation Job
// @Description Request that every retained device of the job, on every machine it covers, end as Failed. Unfinished devices finish after BMC cleanup; completed devices keep their historical phase while the latest request decides the effective result.
// @Tags attestation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param jobId path string true "ID of the attestation job"
// @Param request body model.APIAttestationJobActionRequest true "Site of the job"
// @Success 202 {object} model.APIMessageResponse
// @Router /v2/org/{org}/nico/attestation-job/{jobId}/fail [post]
func (h FailAttestationJobHandler) Handle(c echo.Context) error {
	return h.action.handle(c)
}
