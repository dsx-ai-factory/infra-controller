// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"errors"
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

func firmwareRequestBindError(c echo.Context, err error) error {
	var unknownFieldError *model.UnknownFirmwareAuthenticationFieldError
	if errors.As(err, &unknownFieldError) {
		return cutil.NewAPIErrorResponse(
			c,
			http.StatusBadRequest,
			unknownFieldError.Error(),
			nil,
		)
	}
	return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
}
