// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"strings"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"google.golang.org/protobuf/types/known/timestamppb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// Attestation validation messages shared by several rules.
const (
	validationErrorPrintableASCII = "must be nonempty printable ASCII"
	validationErrorNotEmpty       = "must not be empty"
	validationErrorNoNUL          = "must not contain U+0000"
)

// Rule sets shared by the attestation request DTOs.
var (
	requiredUUIDRules          = []validation.Rule{validation.Required.Error(validationErrorValueRequired), validationis.UUID.Error(validationErrorInvalidUUID)}
	requiredHardwareClassRules = []validation.Rule{validation.Required.Error(validationErrorValueRequired), validation.By(validateHardwareClass)}
	requiredVersionRules       = []validation.Rule{validation.Required.Error(validationErrorValueRequired), validation.By(validateNoNUL)}
)

// oneOf returns an In rule whose message lists the accepted values.
func oneOf[T ~string](values ...T) validation.InRule {
	in := make([]interface{}, len(values))
	names := make([]string, len(values))
	for i, v := range values {
		in[i], names[i] = v, string(v)
	}
	return validation.In(in...).Error("must be one of " + strings.Join(names, ", "))
}

// errAttestationMachineID is the machineId validation failure.
var errAttestationMachineID = errors.New(validationErrorPrintableASCII)

// validateAttestationMachineID accepts a nonempty ID of printable ASCII
// (0x20-0x7E), which also rules out invalid UTF-8. Core owns the full machine
// ID syntax.
func validateAttestationMachineID(value interface{}) error {
	id, _ := value.(string)
	if id == "" {
		return errAttestationMachineID
	}
	for i := range len(id) {
		if id[i] < 0x20 || id[i] > 0x7E {
			return errAttestationMachineID
		}
	}
	return nil
}

// validateNoNUL rejects strings containing U+0000, which Core cannot store in
// JSONB and the audit log cannot distinguish from a literal "\u0000".
func validateNoNUL(value interface{}) error {
	v, _ := validation.Indirect(value)
	s, _ := v.(string)
	if strings.ContainsRune(s, 0) {
		return errors.New(validationErrorNoNUL)
	}
	return nil
}

// uint32PtrRange bounds a present value inclusively. ozzo's Min skips zero as
// empty, so an explicit zero needs this rule to be rejected.
func uint32PtrRange(lo, hi uint32) validation.RuleFunc {
	return func(value interface{}) error {
		v, _ := validation.Indirect(value)
		n, ok := v.(uint32)
		if ok && (n < lo || n > hi) {
			return fmt.Errorf("must be between %d and %d", lo, hi)
		}
		return nil
	}
}

// requiredTime converts a Core timestamp that must be present and valid.
func requiredTime(ts *timestamppb.Timestamp) (time.Time, bool) {
	if ts.CheckValid() != nil { // a nil timestamp is invalid
		return time.Time{}, false
	}
	return ts.AsTime().UTC(), true
}

// optionalTime converts a Core timestamp that may be absent.
func optionalTime(ts *timestamppb.Timestamp) (*time.Time, bool) {
	if ts == nil {
		return nil, true
	}
	t, ok := requiredTime(ts)
	return &t, ok
}

// APIAttestationSiteQuery selects the Site for a site-scoped read.
type APIAttestationSiteQuery struct {
	// SiteID is the Site to read from
	SiteID string `query:"siteId"`
}

// Validate requires a Site UUID.
func (q *APIAttestationSiteQuery) Validate() error {
	return validation.Errors{
		"siteId": validation.Validate(q.SiteID, requiredUUIDRules...),
	}.Filter()
}

// AttestationStatus aggregates the attestation results of a set of devices.
// An unfinished device takes precedence over a failed one, then a waived one;
// Passed means every device passed.
type AttestationStatus string

// Attestation statuses.
const (
	AttestationStatusInProgress AttestationStatus = "InProgress"
	AttestationStatusWaived     AttestationStatus = "Waived"
	AttestationStatusPassed     AttestationStatus = "Passed"
	AttestationStatusFailed     AttestationStatus = "Failed"
)

// attestationStatuses lists every accepted status filter value.
var attestationStatuses = []AttestationStatus{AttestationStatusInProgress, AttestationStatusWaived, AttestationStatusPassed, AttestationStatusFailed}

// FromProto sets the status from Core, failing for unknown numbers. The
// deprecated CANCELLED alias has Waived's number and maps to Waived.
func (s *AttestationStatus) FromProto(p corev1.SpdmAttestationStatus) error {
	switch p {
	case corev1.SpdmAttestationStatus_SPDM_ATT_IN_PROGRESS:
		*s = AttestationStatusInProgress
	case corev1.SpdmAttestationStatus_SPDM_ATT_WAIVED:
		*s = AttestationStatusWaived
	case corev1.SpdmAttestationStatus_SPDM_ATT_PASSED:
		*s = AttestationStatusPassed
	case corev1.SpdmAttestationStatus_SPDM_ATT_FAILED:
		*s = AttestationStatusFailed
	default:
		return fmt.Errorf("unknown attestation status %d", p)
	}
	return nil
}

// ToProto converts a validated status.
func (s AttestationStatus) ToProto() corev1.SpdmAttestationStatus {
	switch s {
	case AttestationStatusWaived:
		return corev1.SpdmAttestationStatus_SPDM_ATT_WAIVED
	case AttestationStatusPassed:
		return corev1.SpdmAttestationStatus_SPDM_ATT_PASSED
	case AttestationStatusFailed:
		return corev1.SpdmAttestationStatus_SPDM_ATT_FAILED
	default:
		return corev1.SpdmAttestationStatus_SPDM_ATT_IN_PROGRESS
	}
}

// AttesterSelectionMode chooses which of a machine's SPDM attesters a profile
// requires.
type AttesterSelectionMode string

// Attester selection modes.
const (
	AttesterSelectionModeNone      AttesterSelectionMode = "None"
	AttesterSelectionModeAll       AttesterSelectionMode = "All"
	AttesterSelectionModeAllowlist AttesterSelectionMode = "Allowlist"
	AttesterSelectionModeDenylist  AttesterSelectionMode = "Denylist"
)

// attesterSelectionModes lists every accepted mode.
var attesterSelectionModes = []AttesterSelectionMode{AttesterSelectionModeNone, AttesterSelectionModeAll, AttesterSelectionModeAllowlist, AttesterSelectionModeDenylist}

// ToProto converts a validated mode.
func (m AttesterSelectionMode) ToProto() corev1.AttesterSelectionMode {
	switch m {
	case AttesterSelectionModeNone:
		return corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_NONE
	case AttesterSelectionModeAll:
		return corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_ALL
	case AttesterSelectionModeAllowlist:
		return corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_ALLOWLIST
	case AttesterSelectionModeDenylist:
		return corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_DENYLIST
	default:
		return corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_UNSPECIFIED
	}
}

// FromProto sets the mode from a Core value, failing for the unset sentinel
// and numbers this REST does not know.
func (m *AttesterSelectionMode) FromProto(p corev1.AttesterSelectionMode) error {
	switch p {
	case corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_NONE:
		*m = AttesterSelectionModeNone
	case corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_ALL:
		*m = AttesterSelectionModeAll
	case corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_ALLOWLIST:
		*m = AttesterSelectionModeAllowlist
	case corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_DENYLIST:
		*m = AttesterSelectionModeDenylist
	default:
		return fmt.Errorf("unknown attester selection mode %d", p)
	}
	return nil
}
