// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

// APIGetAllDpuMachineHealthReportRequest binds query parameters for GET /dpu/:id/health-report.
type APIGetAllDpuMachineHealthReportRequest struct {
	SiteID string `query:"siteId"`
}

// Validate checks the DPU Machine health report list query shape.
func (agadmhrr *APIGetAllDpuMachineHealthReportRequest) Validate() error {
	return validation.ValidateStruct(agadmhrr,
		validation.Field(&agadmhrr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID),
		),
	)
}
