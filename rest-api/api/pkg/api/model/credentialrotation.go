// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// CredentialRotationType selects the credential family a site-wide rotation
// targets. Values are CapitalCase and map 1:1 to the forge.Forge
// RotationCredentialType enum.
type CredentialRotationType string

const (
	// CredentialRotationTypeBMC rotates the site-wide BMC root credential.
	CredentialRotationTypeBMC CredentialRotationType = "BMC"
	// CredentialRotationTypeHostUEFI rotates the site-default host UEFI credential.
	CredentialRotationTypeHostUEFI CredentialRotationType = "HostUEFI"
	// CredentialRotationTypeDPUUEFI rotates the site-default DPU UEFI credential.
	CredentialRotationTypeDPUUEFI CredentialRotationType = "DPUUEFI"
	// CredentialRotationTypeNVOS rotates the NVOS switch credential.
	CredentialRotationTypeNVOS CredentialRotationType = "NVOS"
	// CredentialRotationTypeLockdownIKM rotates the SuperNIC lockdown IKM credential.
	CredentialRotationTypeLockdownIKM CredentialRotationType = "LockdownIKM"
)

// credentialRotationTypeToProto is the single source of truth for the
// REST-to-proto mapping; the reverse lookup, the ozzo allow-list, and the
// validation error message all derive from it so a new family cannot be added
// to one without the other.
var credentialRotationTypeToProto = map[CredentialRotationType]corev1.RotationCredentialType{
	CredentialRotationTypeBMC:         corev1.RotationCredentialType_ROTATION_BMC,
	CredentialRotationTypeHostUEFI:    corev1.RotationCredentialType_ROTATION_HOST_UEFI,
	CredentialRotationTypeDPUUEFI:     corev1.RotationCredentialType_ROTATION_DPU_UEFI,
	CredentialRotationTypeNVOS:        corev1.RotationCredentialType_ROTATION_NVOS,
	CredentialRotationTypeLockdownIKM: corev1.RotationCredentialType_ROTATION_LOCKDOWN_IKM,
}

// credentialRotationTypeErr is the validation.In error message, derived from the
// mapping keys so it always lists exactly the accepted values.
var credentialRotationTypeErr = fmt.Sprintf("invalid credential type, expected one of: %s", util.SprintMapKeys(credentialRotationTypeToProto))

// credentialRotationTypeValues is the ozzo validation.In allow-list built from
// the mapping keys.
var credentialRotationTypeValues = func() []interface{} {
	values := make([]interface{}, 0, len(credentialRotationTypeToProto))
	for k := range credentialRotationTypeToProto {
		values = append(values, k)
	}
	return values
}()

// ToProto maps the validated type to its proto enum. It trusts that Validate
// (or ValidateCredentialRotationType) has already rejected unknown values, so
// an unmapped type collapses to the proto zero value the server rejects.
func (t CredentialRotationType) ToProto() corev1.RotationCredentialType {
	return credentialRotationTypeToProto[t]
}

// FromProto sets the receiver to the credential rotation type mapped from the
// proto enum. The reverse lookup is derived from credentialRotationTypeToProto
// so it cannot drift from the forward mapping; an unmapped proto value yields
// the empty type, which Validate rejects.
func (t *CredentialRotationType) FromProto(p corev1.RotationCredentialType) {
	*t = util.ReverseMap(credentialRotationTypeToProto)[p]
}

// ValidateCredentialRotationType validates a standalone credential rotation
// type. The GET status endpoint reads the family from a query parameter (no
// request body), so it cannot go through a struct Validate.
func ValidateCredentialRotationType(t CredentialRotationType) error {
	return validation.Validate(t,
		validation.Required.Error(validationErrorValueRequired),
		validation.In(credentialRotationTypeValues...).Error(credentialRotationTypeErr))
}

// APICredentialRotationRequest stages a site-wide credential rotation: it
// publishes a new rotate-to secret and bumps the site-wide target version.
// Devices converge to the new version asynchronously; poll the status endpoint
// to observe convergence.
type APICredentialRotationRequest struct {
	// SiteID is the ID of the Site whose credential family is rotated.
	SiteID string `json:"siteId"`
	// CredentialType selects the credential family to rotate.
	CredentialType CredentialRotationType `json:"credentialType"`
	// Password is the explicit rotate-to password. Optional: when omitted the
	// server auto-generates a strong password. Never echoed in responses.
	Password *string `json:"password,omitempty"`
	// Reason is a free-form operator note recorded with the rotation. It must
	// not contain secrets; it is persisted in the rotation request metadata.
	Reason *string `json:"reason,omitempty"`
}

// Validate checks the request shape before it is converted to a proto.
func (r *APICredentialRotationRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.CredentialType,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(credentialRotationTypeValues...).Error(credentialRotationTypeErr)),
		validation.Field(&r.Password,
			validation.NilOrNotEmpty.Error("password cannot be empty when provided")),
	)
}

// ToProto converts the validated request into a forge.Forge
// RotateCredentialRequest.
func (r *APICredentialRotationRequest) ToProto() *corev1.RotateCredentialRequest {
	return &corev1.RotateCredentialRequest{
		CredentialType: r.CredentialType.ToProto(),
		Password:       r.Password,
		Reason:         r.Reason,
	}
}

// APICredentialRotationResult is returned when a rotation is staged. It carries
// the newly published target version rather than the password.
type APICredentialRotationResult struct {
	// CredentialType is the credential family that was rotated.
	CredentialType CredentialRotationType `json:"credentialType"`
	// TargetVersion is the newly published site-wide version devices converge to.
	TargetVersion uint32 `json:"targetVersion"`
	// Started is when the rotation was staged.
	Started *time.Time `json:"started"`
}

// FromProto populates the result from a forge.Forge RotateCredentialResult.
func (r *APICredentialRotationResult) FromProto(p *corev1.RotateCredentialResult) {
	if p == nil {
		return
	}
	r.CredentialType.FromProto(p.GetCredentialType())
	r.TargetVersion = p.GetTargetVersion()
	if ts := p.GetStartedAt(); ts != nil {
		v := ts.AsTime().UTC()
		r.Started = &v
	}
}

// APICredentialRotationStatus reports convergence of an in-flight (or completed)
// site-wide rotation. When the request targeted a single device by MAC the count
// fields describe just that device and Device carries the per-device detail.
type APICredentialRotationStatus struct {
	// TargetVersion is the current site-wide target version for this family.
	TargetVersion uint32 `json:"targetVersion"`
	// Converged is the number of devices at or beyond the target version.
	Converged uint64 `json:"converged"`
	// Pending is the number of devices not yet converged and eligible to rotate.
	Pending uint64 `json:"pending"`
	// Quarantined is the number of devices currently in a rotation backoff window.
	Quarantined uint64 `json:"quarantined"`
	// QuarantinedDeviceMacs lists the MACs of the quarantined devices.
	QuarantinedDeviceMacs []string `json:"quarantinedDeviceMacs"`
	// Started is when the current target version was staged.
	Started *time.Time `json:"started"`
	// Complete is true only when every device in the queried set has reached the
	// target with none pending and none quarantined.
	Complete bool `json:"complete"`
	// Device carries per-device detail, present only for a MAC-targeted query.
	Device *APIDeviceCredentialRotationStatus `json:"device,omitempty"`
}

// APIDeviceCredentialRotationStatus is the per-device convergence detail
// returned for a MAC-targeted status query.
type APIDeviceCredentialRotationStatus struct {
	// DeviceMac is the device this status describes.
	DeviceMac string `json:"deviceMac"`
	// CurrentVersion is the credential version live on the hardware. Null when
	// not yet established, which counts as pending.
	CurrentVersion *uint32 `json:"currentVersion"`
	// RotatingToVersion is set while a rotation is mid-flight on this device.
	RotatingToVersion *uint32 `json:"rotatingToVersion"`
	// Converged is true once CurrentVersion reaches the site-wide target.
	Converged bool `json:"converged"`
	// Quarantined is true while the device is in a rotation backoff window.
	Quarantined bool `json:"quarantined"`
	// QuarantinedUntil is when the current backoff window expires; set only while
	// quarantined.
	QuarantinedUntil *time.Time `json:"quarantinedUntil"`
	// RotateAttempts is the number of rotation attempts recorded for this device.
	RotateAttempts uint32 `json:"rotateAttempts"`
	// LastAttempted is when the last rotation attempt ran; null if none.
	LastAttempted *time.Time `json:"lastAttempted"`
	// LastError is a redacted last-error string for observability; never a secret.
	LastError *string `json:"lastError"`
}

// FromProto populates the status from a forge.Forge
// CredentialRotationStatusResult.
func (s *APICredentialRotationStatus) FromProto(p *corev1.CredentialRotationStatusResult) {
	if p == nil {
		return
	}
	s.TargetVersion = p.GetTargetVersion()
	s.Converged = p.GetConverged()
	s.Pending = p.GetPending()
	s.Quarantined = p.GetQuarantined()
	s.QuarantinedDeviceMacs = p.GetQuarantinedDeviceMacs()
	if ts := p.GetStartedAt(); ts != nil {
		v := ts.AsTime().UTC()
		s.Started = &v
	}
	s.Complete = p.GetComplete()
	if d := p.GetDevice(); d != nil {
		dev := &APIDeviceCredentialRotationStatus{}
		dev.FromProto(d)
		s.Device = dev
	}
}

// FromProto populates the per-device status from a forge.Forge
// DeviceCredentialRotationStatus.
func (d *APIDeviceCredentialRotationStatus) FromProto(p *corev1.DeviceCredentialRotationStatus) {
	if p == nil {
		return
	}
	d.DeviceMac = p.GetDeviceMac()
	if p.CurrentVersion != nil {
		v := p.GetCurrentVersion()
		d.CurrentVersion = &v
	}
	if p.RotatingToVersion != nil {
		v := p.GetRotatingToVersion()
		d.RotatingToVersion = &v
	}
	d.Converged = p.GetConverged()
	d.Quarantined = p.GetQuarantined()
	if ts := p.GetQuarantinedUntil(); ts != nil {
		v := ts.AsTime().UTC()
		d.QuarantinedUntil = &v
	}
	d.RotateAttempts = p.GetRotateAttempts()
	if ts := p.GetLastAttemptAt(); ts != nil {
		v := ts.AsTime().UTC()
		d.LastAttempted = &v
	}
	if p.LastError != nil {
		v := p.GetLastError()
		d.LastError = &v
	}
}
