// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// APIDpuExtensionServiceDeploymentRequest is the data structure to capture request to deploy a DPU Extension Service
type APIDpuExtensionServiceDeploymentRequest struct {
	// DpuExtensionServiceID is the ID of the DPU Extension Service to deploy
	DpuExtensionServiceID string `json:"dpuExtensionServiceId"`
	// Version is the version of the DPU Extension Service to deploy
	Version string `json:"version"`
}

// Validate ensures that the values passed in request are acceptable
func (desdr APIDpuExtensionServiceDeploymentRequest) Validate() error {
	return validation.ValidateStruct(&desdr,
		validation.Field(&desdr.DpuExtensionServiceID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&desdr.Version,
			validation.Required.Error(validationErrorValueRequired)),
	)
}

// APIDpuExtensionServiceDeployment is the data structure to capture API representation of a DpuExtensionServiceDeployment
type APIDpuExtensionServiceDeployment struct {
	// ID is the unique UUID v4 identifier for the DpuExtensionServiceDeployment
	ID string `json:"id"`
	// DpuExtensionService is the summary of the DPU Extension Service
	DpuExtensionService *APIDpuExtensionServiceSummary `json:"dpuExtensionService"`
	// Version is the deployed version of the DPU Extension Service
	Version string `json:"version"`
	// Status is the deployment status
	Status string `json:"status"`
	// Created indicates when this deployment was created
	Created time.Time `json:"created"`
	// Updated indicates when this deployment was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIDpuExtensionServiceDeployment creates and returns a new APIDpuExtensionServiceDeployment object
func NewAPIDpuExtensionServiceDeployment(dbdesd *cdbm.DpuExtensionServiceDeployment) *APIDpuExtensionServiceDeployment {
	apiDpuExtensionServiceDeployment := &APIDpuExtensionServiceDeployment{
		ID:      dbdesd.ID.String(),
		Version: dbdesd.Version,
		Status:  dbdesd.Status,
		Created: dbdesd.Created,
		Updated: dbdesd.Updated,
	}

	if dbdesd.DpuExtensionService != nil {
		apiDpuExtensionServiceDeployment.DpuExtensionService = NewAPIDpuExtensionServiceSummary(dbdesd.DpuExtensionService)
	}

	return apiDpuExtensionServiceDeployment
}
