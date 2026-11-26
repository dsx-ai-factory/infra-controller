// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"strings"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// AttestationJobOrderByJobIDAsc is the only supported, and the default, order
// for listing attestation jobs.
const AttestationJobOrderByJobIDAsc = "JOB_ID_ASC"

// APIAttestationJobListRequest lists a Site's retained attestation jobs by
// aggregate status, one cursor page at a time.
type APIAttestationJobListRequest struct {
	// SiteID is the Site to read from
	SiteID string `query:"siteId" json:"siteId"`
	// Status selects jobs by aggregate status; InProgress when omitted
	Status *AttestationStatus `query:"status" json:"status"`
	// PageSize caps the page; the Site's limit (default 100) when omitted
	PageSize *uint32 `query:"pageSize" json:"pageSize"`
	// PageToken continues from a previous page's nextPageToken
	PageToken string `query:"pageToken" json:"pageToken"`
	// OrderBy accepts only JOB_ID_ASC, the default
	OrderBy string `query:"orderBy" json:"orderBy"`
}

// Validate checks the request before any lookup.
func (r *APIAttestationJobListRequest) Validate() error {
	return validation.Errors{
		"siteId":   validation.Validate(r.SiteID, requiredUUIDRules...),
		"status":   validation.Validate(r.Status, oneOf(attestationStatuses...)),
		"pageSize": validation.Validate(r.PageSize, validation.By(uint32PtrRange(1, pagination.MaxPageSize))),
		"orderBy":  validation.Validate(r.OrderBy, validation.In(AttestationJobOrderByJobIDAsc).Error(fmt.Sprintf("must be %s", AttestationJobOrderByJobIDAsc))),
	}.Filter()
}

// ToProto converts a validated request.
func (r *APIAttestationJobListRequest) ToProto() *corev1.SpdmListAttestationRequest {
	selector := corev1.SpdmListAttestationRequestSelector_SPDM_LIST_IN_PROGRESS
	if r.Status != nil {
		switch *r.Status {
		case AttestationStatusWaived:
			selector = corev1.SpdmListAttestationRequestSelector_SPDM_LIST_WAIVED
		case AttestationStatusFailed:
			selector = corev1.SpdmListAttestationRequestSelector_SPDM_LIST_FAILED
		case AttestationStatusPassed:
			selector = corev1.SpdmListAttestationRequestSelector_SPDM_LIST_PASSED
		}
	}
	out := &corev1.SpdmListAttestationRequest{Selector: selector, PageToken: r.PageToken}
	if r.PageSize != nil {
		out.PageSize = *r.PageSize
	}
	return out
}

// APIAttestationJobSummary is one retained job's aggregate status.
type APIAttestationJobSummary struct {
	// JobID identifies the job
	JobID string `json:"jobId"`
	// MachineIDs are the distinct machines the job affects
	MachineIDs []string `json:"machineIds"`
	// AttestationStatus aggregates the job's devices
	AttestationStatus AttestationStatus `json:"attestationStatus"`
}

// APIAttestationJobList is one cursor page of jobs.
type APIAttestationJobList struct {
	// Jobs are ordered by ascending job ID
	Jobs []APIAttestationJobSummary `json:"jobs"`
	// NextPageToken continues the listing; empty on the last page
	NextPageToken string `json:"nextPageToken"`
}

// FromProto sets the page from Core, failing on a missing ID or unknown status.
func (m *APIAttestationJobList) FromProto(p *corev1.SpdmListAttestationResponse) error {
	out := APIAttestationJobList{Jobs: make([]APIAttestationJobSummary, 0, len(p.GetStatuses())), NextPageToken: p.GetNextPageToken()}
	for _, ps := range p.GetStatuses() {
		jobID, err := uuid.Parse(ps.GetJobId().GetValue())
		if err != nil {
			return errors.New("attestation job has no valid job ID")
		}
		job := APIAttestationJobSummary{JobID: jobID.String(), MachineIDs: make([]string, 0, len(ps.GetMachineIds()))}
		err = job.AttestationStatus.FromProto(ps.GetAttestationStatus())
		if err != nil {
			return err
		}
		for _, mid := range ps.GetMachineIds() {
			if mid.GetId() == "" {
				return errors.New("attestation job lists an empty machine ID")
			}
			job.MachineIDs = append(job.MachineIDs, mid.GetId())
		}
		out.Jobs = append(out.Jobs, job)
	}
	*m = out
	return nil
}

// APIAttestationJobRequest names one retained job on a Site.
type APIAttestationJobRequest struct {
	// SiteID is the Site holding the job
	SiteID string `query:"siteId" json:"siteId"`
	// JobID comes from the request path
	JobID string `json:"-"`
}

// Validate checks the request before any lookup.
func (r *APIAttestationJobRequest) Validate() error {
	return validation.Errors{
		"siteId": validation.Validate(r.SiteID, requiredUUIDRules...),
		"jobId":  validation.Validate(r.JobID, requiredUUIDRules...),
	}.Filter()
}

// ToProto converts a validated request.
func (r *APIAttestationJobRequest) ToProto() *corev1.UUID {
	return &corev1.UUID{Value: r.JobID}
}

// AttestationDevicePhase is a device's attestation progress, projected from
// Core's internal state without its task IDs or failure payloads.
type AttestationDevicePhase string

// Device phases. Unknown covers states this REST does not recognize.
const (
	AttestationDevicePhaseFetchMetadata             AttestationDevicePhase = "FetchMetadata"
	AttestationDevicePhaseFetchCertificate          AttestationDevicePhase = "FetchCertificate"
	AttestationDevicePhaseTriggerEvidenceCollection AttestationDevicePhase = "TriggerEvidenceCollection"
	AttestationDevicePhasePollEvidenceCollection    AttestationDevicePhase = "PollEvidenceCollection"
	AttestationDevicePhaseWaitForTaskCancellation   AttestationDevicePhase = "WaitForTaskCancellation"
	AttestationDevicePhaseNrasVerification          AttestationDevicePhase = "NrasVerification"
	AttestationDevicePhaseApplyAppraisalPolicy      AttestationDevicePhase = "ApplyAppraisalPolicy"
	AttestationDevicePhasePassed                    AttestationDevicePhase = "Passed"
	AttestationDevicePhaseFailed                    AttestationDevicePhase = "Failed"
	AttestationDevicePhaseWaived                    AttestationDevicePhase = "Waived"
	AttestationDevicePhaseUnknown                   AttestationDevicePhase = "Unknown"
)

var knownAttestationDevicePhases = []AttestationDevicePhase{
	AttestationDevicePhaseFetchMetadata,
	AttestationDevicePhaseFetchCertificate,
	AttestationDevicePhaseTriggerEvidenceCollection,
	AttestationDevicePhasePollEvidenceCollection,
	AttestationDevicePhaseWaitForTaskCancellation,
	AttestationDevicePhaseNrasVerification,
	AttestationDevicePhaseApplyAppraisalPolicy,
	AttestationDevicePhasePassed,
	AttestationDevicePhaseFailed,
	AttestationDevicePhaseWaived,
}

// FromCoreState projects Core's debug-formatted state onto its variant name,
// such as "PollEvidenceCollection" from `PollEvidenceCollection { task_id: ... }`.
// Anything else becomes Unknown so no task ID or failure text is exposed.
func (p *AttestationDevicePhase) FromCoreState(state string) {
	name := state
	cut := strings.IndexAny(name, "( {")
	if cut >= 0 {
		name = name[:cut]
	}
	for _, known := range knownAttestationDevicePhases {
		if string(known) == name {
			*p = known
			return
		}
	}
	*p = AttestationDevicePhaseUnknown
}

// AttestationCancellationTarget is the latest operator request for a job.
type AttestationCancellationTarget string

// Cancellation targets.
const (
	AttestationCancellationTargetWaive AttestationCancellationTarget = "Waive"
	AttestationCancellationTargetFail  AttestationCancellationTarget = "Fail"
)

// APIAttestationDevice is one device row of a retained job.
type APIAttestationDevice struct {
	// MachineID is the machine the device belongs to
	MachineID string `json:"machineId"`
	// DeviceID is the device's ComponentIntegrity ID
	DeviceID string `json:"deviceId"`
	// Phase is the device's attestation progress or result
	Phase AttestationDevicePhase `json:"phase"`
	// StartedAt is when the device was scheduled
	StartedAt time.Time `json:"startedAt"`
	// CompletedAt is when the device finished; null while unfinished
	CompletedAt *time.Time `json:"completedAt"`
	// CancellationRequestedAt is the latest waive or fail request; null when none
	CancellationRequestedAt *time.Time `json:"cancellationRequestedAt"`
	// CancellationTarget is the latest waive or fail request's target; null when none
	CancellationTarget *AttestationCancellationTarget `json:"cancellationTarget"`
}

// APIAttestationJob is one retained job's devices.
type APIAttestationJob struct {
	// JobID identifies the job
	JobID string `json:"jobId"`
	// Devices are the job's device rows, in no particular order
	Devices []APIAttestationDevice `json:"devices"`
}

// FromProto sets the job from Core. A successful reply for a retained job
// always has at least one device; an empty one cannot be told apart from a
// broken reply, so it fails.
func (m *APIAttestationJob) FromProto(jobID string, p *corev1.SpdmGetAttestationResponse) error {
	if len(p.GetAttestationsDetails()) == 0 {
		return errors.New("attestation job reply has no devices")
	}
	devices := make([]APIAttestationDevice, 0, len(p.GetAttestationsDetails()))
	for _, pd := range p.GetAttestationsDetails() {
		if pd.GetMachineId().GetId() == "" || pd.GetDeviceId() == "" {
			return errors.New("attestation device has no machine or device ID")
		}
		startedAt, ok := requiredTime(pd.GetStartedAt())
		if !ok {
			return errors.New("attestation device has no valid start time")
		}
		device := APIAttestationDevice{
			MachineID: pd.GetMachineId().GetId(),
			DeviceID:  pd.GetDeviceId(),
			StartedAt: startedAt,
		}
		device.Phase.FromCoreState(pd.GetState())
		device.CompletedAt, ok = optionalTime(pd.GetCompletedAt())
		if !ok {
			return errors.New("attestation device has an invalid completion time")
		}
		device.CancellationRequestedAt, ok = optionalTime(pd.GetCancelledAt())
		if !ok {
			return errors.New("attestation device has an invalid cancellation time")
		}
		if pd.CancellationTarget != nil {
			var target AttestationCancellationTarget
			switch pd.GetCancellationTarget() {
			case corev1.SpdmCancellationTarget_SPDM_CANCELLATION_TARGET_WAIVE:
				target = AttestationCancellationTargetWaive
			case corev1.SpdmCancellationTarget_SPDM_CANCELLATION_TARGET_FAIL:
				target = AttestationCancellationTargetFail
			default:
				return fmt.Errorf("attestation device has unknown cancellation target %d", pd.GetCancellationTarget())
			}
			device.CancellationTarget = &target
		}
		devices = append(devices, device)
	}
	*m = APIAttestationJob{JobID: jobID, Devices: devices}
	return nil
}
