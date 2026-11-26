// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"slices"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// MachineAttestationTriggerMaxRedfishTimeoutSecs bounds a trigger's Redfish
	// waits so scheduling finishes inside the proxy's activity deadline.
	MachineAttestationTriggerMaxRedfishTimeoutSecs = 10
	// MachineAttestationStatusMaxRedfishTimeoutSecs bounds a live assessment so
	// it finishes inside the proxy's activity deadline.
	MachineAttestationStatusMaxRedfishTimeoutSecs = 35
)

// AttestationSchedulingOutcome says why a trigger scheduled what it did.
type AttestationSchedulingOutcome string

// Scheduling outcomes reported by the profile-based trigger.
const (
	AttestationSchedulingOutcomeScheduled            AttestationSchedulingOutcome = "Scheduled"
	AttestationSchedulingOutcomeAttestationDisabled  AttestationSchedulingOutcome = "AttestationDisabled"
	AttestationSchedulingOutcomeNoAttestersFound     AttestationSchedulingOutcome = "NoAttestersFound"
	AttestationSchedulingOutcomePolicyMatchedNothing AttestationSchedulingOutcome = "PolicyMatchedNothing"
	AttestationSchedulingOutcomeClassNotRecorded     AttestationSchedulingOutcome = "ClassNotRecorded"
	AttestationSchedulingOutcomeNoProfile            AttestationSchedulingOutcome = "NoProfile"
	AttestationSchedulingOutcomePartiallySatisfied   AttestationSchedulingOutcome = "PartiallySatisfied"
)

// APIMachineAttestationTriggerRequest schedules profile-based attestation of
// one machine's devices.
type APIMachineAttestationTriggerRequest struct {
	// MachineID comes from the request path
	MachineID string `json:"-"`
	// RedfishTimeoutSecs bounds each Redfish wait while discovering attesters
	RedfishTimeoutSecs *uint32 `json:"redfishTimeoutSecs"`
}

// Validate checks the request before any lookup.
func (r *APIMachineAttestationTriggerRequest) Validate() error {
	return validation.Errors{
		"machineId": validation.Validate(r.MachineID, validation.By(validateAttestationMachineID)),
		"redfishTimeoutSecs": validation.Validate(r.RedfishTimeoutSecs,
			validation.NotNil.Error(validationErrorValueRequired),
			validation.By(uint32PtrRange(1, MachineAttestationTriggerMaxRedfishTimeoutSecs)),
		),
	}.Filter()
}

// ToProto converts a validated request. No attester override is sent: this
// route schedules only what the machine's profile selects.
func (r *APIMachineAttestationTriggerRequest) ToProto() *corev1.SpdmMachineAttestationTriggerRequest {
	return &corev1.SpdmMachineAttestationTriggerRequest{
		MachineId:          &corev1.MachineId{Id: r.MachineID},
		RedfishTimeoutSecs: *r.RedfishTimeoutSecs,
	}
}

// APIMachineAttestationTrigger is the scheduling receipt for one trigger.
type APIMachineAttestationTrigger struct {
	// MachineID is the machine the receipt describes
	MachineID string `json:"machineId"`
	// Outcome says why the trigger scheduled what it did
	Outcome AttestationSchedulingOutcome `json:"outcome"`
	// ResolvedHardwareClass is the class recorded for the machine's BMC; empty when none is recorded
	ResolvedHardwareClass string `json:"resolvedHardwareClass"`
	// UsedAnyFallback is true when the "any" profile supplied the policy
	UsedAnyFallback bool `json:"usedAnyFallback"`
	// ProfileVersion is the version of the profile that decided this trigger; null when no profile applied
	ProfileVersion *string `json:"profileVersion"`
	// DevicesUnderAttestation counts the devices scheduled by this trigger
	DevicesUnderAttestation int32 `json:"devicesUnderAttestation"`
	// JobID identifies the scheduled job; null when nothing was scheduled
	JobID *string `json:"jobId"`
	// StartedAt is when the scheduled devices were written; null when nothing was scheduled
	StartedAt *time.Time `json:"startedAt"`
	// AttesterIDs are the scheduled attesters in ascending order
	AttesterIDs []string `json:"attesterIds"`
}

// FromProto sets the receipt from Core for the requested machine, failing
// when the receipt contradicts itself or the profile-only request.
func (m *APIMachineAttestationTrigger) FromProto(machineID string, p *corev1.SpdmMachineAttestationTriggerResponse) error {
	if p == nil {
		return errors.New("trigger receipt is missing")
	}
	if p.GetMachineId().GetId() != machineID {
		return errors.New("trigger receipt is for a different machine")
	}
	if p.AttesterIdRegex != nil {
		return errors.New("trigger receipt reports an attester override that was not requested")
	}
	out := APIMachineAttestationTrigger{
		MachineID:               machineID,
		ResolvedHardwareClass:   p.GetResolvedHardwareClass(),
		UsedAnyFallback:         p.GetUsedAnyFallback(),
		ProfileVersion:          p.ProfileVersion,
		DevicesUnderAttestation: p.GetDevicesUnderAttestation(),
		AttesterIDs:             slices.Clone(p.GetAttesterIds()),
	}
	if out.AttesterIDs == nil {
		out.AttesterIDs = []string{}
	}

	scheduled := false
	versioned := true
	switch p.GetOutcome() {
	case corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_SCHEDULED:
		out.Outcome, scheduled = AttestationSchedulingOutcomeScheduled, true
	case corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_PARTIALLY_SATISFIED:
		out.Outcome, scheduled = AttestationSchedulingOutcomePartiallySatisfied, true
	case corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_ATTESTATION_DISABLED:
		out.Outcome = AttestationSchedulingOutcomeAttestationDisabled
	case corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_NO_ATTESTERS_FOUND:
		out.Outcome = AttestationSchedulingOutcomeNoAttestersFound
	case corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_POLICY_MATCHED_NOTHING:
		out.Outcome = AttestationSchedulingOutcomePolicyMatchedNothing
	case corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_CLASS_NOT_RECORDED:
		out.Outcome, versioned = AttestationSchedulingOutcomeClassNotRecorded, false
	case corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_NO_PROFILE:
		out.Outcome, versioned = AttestationSchedulingOutcomeNoProfile, false
	default:
		return fmt.Errorf("trigger receipt has unsupported outcome %d", p.GetOutcome())
	}

	if versioned && p.GetProfileVersion() == "" {
		return fmt.Errorf("trigger receipt with outcome %s has no profile version", out.Outcome)
	}
	if !versioned && (p.ProfileVersion != nil || out.UsedAnyFallback) {
		return fmt.Errorf("trigger receipt with outcome %s names a deciding profile", out.Outcome)
	}

	if !scheduled {
		if out.DevicesUnderAttestation != 0 || len(out.AttesterIDs) != 0 || p.StartedAt != nil || p.JobId != nil {
			return fmt.Errorf("trigger receipt with outcome %s reports scheduled work", out.Outcome)
		}
		*m = out
		return nil
	}

	if out.DevicesUnderAttestation <= 0 || int(out.DevicesUnderAttestation) != len(out.AttesterIDs) {
		return errors.New("trigger receipt device count does not match its attesters")
	}
	if !slices.IsSorted(out.AttesterIDs) || slices.Contains(out.AttesterIDs, "") {
		return errors.New("trigger receipt attesters are not sorted nonempty IDs")
	}
	startedAt, ok := requiredTime(p.GetStartedAt())
	if !ok {
		return errors.New("trigger receipt has no valid start time")
	}
	out.StartedAt = &startedAt
	jobID, err := uuid.Parse(p.GetJobId().GetValue())
	if err != nil || jobID == uuid.Nil {
		return errors.New("trigger receipt has no valid job ID")
	}
	jobIDStr := jobID.String()
	out.JobID = &jobIDStr
	*m = out
	return nil
}

// AttesterSelectionStatus says why current policy and inventory select the
// attesters a live assessment covers.
type AttesterSelectionStatus string

// Attester selection statuses.
const (
	AttesterSelectionStatusSelected             AttesterSelectionStatus = "Selected"
	AttesterSelectionStatusPartiallySatisfied   AttesterSelectionStatus = "PartiallySatisfied"
	AttesterSelectionStatusDisabled             AttesterSelectionStatus = "Disabled"
	AttesterSelectionStatusNoAttestersFound     AttesterSelectionStatus = "NoAttestersFound"
	AttesterSelectionStatusPolicyMatchedNothing AttesterSelectionStatus = "PolicyMatchedNothing"
	AttesterSelectionStatusClassNotRecorded     AttesterSelectionStatus = "ClassNotRecorded"
	AttesterSelectionStatusNoProfile            AttesterSelectionStatus = "NoProfile"
)

// FromProto sets the selection status from Core, failing for the unset
// sentinel and numbers this REST does not know.
func (s *AttesterSelectionStatus) FromProto(p corev1.SpdmAttesterSelectionStatus) error {
	switch p {
	case corev1.SpdmAttesterSelectionStatus_SPDM_ATTESTER_SELECTION_STATUS_SELECTED:
		*s = AttesterSelectionStatusSelected
	case corev1.SpdmAttesterSelectionStatus_SPDM_ATTESTER_SELECTION_STATUS_PARTIALLY_SATISFIED:
		*s = AttesterSelectionStatusPartiallySatisfied
	case corev1.SpdmAttesterSelectionStatus_SPDM_ATTESTER_SELECTION_STATUS_DISABLED:
		*s = AttesterSelectionStatusDisabled
	case corev1.SpdmAttesterSelectionStatus_SPDM_ATTESTER_SELECTION_STATUS_NO_ATTESTERS_FOUND:
		*s = AttesterSelectionStatusNoAttestersFound
	case corev1.SpdmAttesterSelectionStatus_SPDM_ATTESTER_SELECTION_STATUS_POLICY_MATCHED_NOTHING:
		*s = AttesterSelectionStatusPolicyMatchedNothing
	case corev1.SpdmAttesterSelectionStatus_SPDM_ATTESTER_SELECTION_STATUS_CLASS_NOT_RECORDED:
		*s = AttesterSelectionStatusClassNotRecorded
	case corev1.SpdmAttesterSelectionStatus_SPDM_ATTESTER_SELECTION_STATUS_NO_PROFILE:
		*s = AttesterSelectionStatusNoProfile
	default:
		return fmt.Errorf("unknown attester selection status %d", p)
	}
	return nil
}

// APIMachineAttestationStatusRequest assesses one machine live.
type APIMachineAttestationStatusRequest struct {
	// MachineID comes from the request path
	MachineID string `json:"-"`
	// RedfishTimeoutSecs bounds the whole assessment; Core uses 30 when omitted
	RedfishTimeoutSecs *uint32 `query:"redfishTimeoutSecs" json:"-"`
}

// Validate checks the request before any lookup.
func (r *APIMachineAttestationStatusRequest) Validate() error {
	return validation.Errors{
		"machineId": validation.Validate(r.MachineID, validation.By(validateAttestationMachineID)),
		"redfishTimeoutSecs": validation.Validate(r.RedfishTimeoutSecs,
			validation.By(uint32PtrRange(1, MachineAttestationStatusMaxRedfishTimeoutSecs)),
		),
	}.Filter()
}

// ToProto converts a validated request.
func (r *APIMachineAttestationStatusRequest) ToProto() *corev1.ListMachineAttestationStatusesRequest {
	return &corev1.ListMachineAttestationStatusesRequest{
		Target: &corev1.ListMachineAttestationStatusesRequest_Machine{
			Machine: &corev1.SpdmMachineAttestationMachineRequest{
				MachineId:          &corev1.MachineId{Id: r.MachineID},
				RedfishTimeoutSecs: r.RedfishTimeoutSecs,
			},
		},
	}
}

// APIMachineAttestationStatus is one machine's attestation status.
type APIMachineAttestationStatus struct {
	// MachineID is the machine the status describes
	MachineID string `json:"machineId"`
	// AttestationStatus aggregates the covered devices; null when unknown or when no attester is selected
	AttestationStatus *AttestationStatus `json:"attestationStatus"`
	// AttesterSelectionStatus explains the live selection; null for fleet predictions
	AttesterSelectionStatus *AttesterSelectionStatus `json:"attesterSelectionStatus"`
}

// fromProto sets the row from Core, validating present enums.
func (m *APIMachineAttestationStatus) fromProto(p *corev1.SpdmMachineAttestationStatus) error {
	if p.GetMachineId().GetId() == "" {
		return errors.New("attestation status has no machine ID")
	}
	out := APIMachineAttestationStatus{MachineID: p.GetMachineId().GetId()}
	if p.AttestationStatus != nil {
		var status AttestationStatus
		err := status.FromProto(p.GetAttestationStatus())
		if err != nil {
			return err
		}
		out.AttestationStatus = &status
	}
	if p.AttesterSelectionStatus != nil {
		var selection AttesterSelectionStatus
		err := selection.FromProto(p.GetAttesterSelectionStatus())
		if err != nil {
			return err
		}
		out.AttesterSelectionStatus = &selection
	}
	*m = out
	return nil
}

// FromProto sets the live assessment for the requested machine. Core must
// return exactly that machine, with a selection reason and no continuation.
func (m *APIMachineAttestationStatus) FromProto(machineID string, p *corev1.ListMachineAttestationStatusesResponse) error {
	if p == nil || len(p.GetStatuses()) != 1 || p.GetNextPageToken() != "" {
		return errors.New("live attestation status must contain exactly one machine")
	}
	var out APIMachineAttestationStatus
	err := out.fromProto(p.GetStatuses()[0])
	if err != nil {
		return err
	}
	if out.MachineID != machineID {
		return errors.New("live attestation status is for a different machine")
	}
	if out.AttesterSelectionStatus == nil {
		return errors.New("live attestation status has no attester selection status")
	}
	*m = out
	return nil
}
