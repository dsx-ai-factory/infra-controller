// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// Measured Boot trust approval types exposed by the REST API.
const (
	MeasuredBootApprovalTypeOneshot = "Oneshot"
	MeasuredBootApprovalTypePersist = "Persist"
)

// Selectors supported when deleting a machine trust approval.
const (
	MeasuredBootTrustedMachineSelectorApprovalID = "ApprovalId"
	MeasuredBootTrustedMachineSelectorMachineID  = "MachineId"
)

// Selectors supported when deleting a profile trust approval.
const (
	MeasuredBootTrustedProfileSelectorApprovalID = "ApprovalId"
	MeasuredBootTrustedProfileSelectorProfileID  = "ProfileId"
)

// APIMeasuredBootTrustedMachineCreateRequest creates a machine trust approval.
type APIMeasuredBootTrustedMachineCreateRequest struct {
	SiteID       string `json:"siteId"`
	MachineID    string `json:"machineId"`
	ApprovalType string `json:"approvalType"`
	PCRRegisters string `json:"pcrRegisters,omitempty"`
	Comments     string `json:"comments,omitempty"`
}

// APIMeasuredBootTrustedProfileCreateRequest creates a profile trust approval.
type APIMeasuredBootTrustedProfileCreateRequest struct {
	SiteID       string `json:"siteId"`
	ProfileID    string `json:"profileId"`
	ApprovalType string `json:"approvalType"`
	PCRRegisters string `json:"pcrRegisters,omitempty"`
	Comments     string `json:"comments,omitempty"`
}

// APIMeasuredBootTrustedMachineDeleteRequest deletes a machine trust approval.
type APIMeasuredBootTrustedMachineDeleteRequest struct {
	Selector string `json:"-"`
	ID       string `json:"-"`
}

// APIMeasuredBootTrustedProfileDeleteRequest deletes a profile trust approval.
type APIMeasuredBootTrustedProfileDeleteRequest struct {
	Selector string `json:"-"`
	ID       string `json:"-"`
}

// APIMeasuredBootTrustedMachine is a machine trust approval.
type APIMeasuredBootTrustedMachine struct {
	ApprovalID   string     `json:"approvalId"`
	MachineID    string     `json:"machineId"`
	ApprovalType string     `json:"approvalType"`
	PCRRegisters string     `json:"pcrRegisters"`
	Comments     string     `json:"comments"`
	Created      *time.Time `json:"created"`
}

// APIMeasuredBootTrustedProfile is a profile trust approval.
type APIMeasuredBootTrustedProfile struct {
	ApprovalID   string     `json:"approvalId"`
	ProfileID    string     `json:"profileId"`
	ApprovalType string     `json:"approvalType"`
	PCRRegisters string     `json:"pcrRegisters"`
	Comments     string     `json:"comments"`
	Created      *time.Time `json:"created"`
}

// Validate checks a machine trust approval request.
func (r *APIMeasuredBootTrustedMachineCreateRequest) Validate() error {
	if err := validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.MachineID, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&r.ApprovalType, validation.Required.Error(validationErrorValueRequired)),
	); err != nil {
		return err
	}
	if r.MachineID != "*" {
		if err := validation.Validate(r.MachineID, validationis.UUID.Error(validationErrorInvalidUUID)); err != nil {
			return fmt.Errorf("machineId: %w", err)
		}
	}
	return validateMeasuredBootApprovalType(r.ApprovalType)
}

// Validate checks a profile trust approval request.
func (r *APIMeasuredBootTrustedProfileCreateRequest) Validate() error {
	if err := validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.ProfileID, validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.ApprovalType, validation.Required.Error(validationErrorValueRequired)),
	); err != nil {
		return err
	}
	return validateMeasuredBootApprovalType(r.ApprovalType)
}

// Validate checks a machine trust approval deletion request.
func (r *APIMeasuredBootTrustedMachineDeleteRequest) Validate() error {
	if err := validation.ValidateStruct(r,
		validation.Field(&r.Selector,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(MeasuredBootTrustedMachineSelectorApprovalID, MeasuredBootTrustedMachineSelectorMachineID).
				Error(fmt.Sprintf("invalid selector %q (expected %q or %q)", r.Selector, MeasuredBootTrustedMachineSelectorApprovalID, MeasuredBootTrustedMachineSelectorMachineID)),
		),
		validation.Field(&r.ID, validation.Required.Error(validationErrorValueRequired)),
	); err != nil {
		return err
	}
	if r.Selector == MeasuredBootTrustedMachineSelectorApprovalID || r.ID != "*" {
		if err := validation.Validate(r.ID, validationis.UUID.Error(validationErrorInvalidUUID)); err != nil {
			return fmt.Errorf("id: %w", err)
		}
	}
	return nil
}

// Validate checks a profile trust approval deletion request.
func (r *APIMeasuredBootTrustedProfileDeleteRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.Selector,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(MeasuredBootTrustedProfileSelectorApprovalID, MeasuredBootTrustedProfileSelectorProfileID).
				Error(fmt.Sprintf("invalid selector %q (expected %q or %q)", r.Selector, MeasuredBootTrustedProfileSelectorApprovalID, MeasuredBootTrustedProfileSelectorProfileID)),
		),
		validation.Field(&r.ID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID),
		),
	)
}

// ToProto converts a validated machine trust approval request to its Core message.
func (r *APIMeasuredBootTrustedMachineCreateRequest) ToProto() *corev1.AddMeasurementTrustedMachineRequest {
	return &corev1.AddMeasurementTrustedMachineRequest{
		MachineId:    r.MachineID,
		ApprovalType: measuredBootApprovalTypeToProto(r.ApprovalType),
		PcrRegisters: r.PCRRegisters,
		Comments:     r.Comments,
	}
}

// ToProto converts a validated profile trust approval request to its Core message.
func (r *APIMeasuredBootTrustedProfileCreateRequest) ToProto() *corev1.AddMeasurementTrustedProfileRequest {
	req := &corev1.AddMeasurementTrustedProfileRequest{
		ProfileId:    &corev1.MeasurementSystemProfileId{Value: r.ProfileID},
		ApprovalType: measuredBootApprovalTypeToProto(r.ApprovalType),
	}
	if r.PCRRegisters != "" {
		req.PcrRegisters = &r.PCRRegisters
	}
	if r.Comments != "" {
		req.Comments = &r.Comments
	}
	return req
}

// ToProto converts a validated machine trust approval deletion request to its Core message.
func (r *APIMeasuredBootTrustedMachineDeleteRequest) ToProto() *corev1.RemoveMeasurementTrustedMachineRequest {
	if r.Selector == MeasuredBootTrustedMachineSelectorApprovalID {
		return &corev1.RemoveMeasurementTrustedMachineRequest{
			Selector: &corev1.RemoveMeasurementTrustedMachineRequest_ApprovalId{
				ApprovalId: &corev1.MeasurementApprovedMachineId{Value: r.ID},
			},
		}
	}
	return &corev1.RemoveMeasurementTrustedMachineRequest{
		Selector: &corev1.RemoveMeasurementTrustedMachineRequest_MachineId{MachineId: r.ID},
	}
}

// ToProto converts a validated profile trust approval deletion request to its Core message.
func (r *APIMeasuredBootTrustedProfileDeleteRequest) ToProto() *corev1.RemoveMeasurementTrustedProfileRequest {
	if r.Selector == MeasuredBootTrustedProfileSelectorApprovalID {
		return &corev1.RemoveMeasurementTrustedProfileRequest{
			Selector: &corev1.RemoveMeasurementTrustedProfileRequest_ApprovalId{
				ApprovalId: &corev1.MeasurementApprovedProfileId{Value: r.ID},
			},
		}
	}
	return &corev1.RemoveMeasurementTrustedProfileRequest{
		Selector: &corev1.RemoveMeasurementTrustedProfileRequest_ProfileId{
			ProfileId: &corev1.MeasurementSystemProfileId{Value: r.ID},
		},
	}
}

// NewAPIMeasuredBootTrustedMachine creates an API model from a Core machine trust record.
func NewAPIMeasuredBootTrustedMachine(record *corev1.MeasurementApprovedMachineRecordPb) *APIMeasuredBootTrustedMachine {
	if record == nil {
		return nil
	}
	resp := &APIMeasuredBootTrustedMachine{}
	resp.FromProto(record)
	return resp
}

// FromProto converts one Core machine trust record.
func (r *APIMeasuredBootTrustedMachine) FromProto(record *corev1.MeasurementApprovedMachineRecordPb) {
	if record == nil {
		return
	}
	*r = APIMeasuredBootTrustedMachine{
		ApprovalID:   record.GetApprovalId().GetValue(),
		MachineID:    record.GetMachineId(),
		ApprovalType: measuredBootApprovalTypeFromProto(record.GetApprovalType()),
		PCRRegisters: record.GetPcrRegisters(),
		Comments:     record.GetComments(),
	}
	if ts := record.GetTs(); ts != nil {
		created := ts.AsTime().UTC()
		r.Created = &created
	}
}

// NewAPIMeasuredBootTrustedProfile creates an API model from a Core profile trust record.
func NewAPIMeasuredBootTrustedProfile(record *corev1.MeasurementApprovedProfileRecordPb) *APIMeasuredBootTrustedProfile {
	if record == nil {
		return nil
	}
	resp := &APIMeasuredBootTrustedProfile{}
	resp.FromProto(record)
	return resp
}

// FromProto converts one Core profile trust record.
func (r *APIMeasuredBootTrustedProfile) FromProto(record *corev1.MeasurementApprovedProfileRecordPb) {
	if record == nil {
		return
	}
	*r = APIMeasuredBootTrustedProfile{
		ApprovalID:   record.GetApprovalId().GetValue(),
		ProfileID:    record.GetProfileId().GetValue(),
		ApprovalType: measuredBootApprovalTypeFromProto(record.GetApprovalType()),
		PCRRegisters: record.GetPcrRegisters(),
		Comments:     record.GetComments(),
	}
	if ts := record.GetTs(); ts != nil {
		created := ts.AsTime().UTC()
		r.Created = &created
	}
}

// APIMeasuredBootTrustedMachines is a list of machine trust approvals.
type APIMeasuredBootTrustedMachines []*APIMeasuredBootTrustedMachine

// FromProto converts Core machine trust records.
func (r *APIMeasuredBootTrustedMachines) FromProto(records []*corev1.MeasurementApprovedMachineRecordPb) {
	result := make(APIMeasuredBootTrustedMachines, 0, len(records))
	for _, record := range records {
		result = append(result, NewAPIMeasuredBootTrustedMachine(record))
	}
	*r = result
}

// APIMeasuredBootTrustedProfiles is a list of profile trust approvals.
type APIMeasuredBootTrustedProfiles []*APIMeasuredBootTrustedProfile

// FromProto converts Core profile trust records.
func (r *APIMeasuredBootTrustedProfiles) FromProto(records []*corev1.MeasurementApprovedProfileRecordPb) {
	result := make(APIMeasuredBootTrustedProfiles, 0, len(records))
	for _, record := range records {
		result = append(result, NewAPIMeasuredBootTrustedProfile(record))
	}
	*r = result
}

func validateMeasuredBootApprovalType(approvalType string) error {
	switch approvalType {
	case MeasuredBootApprovalTypeOneshot, MeasuredBootApprovalTypePersist:
		return nil
	default:
		return fmt.Errorf("invalid approvalType %q (expected %q or %q)", approvalType, MeasuredBootApprovalTypeOneshot, MeasuredBootApprovalTypePersist)
	}
}

func measuredBootApprovalTypeToProto(approvalType string) corev1.MeasurementApprovedTypePb {
	if approvalType == MeasuredBootApprovalTypePersist {
		return corev1.MeasurementApprovedTypePb_Persist
	}
	return corev1.MeasurementApprovedTypePb_Oneshot
}

func measuredBootApprovalTypeFromProto(approvalType corev1.MeasurementApprovedTypePb) string {
	if approvalType == corev1.MeasurementApprovedTypePb_Persist {
		return MeasuredBootApprovalTypePersist
	}
	return MeasuredBootApprovalTypeOneshot
}
