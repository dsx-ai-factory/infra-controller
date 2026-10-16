// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"fmt"
	"net/http"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"

	common "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

// GetAllVPCAttachmentHandler retrieves the safe attachment view for a Tenant-owned VPC.
type GetAllVPCAttachmentHandler struct {
	dbSession *cdb.Session
}

// NewGetAllVPCAttachmentHandler initializes a VPC attachment list handler.
func NewGetAllVPCAttachmentHandler(dbSession *cdb.Session) GetAllVPCAttachmentHandler {
	return GetAllVPCAttachmentHandler{dbSession: dbSession}
}

// Handle godoc
// @Summary Retrieve all attachments for a VPC
// @Description Retrieve current Interface attachments for a Tenant-owned VPC
// @Tags vpc
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param vpcId path string true "ID of VPC"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {array} model.APIVpcAttachment
// @Router /v2/org/{org}/nico/vpc/{vpcId}/attachment [get]
func (h GetAllVPCAttachmentHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("VPCAttachment", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	pageRequest := pagination.PageRequest{}
	err = common.ValidateKnownQueryParams(c.QueryParams(), pageRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	err = pageRequest.Validate(cdbm.InterfaceOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	tenant, err := common.GetTenantForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	vpcID, err := uuid.Parse(c.Param("vpcId"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid VPC ID in URL", nil)
	}
	vpc, err := cdbm.NewVpcDAO(h.dbSession).GetByID(ctx, nil, vpcID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find VPC with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving VPC from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC", nil)
	}
	if vpc.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "VPC does not belong to current Tenant", nil)
	}

	dbInterfaces, total, err := cdbm.NewInterfaceDAO(h.dbSession).GetAll(
		ctx,
		nil,
		cdbm.InterfaceFilterInput{VpcID: &vpc.ID},
		cdbp.PageInput{
			Limit:   pageRequest.Limit,
			Offset:  pageRequest.Offset,
			OrderBy: pageRequest.OrderBy,
		},
		[]string{cdbm.InstanceRelationName},
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving VPC attachments from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC attachments", nil)
	}

	attachments := make([]model.APIVpcAttachment, 0, len(dbInterfaces))
	for i := range dbInterfaces {
		if dbInterfaces[i].Instance == nil {
			logger.Error().Str("interfaceID", dbInterfaces[i].ID.String()).Msg("VPC attachment is missing its Instance relation")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to build VPC attachment response", nil)
		}
		var attachment model.APIVpcAttachment
		attachment.FromDBModel(&dbInterfaces[i], vpc.ID)
		attachments = append(attachments, attachment)
	}

	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	return c.JSON(http.StatusOK, attachments)
}
