// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

// maxMachineExternalValidationTextBytes matches the limit Core applies to
// attempt details and hold removal reasons.
const maxMachineExternalValidationTextBytes = 4096

var validationErrorMachineExternalValidationTextLength = fmt.Sprintf("must not exceed %d bytes", maxMachineExternalValidationTextBytes)

// MachineExternalValidationHoldStatus is the status of an external validation hold
type MachineExternalValidationHoldStatus string

const (
	MachineExternalValidationHoldStatusPending         MachineExternalValidationHoldStatus = "Pending"
	MachineExternalValidationHoldStatusAttemptOpen     MachineExternalValidationHoldStatus = "AttemptOpen"
	MachineExternalValidationHoldStatusAwaitingCleanup MachineExternalValidationHoldStatus = "AwaitingCleanup"
	MachineExternalValidationHoldStatusRecovery        MachineExternalValidationHoldStatus = "Recovery"
)

// MachineExternalValidationAttemptStatus is the status of an external validation attempt
type MachineExternalValidationAttemptStatus string

const (
	MachineExternalValidationAttemptStatusOpen      MachineExternalValidationAttemptStatus = "Open"
	MachineExternalValidationAttemptStatusPassed    MachineExternalValidationAttemptStatus = "Passed"
	MachineExternalValidationAttemptStatusFailed    MachineExternalValidationAttemptStatus = "Failed"
	MachineExternalValidationAttemptStatusCancelled MachineExternalValidationAttemptStatus = "Cancelled"
)

// MachineExternalValidationAllocationStatus is the progress of the validation Instance allocation for an attempt
type MachineExternalValidationAllocationStatus string

const (
	MachineExternalValidationAllocationStatusNotStarted MachineExternalValidationAllocationStatus = "NotStarted"
	MachineExternalValidationAllocationStatusCreating   MachineExternalValidationAllocationStatus = "Creating"
	MachineExternalValidationAllocationStatusCreated    MachineExternalValidationAllocationStatus = "Created"
)

// MachineExternalValidationOutcome is the result recorded when completing an external validation attempt
type MachineExternalValidationOutcome string

const (
	MachineExternalValidationOutcomePassed    MachineExternalValidationOutcome = "Passed"
	MachineExternalValidationOutcomeFailed    MachineExternalValidationOutcome = "Failed"
	MachineExternalValidationOutcomeCancelled MachineExternalValidationOutcome = "Cancelled"
)

var validMachineExternalValidationOutcomes = []interface{}{
	MachineExternalValidationOutcomePassed,
	MachineExternalValidationOutcomeFailed,
	MachineExternalValidationOutcomeCancelled,
}

// APIMachineExternalValidationHold is the API representation of an external validation hold.
// While a hold is active, the Machine can only be allocated through an external validation attempt.
type APIMachineExternalValidationHold struct {
	// ID is the ID of the hold
	ID string `json:"id"`
	// MachineID is the ID of the held Machine
	MachineID string `json:"machineId"`
	// SiteID is the ID of the Site the Machine belongs to
	SiteID string `json:"siteId"`
	// TenantID is the ID of the Tenant configured to validate the Machine, nil when no REST Tenant has the configured org
	TenantID *string `json:"tenantId"`
	// Status is the status of the hold
	Status MachineExternalValidationHoldStatus `json:"status"`
	// RecoveryReason describes why the hold needs operator attention, set only when Status is Recovery
	RecoveryReason *string `json:"recoveryReason"`
	// Attempt is the current attempt, nil when the hold has none
	Attempt *APIMachineExternalValidationAttempt `json:"attempt"`
	// Created is the time the hold was created
	Created time.Time `json:"created"`
}

// APIMachineExternalValidationAttempt is the API representation of an external validation attempt
type APIMachineExternalValidationAttempt struct {
	// ID is the ID of the attempt, sent as externalValidationRequestId when creating the validation Instance
	ID string `json:"id"`
	// HoldID is the ID of the hold the attempt belongs to
	HoldID string `json:"holdId"`
	// IdempotencyKey is the key the attempt was started with
	IdempotencyKey string `json:"idempotencyKey"`
	// Status is the status of the attempt
	Status MachineExternalValidationAttemptStatus `json:"status"`
	// AllocationStatus is the progress of the validation Instance allocation
	AllocationStatus MachineExternalValidationAllocationStatus `json:"allocationStatus"`
	// InstanceID is the ID of the validation Instance, nil until the Instance is created
	InstanceID *string `json:"instanceId"`
	// Details is the text recorded when the attempt was completed or cancelled
	Details *string `json:"details"`
	// Created is the time the attempt was started
	Created time.Time `json:"created"`
}

// APIGetAllMachineExternalValidationHoldRequest binds query parameters for GET /machine/external-validation/hold
type APIGetAllMachineExternalValidationHoldRequest struct {
	// SiteID is the ID of the Site to list holds for
	SiteID string `query:"siteId"`
	// MachineID limits the list to the hold of one Machine
	MachineID string `query:"machineId"`
}

// Validate ensures that the values passed in request are acceptable
func (r *APIGetAllMachineExternalValidationHoldRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

// APIMachineExternalValidationAttemptCreateRequest is the request to start an external validation attempt
type APIMachineExternalValidationAttemptCreateRequest struct {
	// IdempotencyKey identifies this start request, replaying it returns the attempt it started
	IdempotencyKey string `json:"idempotencyKey"`
}

// Validate ensures that the values passed in request are acceptable
func (r *APIMachineExternalValidationAttemptCreateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.IdempotencyKey,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

// APIMachineExternalValidationAttemptCompleteRequest is the request to record the result of an external validation attempt
type APIMachineExternalValidationAttemptCompleteRequest struct {
	// Outcome is the result of the external validation
	Outcome MachineExternalValidationOutcome `json:"outcome"`
	// InstanceID is the ID of the validation Instance created for the attempt
	InstanceID string `json:"instanceId"`
	// Details is optional text or a reference to the external result
	Details *string `json:"details"`
}

// Validate ensures that the values passed in request are acceptable
func (r *APIMachineExternalValidationAttemptCompleteRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.Outcome,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(validMachineExternalValidationOutcomes...).Error(fmt.Sprintf("must be one of %v", validMachineExternalValidationOutcomes))),
		validation.Field(&r.InstanceID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.Details,
			validation.Length(0, maxMachineExternalValidationTextBytes).Error(validationErrorMachineExternalValidationTextLength)),
	)
}

// APIMachineExternalValidationAttemptCancelRequest is the request to cancel an external validation attempt before its Instance is allocated
type APIMachineExternalValidationAttemptCancelRequest struct {
	// Details is the reason for cancelling the attempt
	Details string `json:"details"`
}

// Validate ensures that the values passed in request are acceptable
func (r *APIMachineExternalValidationAttemptCancelRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.Details,
			validation.Required.Error(validationErrorValueRequired),
			validation.Length(1, maxMachineExternalValidationTextBytes).Error(validationErrorMachineExternalValidationTextLength)),
	)
}

// APIMachineExternalValidationHoldDeleteRequest is the request to remove an external validation hold
type APIMachineExternalValidationHoldDeleteRequest struct {
	// Reason is the audit reason for removing the hold
	Reason string `json:"reason"`
}

// Validate ensures that the values passed in request are acceptable
func (r *APIMachineExternalValidationHoldDeleteRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.Reason,
			validation.Required.Error(validationErrorValueRequired),
			validation.Length(1, maxMachineExternalValidationTextBytes).Error(validationErrorMachineExternalValidationTextLength)),
	)
}
