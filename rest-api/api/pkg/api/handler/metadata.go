// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
)

// MetadataHandler is an API handler to return system information about the API
type MetadataHandler struct{}

// NewMetadataHandler creates and returns a new handler
func NewMetadataHandler() MetadataHandler {
	return MetadataHandler{}
}

// Handle godoc
// @Summary Returns system information about the API
// @Description Returns system information about the API
// @Tags metadata
// @Accept */*
// @Produce json
// @Success 200 {object} model.APIMetadata
// @Router /v2/org/{org}/nico/metadata [get]
func (mdh MetadataHandler) Handle(c echo.Context) error {
	amd := model.NewAPIMetadata()
	return c.JSON(http.StatusOK, amd)
}
