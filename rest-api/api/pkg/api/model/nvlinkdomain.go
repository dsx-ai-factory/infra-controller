// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"github.com/google/uuid"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// APIBatchUpdateNVLinkDomainPowerStateRequest is the request body for power
// controlling one or more NVLink Domains.
type APIBatchUpdateNVLinkDomainPowerStateRequest struct {
	SiteID          string   `json:"siteId"`
	NVLinkDomainIDs []string `json:"domainIds"`
	State           string   `json:"state"`
	// RuleID pins every task spawned by this batch to one Operation Rule.
	// See APIUpdatePowerStateRequest.RuleID for semantics.
	RuleID *string `json:"ruleId"`
	// OverrideReadinessCheck applies the readiness bypass to every spawned task.
	// See APIUpdatePowerStateRequest for semantics.
	OverrideReadinessCheck bool `json:"overrideReadinessCheck"`
}

// Validate checks the NVLink Domain IDs and power-control fields.
func (r *APIBatchUpdateNVLinkDomainPowerStateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error("siteId is required"),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.NVLinkDomainIDs,
			validation.Required.Error("domainIds must contain at least one NVLink Domain ID"),
			validation.By(validateNVLinkDomainIDs)),
		validation.Field(&r.State,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(validPowerControlStatesAny...).Error(
				fmt.Sprintf("must be one of %v", ValidPowerControlStates))),
		validation.Field(&r.RuleID, validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

// APINVLinkDomainFirmwareUpdateRequest updates firmware on one NVLink Domain. It
// omits tray sub-target selection because an NVLink Domain resolves to whole racks.
type APINVLinkDomainFirmwareUpdateRequest struct {
	SiteID string `json:"siteId"`
	// Version is the target firmware version. When nil or empty, the operation
	// uses the default firmware version for each targeted component.
	Version *string `json:"version"`
	// RuleID pins the firmware operation to one Operation Rule.
	// See APIUpdateFirmwareRequest.RuleID for semantics.
	RuleID *string `json:"ruleId"`
	// OverrideReadinessCheck bypasses readiness checks for the operation.
	// See APIUpdateFirmwareRequest for semantics.
	OverrideReadinessCheck bool `json:"overrideReadinessCheck"`
	// OverrideVersionCheck overrides firmware version-based checks for the
	// operation. See APIUpdateFirmwareRequest for semantics.
	OverrideVersionCheck bool `json:"overrideVersionCheck"`
}

// Validate checks the firmware-update fields.
func (r *APINVLinkDomainFirmwareUpdateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error("siteId is required"),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.RuleID, validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

// APIBatchNVLinkDomainFirmwareUpdateRequest is the request body for updating
// firmware on one or more NVLink Domains.
type APIBatchNVLinkDomainFirmwareUpdateRequest struct {
	SiteID          string   `json:"siteId"`
	NVLinkDomainIDs []string `json:"domainIds"`
	// Version is the target firmware version. When nil or empty, the operation
	// uses the default firmware version for each targeted component.
	Version *string `json:"version"`
	// RuleID pins every task spawned by this batch to one Operation Rule.
	// See APIUpdateFirmwareRequest.RuleID for semantics.
	RuleID *string `json:"ruleId"`
	// OverrideReadinessCheck applies the readiness bypass to every spawned task.
	// See APIUpdateFirmwareRequest for semantics.
	OverrideReadinessCheck bool `json:"overrideReadinessCheck"`
	// OverrideVersionCheck applies the firmware version-check override to every
	// spawned task. See APIUpdateFirmwareRequest for semantics.
	OverrideVersionCheck bool `json:"overrideVersionCheck"`
}

// Validate checks the NVLink Domain IDs and firmware-update fields.
func (r *APIBatchNVLinkDomainFirmwareUpdateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error("siteId is required"),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.NVLinkDomainIDs,
			validation.Required.Error("domainIds must contain at least one NVLink Domain ID"),
			validation.By(validateNVLinkDomainIDs)),
		validation.Field(&r.RuleID, validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

func validateNVLinkDomainIDs(value any) error {
	nvLinkDomainIDs := value.([]string)
	errs := validation.Errors{}
	seen := make(map[uuid.UUID]struct{}, len(nvLinkDomainIDs))
	for i, nvLinkDomainID := range nvLinkDomainIDs {
		parsed, err := uuid.Parse(nvLinkDomainID)
		if err != nil || parsed == uuid.Nil {
			errs[fmt.Sprintf("%d", i)] = validation.NewError(
				"validation_domain_id",
				"NVLink Domain ID must be a non-zero UUID",
			)
			continue
		}
		if _, exists := seen[parsed]; exists {
			errs[fmt.Sprintf("%d", i)] = validation.NewError(
				"validation_duplicate_domain_id",
				fmt.Sprintf("duplicates NVLink Domain ID %s", nvLinkDomainID),
			)
			continue
		}
		seen[parsed] = struct{}{}
	}

	return errs.Filter()
}

// ValidateNVLinkDomainID requires one non-zero NVLink Domain UUID.
func ValidateNVLinkDomainID(nvLinkDomainID string) error {
	parsed, err := uuid.Parse(nvLinkDomainID)
	if err != nil || parsed == uuid.Nil {
		return fmt.Errorf("NVLink Domain ID must be a non-zero UUID")
	}

	return nil
}

// NVLinkDomainTargetSpec builds a Flow operation target spec from NVLink Domain UUIDs.
func NVLinkDomainTargetSpec(nvLinkDomainIDs []string) *flowv1.OperationTargetSpec {
	targets := make([]*flowv1.NVLDomainTarget, 0, len(nvLinkDomainIDs))
	for _, nvLinkDomainID := range nvLinkDomainIDs {
		targets = append(targets, &flowv1.NVLDomainTarget{
			Identifier: &flowv1.NVLDomainTarget_Id{
				Id: &flowv1.UUID{Id: nvLinkDomainID},
			},
		})
	}

	return &flowv1.OperationTargetSpec{
		Targets: &flowv1.OperationTargetSpec_NvlDomains{
			NvlDomains: &flowv1.NVLDomainTargets{Targets: targets},
		},
	}
}
