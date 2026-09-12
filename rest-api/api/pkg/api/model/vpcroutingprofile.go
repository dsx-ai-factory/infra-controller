// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"regexp"
	"strconv"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
)

const maxVpcRoutingVni = 16777215

var vpcRoutingVersionRegexp = regexp.MustCompile(`^V([0-9]+)-T([0-9]+)$`)

// APIVpcRoutingProfileUpdateRequest changes the configured profile and active VNI.
type APIVpcRoutingProfileUpdateRequest struct {
	RoutingProfile string `json:"routingProfile"`
	// Vni selects an exact destination; omission delegates selection to Core.
	Vni *int `json:"vni"`
	// VpcID and IfVersionMatch are resolved by the handler, not supplied in JSON.
	VpcID          string `json:"-"`
	IfVersionMatch string `json:"-"`
}

// Validate checks the configured name and optional 24-bit VNI.
func (r *APIVpcRoutingProfileUpdateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.RoutingProfile, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&r.Vni, validation.When(r.Vni != nil,
			validation.Required, validation.Min(1), validation.Max(maxVpcRoutingVni))),
	)
}

// ToProto maps the validated request and the handler's original observation.
func (r *APIVpcRoutingProfileUpdateRequest) ToProto() *corev1.VpcChangeRoutingProfileRequest {
	return &corev1.VpcChangeRoutingProfileRequest{
		Id:                 &corev1.VpcId{Value: r.VpcID},
		IfVersionMatch:     &r.IfVersionMatch,
		RoutingProfileType: NormalizeAPIVpcRoutingProfileForSite(r.RoutingProfile),
		Vni:                cutil.IntPtrToUint32Ptr(r.Vni),
	}
}

// APIVpcInactiveVniReleaseRequest releases the allocation the operator inspected.
type APIVpcInactiveVniReleaseRequest struct {
	IfVersionMatch      string `json:"ifVersionMatch"`
	ExpectedInactiveVni *int   `json:"expectedInactiveVni"`
	VpcID               string `json:"-"`
}

// Validate requires the original observed version and exact inactive VNI.
func (r *APIVpcInactiveVniReleaseRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.IfVersionMatch, validation.Required.Error(validationErrorValueRequired),
			validation.By(r.validateVersion)),
		validation.Field(&r.ExpectedInactiveVni, validation.Required.Error(validationErrorValueRequired),
			validation.Min(1), validation.Max(maxVpcRoutingVni)),
	)
}

func (r *APIVpcInactiveVniReleaseRequest) validateVersion(_ interface{}) error {
	_, err := parseVpcRoutingVersion(r.IfVersionMatch)
	return err
}

// ToProto preserves the caller's version and allocation selection exactly.
func (r *APIVpcInactiveVniReleaseRequest) ToProto() *corev1.VpcReleaseInactiveVniRequest {
	return &corev1.VpcReleaseInactiveVniRequest{
		Id:                  &corev1.VpcId{Value: r.VpcID},
		IfVersionMatch:      &r.IfVersionMatch,
		ExpectedInactiveVni: cutil.IntPtrToUint32Ptr(r.ExpectedInactiveVni),
	}
}

// APIVpcRetainedVniAllocation identifies the VPC's inactive allocation.
type APIVpcRetainedVniAllocation struct {
	PoolName string `json:"poolName"`
	Vni      uint32 `json:"vni"`
}

// APIVpcRoutingState reports persisted Core state, not dataplane convergence.
type APIVpcRoutingState struct {
	VpcID              string                       `json:"vpcId"`
	Version            string                       `json:"version"`
	RoutingProfile     *string                      `json:"routingProfile"`
	ActiveVni          uint32                       `json:"activeVni"`
	RetainedAllocation *APIVpcRetainedVniAllocation `json:"retainedAllocation"`
}

// FromProto maps Core state, preserving its controller ID for validation.
// The handler replaces VpcID with the REST ID only after validation succeeds.
func (s *APIVpcRoutingState) FromProto(raw *corev1.VpcRoutingState) {
	*s = APIVpcRoutingState{}
	if raw == nil {
		return
	}
	s.VpcID = raw.GetId().GetValue()
	s.Version = raw.Version
	s.ActiveVni = raw.ActiveVni
	if raw.RoutingProfileType != nil {
		s.RoutingProfile = cutil.GetPtr(NormalizeAPIVpcRoutingProfileFromSite(*raw.RoutingProfileType))
	}
	if raw.RetainedAllocation != nil {
		s.RetainedAllocation = &APIVpcRetainedVniAllocation{
			PoolName: raw.RetainedAllocation.PoolName,
			Vni:      raw.RetainedAllocation.Vni,
		}
	}
}

// ValidateResponse checks the inspected identity, version, and retained allocation.
// Inspection also supports stored zero or larger-than-24-bit VNIs.
func (s *APIVpcRoutingState) ValidateResponse(expectedID string) error {
	if s == nil || s.VpcID == "" || s.VpcID != expectedID {
		return errors.New("Core returned a missing or different VPC identity")
	}
	_, err := parseVpcRoutingVersion(s.Version)
	if err != nil {
		return fmt.Errorf("invalid routing-state version: %w", err)
	}
	if s.RetainedAllocation != nil {
		retained := s.RetainedAllocation
		if (retained.PoolName != "vpc-vni" && retained.PoolName != "external-vpc-vni") || retained.Vni == s.ActiveVni {
			return errors.New("Core returned an invalid retained allocation")
		}
	}
	return nil
}

// ValidateResponse checks that Core acknowledged this change and retained the old VNI.
func (r *APIVpcRoutingProfileUpdateRequest) ValidateResponse(raw *corev1.VpcRoutingState, observed *APIVpcRoutingState) error {
	state := &APIVpcRoutingState{}
	state.FromProto(raw)
	err := state.ValidateResponse(r.VpcID)
	if err != nil {
		return err
	}
	err = checkVpcRoutingVersionAdvanced(r.IfVersionMatch, state.Version)
	if err != nil {
		return err
	}
	if raw.RoutingProfileType == nil || *raw.RoutingProfileType != NormalizeAPIVpcRoutingProfileForSite(r.RoutingProfile) {
		return errors.New("Core acknowledged a different routing profile")
	}
	retained := state.RetainedAllocation
	if observed == nil || retained == nil || retained.Vni != observed.ActiveVni {
		return errors.New("Core did not retain the previous active VNI")
	}
	if state.ActiveVni == 0 || state.ActiveVni > maxVpcRoutingVni || retained.Vni == 0 || retained.Vni > maxVpcRoutingVni {
		return errors.New("Core returned a VNI outside the routing-profile transition range")
	}
	if r.Vni != nil && int64(state.ActiveVni) != int64(*r.Vni) {
		return errors.New("Core did not select the requested VNI")
	}
	previousRetained := observed.RetainedAllocation
	if previousRetained != nil && (state.ActiveVni != previousRetained.Vni || retained.PoolName == previousRetained.PoolName) {
		return errors.New("Core did not reuse the retained allocation from the other pool")
	}
	return nil
}

// APIVpcInactiveVniReleaseResult reports Core's committed release acknowledgement.
type APIVpcInactiveVniReleaseResult struct {
	VpcID               string  `json:"vpcId"`
	Version             string  `json:"version"`
	RoutingProfile      *string `json:"routingProfile"`
	ActiveVni           uint32  `json:"activeVni"`
	ReleasedInactiveVni uint32  `json:"releasedInactiveVni"`
}

// FromProto maps a validated release acknowledgement, preserving its controller ID.
func (r *APIVpcInactiveVniReleaseResult) FromProto(raw *corev1.VpcReleaseInactiveVniResult) {
	*r = APIVpcInactiveVniReleaseResult{}
	if raw == nil {
		return
	}
	vpc := raw.GetVpc()
	r.VpcID = vpc.GetId().GetValue()
	r.Version = vpc.GetVersion()
	r.ActiveVni = vpc.GetStatus().GetVni()
	r.ReleasedInactiveVni = raw.ReleasedInactiveVni
	if vpc.GetConfig() != nil && vpc.Config.RoutingProfileType != nil {
		r.RoutingProfile = cutil.GetPtr(NormalizeAPIVpcRoutingProfileFromSite(*vpc.Config.RoutingProfileType))
	}
}

// ValidateResponse rejects missing or mismatched release acknowledgements.
func (r *APIVpcInactiveVniReleaseRequest) ValidateResponse(raw *corev1.VpcReleaseInactiveVniResult) error {
	vpc := raw.GetVpc()
	if vpc.GetId().GetValue() == "" || vpc.GetId().GetValue() != r.VpcID {
		return errors.New("Core acknowledged a missing or different VPC")
	}
	if vpc.Config == nil || vpc.Status == nil || vpc.Status.Vni == nil {
		return errors.New("Core omitted the active routing configuration from the release acknowledgement")
	}
	err := checkVpcRoutingVersionAdvanced(r.IfVersionMatch, vpc.Version)
	if err != nil {
		return err
	}
	if r.ExpectedInactiveVni == nil || int64(raw.ReleasedInactiveVni) != int64(*r.ExpectedInactiveVni) || raw.ReleasedInactiveVni == *vpc.Status.Vni {
		return errors.New("Core acknowledged a different released VNI")
	}
	return nil
}

func parseVpcRoutingVersion(version string) (uint64, error) {
	parts := vpcRoutingVersionRegexp.FindStringSubmatch(version)
	if parts == nil {
		return 0, errors.New("version must use V<counter>-T<microseconds> format")
	}
	counter, err := strconv.ParseUint(parts[1], 10, 64)
	if err != nil {
		return 0, fmt.Errorf("invalid version counter: %w", err)
	}
	if counter == 0 {
		return 0, errors.New("version counter must not be zero")
	}
	_, err = strconv.ParseUint(parts[2], 10, 64)
	if err != nil {
		return 0, fmt.Errorf("invalid version timestamp: %w", err)
	}
	return counter, nil
}

func checkVpcRoutingVersionAdvanced(before, after string) error {
	previousCounter, err := parseVpcRoutingVersion(before)
	if err != nil {
		return err
	}
	nextCounter, err := parseVpcRoutingVersion(after)
	if err != nil {
		return err
	}
	// Core wraps the counter and skips zero. Timestamps need not increase.
	expectedCounter := previousCounter + 1
	if expectedCounter == 0 {
		expectedCounter = 1
	}
	if nextCounter != expectedCounter {
		return errors.New("Core did not acknowledge the next configuration version")
	}
	return nil
}
