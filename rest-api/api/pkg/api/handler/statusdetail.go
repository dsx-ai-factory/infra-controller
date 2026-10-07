// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	cerr "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
)

func handleEntityStatusDetails(ctx context.Context, echoCtx echo.Context, dbSession *cdb.Session, entityID string, logger zerolog.Logger) ([]model.APIStatusDetail, error) {
	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err := echoCtx.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return nil, cerr.NewAPIErrorResponse(echoCtx, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.StatusDetailOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return nil, cerr.NewAPIErrorResponse(echoCtx, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(dbSession)
	dbSds, total, serr := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{entityID}}, cdbp.PageInput{Offset: pageRequest.Offset, Limit: pageRequest.Limit, OrderBy: pageRequest.OrderBy})
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Status Details")
		return nil, cerr.NewAPIErrorResponse(echoCtx, http.StatusInternalServerError, "Failed to retrieve Status Details", nil)
	}

	// Create response
	apiSds := []model.APIStatusDetail{}
	for _, sd := range dbSds {
		apiSds = append(apiSds, model.NewAPIStatusDetail(sd))
	}

	// Create pagination response header
	pageReponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageReponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return nil, cerr.NewAPIErrorResponse(echoCtx, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}

	echoCtx.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	return apiSds, nil
}
