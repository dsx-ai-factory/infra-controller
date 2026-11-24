// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// AttestationCoverageReason says which profile would supply a hardware class's
// policy, or why none would.
type AttestationCoverageReason string

// Attestation coverage reasons.
const (
	AttestationCoverageOwnProfile       AttestationCoverageReason = "OwnProfile"
	AttestationCoverageAnyFallback      AttestationCoverageReason = "AnyFallback"
	AttestationCoverageNoProfile        AttestationCoverageReason = "NoProfile"
	AttestationCoverageClassNotRecorded AttestationCoverageReason = "ClassNotRecorded"
)

// FromProto sets the reason from Core, failing for the unset sentinel and
// unknown numbers.
func (r *AttestationCoverageReason) FromProto(p corev1.AttestationCoverage) error {
	switch p {
	case corev1.AttestationCoverage_ATTESTATION_COVERAGE_OWN_PROFILE:
		*r = AttestationCoverageOwnProfile
	case corev1.AttestationCoverage_ATTESTATION_COVERAGE_ANY_FALLBACK:
		*r = AttestationCoverageAnyFallback
	case corev1.AttestationCoverage_ATTESTATION_COVERAGE_NO_PROFILE:
		*r = AttestationCoverageNoProfile
	case corev1.AttestationCoverage_ATTESTATION_COVERAGE_CLASS_NOT_RECORDED:
		*r = AttestationCoverageClassNotRecorded
	default:
		return fmt.Errorf("unknown attestation coverage %d", p)
	}
	return nil
}

// APIAttesterSet is one distinct set of SPDM attesters recorded for a class.
type APIAttesterSet struct {
	// Digest identifies the set's membership
	Digest string `json:"digest"`
	// ReportingEndpoints counts explored endpoints of the class last reporting this set
	ReportingEndpoints int32 `json:"reportingEndpoints"`
	// Attesters counts the set's members
	Attesters int32 `json:"attesters"`
}

// APIAttestationCoverageEntry reports coverage for one recorded hardware class.
type APIAttestationCoverageEntry struct {
	// HardwareClass is empty for explored endpoints that have no recorded class
	HardwareClass string `json:"hardwareClass"`
	// ExploredEndpoints counts explored endpoints carrying the class
	ExploredEndpoints int32 `json:"exploredEndpoints"`
	// Coverage says which profile would apply, or why none would
	Coverage AttestationCoverageReason `json:"coverage"`
	// Mode is the applying profile's mode; null when no profile would apply
	Mode *AttesterSelectionMode `json:"mode"`
	// AttesterSets are the distinct recorded attester sets, ordered by digest
	AttesterSets []APIAttesterSet `json:"attesterSets"`
}

// APIAttestationCoverage reports, per recorded hardware class, which profile
// would decide attestation.
type APIAttestationCoverage struct {
	// Entries are ordered by class, with endpoints carrying no class last
	Entries []APIAttestationCoverageEntry `json:"entries"`
	// AnyProfileMode is the mode of the "any" profile; null when none is stored
	AnyProfileMode *AttesterSelectionMode `json:"anyProfileMode"`
}

// FromProto sets the report from Core, preserving Core's order and every
// entry. It fails on an unknown enum number or a negative count.
func (m *APIAttestationCoverage) FromProto(p *corev1.GetAttestationCoverageResponse) error {
	if p == nil {
		return errors.New("attestation coverage is missing")
	}
	out := APIAttestationCoverage{Entries: make([]APIAttestationCoverageEntry, 0, len(p.GetEntries()))}
	if p.AnyProfileMode != nil {
		var mode AttesterSelectionMode
		err := mode.FromProto(p.GetAnyProfileMode())
		if err != nil {
			return err
		}
		out.AnyProfileMode = &mode
	}
	for _, pe := range p.GetEntries() {
		entry := APIAttestationCoverageEntry{
			HardwareClass:     pe.GetHardwareClass(),
			ExploredEndpoints: pe.GetExploredEndpoints(),
			AttesterSets:      make([]APIAttesterSet, 0, len(pe.GetAttesterSets())),
		}
		if entry.ExploredEndpoints < 0 {
			return fmt.Errorf("coverage entry %q has a negative endpoint count", entry.HardwareClass)
		}
		err := entry.Coverage.FromProto(pe.GetCoverage())
		if err != nil {
			return err
		}
		if pe.Mode != nil {
			var mode AttesterSelectionMode
			err = mode.FromProto(pe.GetMode())
			if err != nil {
				return err
			}
			entry.Mode = &mode
		}
		for _, ps := range pe.GetAttesterSets() {
			if ps.GetDigest() == "" || ps.GetReportingEndpoints() < 0 || ps.GetAttesters() < 0 {
				return fmt.Errorf("coverage entry %q has an invalid attester set", entry.HardwareClass)
			}
			entry.AttesterSets = append(entry.AttesterSets, APIAttesterSet{
				Digest:             ps.GetDigest(),
				ReportingEndpoints: ps.GetReportingEndpoints(),
				Attesters:          ps.GetAttesters(),
			})
		}
		out.Entries = append(out.Entries, entry)
	}
	*m = out
	return nil
}
