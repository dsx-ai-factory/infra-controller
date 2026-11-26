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

// GetAllAttestationJobHandler lists a Site's retained attestation jobs.
type GetAllAttestationJobHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetAllAttestationJobHandler returns an attestation job list handler.
func NewGetAllAttestationJobHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetAllAttestationJobHandler {
	return GetAllAttestationJobHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Retrieve all Attestation Jobs
// @Description List one page of the Site's retained attestation jobs with the given aggregate status, ordered by job ID. Pass nextPageToken as pageToken, with the same status, for the next page.
// @Tags attestation
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param status query string false "Aggregate status; InProgress when omitted"
// @Param pageSize query integer false "Maximum jobs per page, 1-100"
// @Param pageToken query string false "nextPageToken from the previous page"
// @Param orderBy query string false "Only JOB_ID_ASC is supported"
// @Success 200 {object} model.APIAttestationJobList
// @Router /v2/org/{org}/nico/attestation-job [get]
func (h GetAllAttestationJobHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AttestationJob", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIAttestationJobListRequest
	apiErr := admitAttestationQuery(c, &apiReq)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	err := apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Attestation Job list request data", err)
	}

	stc, siteID, apiErr := authorizeAttestationSite(ctx, logger, h.dbSession, h.scp, org, dbUser, apiReq.SiteID)
	if apiErr != nil {
		return apiErr.Send(c)
	}

	coreResp := &corev1.SpdmListAttestationResponse{}
	apiErr = opListAttestationJobs.call(ctx, logger, stc, siteID, apiReq.ToProto(), coreResp)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var resp model.APIAttestationJobList
	err = resp.FromProto(coreResp)
	if err != nil {
		return opListAttestationJobs.invalidReplyError(logger, err).Send(c)
	}
	return c.JSON(http.StatusOK, resp)
}

// GetAttestationJobHandler reads one retained attestation job.
type GetAttestationJobHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetAttestationJobHandler returns an attestation job read handler.
func NewGetAttestationJobHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetAttestationJobHandler {
	return GetAttestationJobHandler{dbSession: dbSession, scp: scp}
}

// Handle godoc
// @Summary Retrieve Attestation Job
// @Description Read the device rows of one retained attestation job.
// @Tags attestation
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param jobId path string true "ID of the attestation job"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APIAttestationJob
// @Router /v2/org/{org}/nico/attestation-job/{jobId} [get]
func (h GetAttestationJobHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AttestationJob", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIAttestationJobRequest
	apiErr := admitAttestationQuery(c, &apiReq)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	apiReq.JobID = c.Param("jobId")
	err := apiReq.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Attestation Job request data", err)
	}

	stc, siteID, apiErr := authorizeAttestationSite(ctx, logger, h.dbSession, h.scp, org, dbUser, apiReq.SiteID)
	if apiErr != nil {
		return apiErr.Send(c)
	}

	coreResp := &corev1.SpdmGetAttestationResponse{}
	apiErr = opGetAttestationJob.call(ctx, logger, stc, siteID, apiReq.ToProto(), coreResp)
	if apiErr != nil {
		return apiErr.Send(c)
	}
	var resp model.APIAttestationJob
	err = resp.FromProto(apiReq.JobID, coreResp)
	if err != nil {
		return opGetAttestationJob.invalidReplyError(logger, err).Send(c)
	}
	return c.JSON(http.StatusOK, resp)
}
