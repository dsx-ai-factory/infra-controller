// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"regexp"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// AttestationAnyHardwareClass names the reserved fallback profile that applies
// to every hardware class without a profile of its own.
const AttestationAnyHardwareClass = "any"

// AttestationProfileOrderByHardwareClassAsc is the only supported, and the
// default, order for listing attestation profiles.
const AttestationProfileOrderByHardwareClassAsc = "HARDWARE_CLASS_ASC"

// AttestationProfileOrderByFields lists the orderBy fields accepted by the
// profile list.
var AttestationProfileOrderByFields = []string{"hardware_class"}

// attestationHardwareClassRegex matches <manufacturer>_<model>, mirroring
// Core's validate_hardware_class.
var attestationHardwareClassRegex = regexp.MustCompile(`^[a-z0-9-]+_[a-z0-9-]+$`)

// validateHardwareClass checks a class against Core's naming rule.
func validateHardwareClass(value interface{}) error {
	class, _ := value.(string)
	if class == AttestationAnyHardwareClass || attestationHardwareClassRegex.MatchString(class) {
		return nil
	}
	return fmt.Errorf("must be %q or <manufacturer>_<model>, each part made of lowercase letters, digits, and '-'", AttestationAnyHardwareClass)
}

// APIComponentIDMatch matches ComponentIntegrity IDs case-sensitively, either
// in full or by prefix. Exactly one of the two is set.
type APIComponentIDMatch struct {
	// Exact matches one ID in full
	Exact *string `json:"exact"`
	// Prefix matches every ID starting with the given string
	Prefix *string `json:"prefix"`
}

// Validate requires exactly one nonempty, NUL-free arm.
func (m APIComponentIDMatch) Validate() error {
	if (m.Exact == nil) == (m.Prefix == nil) {
		return errors.New("exactly one of exact or prefix must be set")
	}
	return validation.Errors{
		"exact":  validation.Validate(m.Exact, validation.NilOrNotEmpty.Error(validationErrorNotEmpty), validation.By(validateNoNUL)),
		"prefix": validation.Validate(m.Prefix, validation.NilOrNotEmpty.Error(validationErrorNotEmpty), validation.By(validateNoNUL)),
	}.Filter()
}

// ToProto converts a validated matcher.
func (m APIComponentIDMatch) ToProto() *corev1.ComponentIdMatch {
	if m.Exact != nil {
		return &corev1.ComponentIdMatch{Pattern: &corev1.ComponentIdMatch_Exact{Exact: *m.Exact}}
	}
	return &corev1.ComponentIdMatch{Pattern: &corev1.ComponentIdMatch_Prefix{Prefix: *m.Prefix}}
}

// FromProto sets the matcher from Core, failing when no nonempty arm is set.
func (m *APIComponentIDMatch) FromProto(p *corev1.ComponentIdMatch) error {
	switch pattern := p.GetPattern().(type) {
	case *corev1.ComponentIdMatch_Exact:
		if pattern.Exact == "" {
			return errors.New("component ID matcher has an empty exact pattern")
		}
		*m = APIComponentIDMatch{Exact: &pattern.Exact}
	case *corev1.ComponentIdMatch_Prefix:
		if pattern.Prefix == "" {
			return errors.New("component ID matcher has an empty prefix pattern")
		}
		*m = APIComponentIDMatch{Prefix: &pattern.Prefix}
	default:
		return errors.New("component ID matcher has no pattern")
	}
	return nil
}

// APIAttesterSelection is the policy a profile applies to its hardware class.
type APIAttesterSelection struct {
	// Mode chooses which attesters are required
	Mode AttesterSelectionMode `json:"mode"`
	// ComponentIDs are required for Allowlist and Denylist and must be empty
	// for All and None
	ComponentIDs []APIComponentIDMatch `json:"componentIds"`
}

// Validate checks the mode, every matcher, and that matchers appear exactly
// when the mode needs them.
func (s APIAttesterSelection) Validate() error {
	return validation.Errors{
		"mode":         validation.Validate(s.Mode, validation.Required.Error(validationErrorValueRequired), oneOf(attesterSelectionModes...)),
		"componentIds": validation.Validate(s.ComponentIDs, validation.By(func(interface{}) error { return s.componentIDCountError() })),
	}.Filter()
}

// componentIDCountError enforces Core's cardinality rule for the mode.
func (s APIAttesterSelection) componentIDCountError() error {
	switch s.Mode {
	case AttesterSelectionModeAllowlist, AttesterSelectionModeDenylist:
		if len(s.ComponentIDs) == 0 {
			return fmt.Errorf("at least one matcher is required for mode %s", s.Mode)
		}
	case AttesterSelectionModeNone, AttesterSelectionModeAll:
		if len(s.ComponentIDs) > 0 {
			return fmt.Errorf("must be empty for mode %s", s.Mode)
		}
	}
	return nil
}

// ToProto converts a validated selection.
func (s APIAttesterSelection) ToProto() *corev1.AttesterSelection {
	matchers := make([]*corev1.ComponentIdMatch, 0, len(s.ComponentIDs))
	for _, m := range s.ComponentIDs {
		matchers = append(matchers, m.ToProto())
	}
	return &corev1.AttesterSelection{Mode: s.Mode.ToProto(), ComponentIds: matchers}
}

// FromProto sets the selection from Core, failing on a missing selection, an
// unknown mode, or an invalid matcher.
func (s *APIAttesterSelection) FromProto(p *corev1.AttesterSelection) error {
	if p == nil {
		return errors.New("attester selection is missing")
	}
	var out APIAttesterSelection
	err := out.Mode.FromProto(p.GetMode())
	if err != nil {
		return err
	}
	out.ComponentIDs = make([]APIComponentIDMatch, 0, len(p.GetComponentIds()))
	for _, pm := range p.GetComponentIds() {
		var m APIComponentIDMatch
		err = m.FromProto(pm)
		if err != nil {
			return err
		}
		out.ComponentIDs = append(out.ComponentIDs, m)
	}
	err = out.componentIDCountError()
	if err != nil {
		return fmt.Errorf("componentIds: %w", err)
	}
	*s = out
	return nil
}

// APIAttestationProfileCreateRequest creates the profile for a hardware class.
type APIAttestationProfileCreateRequest struct {
	// SiteID is the Site to create the profile on
	SiteID string `json:"siteId"`
	// HardwareClass is the class the profile applies to
	HardwareClass string `json:"hardwareClass"`
	// Selection is the attester policy
	Selection *APIAttesterSelection `json:"selection"`
}

// Validate checks the request before any lookup.
func (r *APIAttestationProfileCreateRequest) Validate() error {
	return validation.Errors{
		"siteId":        validation.Validate(r.SiteID, requiredUUIDRules...),
		"hardwareClass": validation.Validate(r.HardwareClass, requiredHardwareClassRules...),
		"selection":     validation.Validate(r.Selection, validation.NotNil.Error(validationErrorValueRequired)),
	}.Filter()
}

// ToProto converts a validated request.
func (r *APIAttestationProfileCreateRequest) ToProto() *corev1.CreateAttestationProfileRequest {
	return &corev1.CreateAttestationProfileRequest{HardwareClass: r.HardwareClass, Selection: r.Selection.ToProto()}
}

// APIAttestationProfileUpdateRequest replaces a profile's selection when its
// version still matches.
type APIAttestationProfileUpdateRequest struct {
	// SiteID is the Site holding the profile
	SiteID string `json:"siteId"`
	// HardwareClass comes from the request path
	HardwareClass string `json:"-"`
	// Selection replaces the stored selection
	Selection *APIAttesterSelection `json:"selection"`
	// IfVersionMatch is the version the caller read, forwarded verbatim
	IfVersionMatch *string `json:"ifVersionMatch"`
	// AcknowledgeFallbackImpact must be true to set the "any" profile to None
	AcknowledgeFallbackImpact bool `json:"acknowledgeFallbackImpact"`
}

// Validate checks the request before any lookup.
func (r *APIAttestationProfileUpdateRequest) Validate() error {
	return validation.Errors{
		"siteId":                    validation.Validate(r.SiteID, requiredUUIDRules...),
		"hardwareClass":             validation.Validate(r.HardwareClass, requiredHardwareClassRules...),
		"selection":                 validation.Validate(r.Selection, validation.NotNil.Error(validationErrorValueRequired)),
		"ifVersionMatch":            validation.Validate(r.IfVersionMatch, requiredVersionRules...),
		"acknowledgeFallbackImpact": r.fallbackAcknowledgementError(),
	}.Filter()
}

// fallbackAcknowledgementError guards disabling attestation for every class
// that has no profile of its own.
func (r *APIAttestationProfileUpdateRequest) fallbackAcknowledgementError() error {
	if r.HardwareClass == AttestationAnyHardwareClass && r.Selection != nil && r.Selection.Mode == AttesterSelectionModeNone && !r.AcknowledgeFallbackImpact {
		return errors.New("must be true to set the any profile to None, which disables attestation for every class without its own profile")
	}
	return nil
}

// ToProto converts a validated request.
func (r *APIAttestationProfileUpdateRequest) ToProto() *corev1.UpdateAttestationProfileRequest {
	return &corev1.UpdateAttestationProfileRequest{
		HardwareClass:  r.HardwareClass,
		Selection:      r.Selection.ToProto(),
		IfVersionMatch: r.IfVersionMatch,
	}
}

// APIAttestationProfileDeleteRequest deletes a profile when its version still
// matches.
type APIAttestationProfileDeleteRequest struct {
	// SiteID is the Site holding the profile
	SiteID string `query:"siteId" json:"-"`
	// HardwareClass comes from the request path
	HardwareClass string `json:"-"`
	// IfVersionMatch is the version the caller read, forwarded verbatim
	IfVersionMatch *string `query:"ifVersionMatch" json:"-"`
	// AcknowledgeFallbackImpact must be true to delete the "any" profile
	AcknowledgeFallbackImpact bool `query:"acknowledgeFallbackImpact" json:"-"`
}

// Validate checks the request before any lookup.
func (r *APIAttestationProfileDeleteRequest) Validate() error {
	return validation.Errors{
		"siteId":                    validation.Validate(r.SiteID, requiredUUIDRules...),
		"hardwareClass":             validation.Validate(r.HardwareClass, requiredHardwareClassRules...),
		"ifVersionMatch":            validation.Validate(r.IfVersionMatch, requiredVersionRules...),
		"acknowledgeFallbackImpact": r.fallbackAcknowledgementError(),
	}.Filter()
}

// fallbackAcknowledgementError guards removing the site-wide fallback.
func (r *APIAttestationProfileDeleteRequest) fallbackAcknowledgementError() error {
	if r.HardwareClass == AttestationAnyHardwareClass && !r.AcknowledgeFallbackImpact {
		return errors.New("must be true to delete the any profile, which removes the fallback for every class without its own profile")
	}
	return nil
}

// ToProto converts a validated request.
func (r *APIAttestationProfileDeleteRequest) ToProto() *corev1.DeleteAttestationProfileRequest {
	return &corev1.DeleteAttestationProfileRequest{HardwareClass: r.HardwareClass, IfVersionMatch: r.IfVersionMatch}
}

// APIAttestationProfileListRequest lists a Site's profiles one numbered page
// at a time.
type APIAttestationProfileListRequest struct {
	// Site selects the Site to read from
	Site APIAttestationSiteQuery
	// Page selects the page; orderBy accepts only HARDWARE_CLASS_ASC
	Page pagination.PageRequest
}

// Validate checks the Site selection.
func (r *APIAttestationProfileListRequest) Validate() error {
	return r.Site.Validate()
}

// ValidatePage defaults orderBy to HARDWARE_CLASS_ASC, the only supported
// order, and validates paging.
func (r *APIAttestationProfileListRequest) ValidatePage() error {
	if r.Page.OrderByStr == nil {
		orderBy := AttestationProfileOrderByHardwareClassAsc
		r.Page.OrderByStr = &orderBy
	}
	err := r.Page.Validate(AttestationProfileOrderByFields)
	if err != nil {
		return err
	}
	if *r.Page.OrderByStr != AttestationProfileOrderByHardwareClassAsc {
		return validation.Errors{"orderBy": fmt.Errorf("only %s is supported", AttestationProfileOrderByHardwareClassAsc)}
	}
	return nil
}

// APIAttestationProfileGetRequest reads one profile.
type APIAttestationProfileGetRequest struct {
	// SiteID is the Site to read from
	SiteID string `query:"siteId"`
	// HardwareClass comes from the request path
	HardwareClass string `json:"-"`
}

// Validate checks the request before any lookup.
func (r *APIAttestationProfileGetRequest) Validate() error {
	return validation.Errors{
		"siteId":        validation.Validate(r.SiteID, requiredUUIDRules...),
		"hardwareClass": validation.Validate(r.HardwareClass, requiredHardwareClassRules...),
	}.Filter()
}

// ToProto converts a validated request.
func (r *APIAttestationProfileGetRequest) ToProto() *corev1.GetAttestationProfileRequest {
	return &corev1.GetAttestationProfileRequest{HardwareClass: r.HardwareClass}
}

// APIAttestationProfile is the policy stored for one hardware class.
type APIAttestationProfile struct {
	// HardwareClass is the class the profile applies to
	HardwareClass string `json:"hardwareClass"`
	// Version identifies this revision; pass it as ifVersionMatch to change it
	Version string `json:"version"`
	// Selection is the attester policy
	Selection APIAttesterSelection `json:"selection"`
	// UpdatedAt is when this revision was written
	UpdatedAt time.Time `json:"updatedAt"`
	// UpdatedBy is the principal Core recorded for this revision
	UpdatedBy string `json:"updatedBy"`
}

// FromProto sets the profile from Core. It fails unless the reply carries a
// class, a version, a valid selection and update time, so an empty reply
// cannot confirm a write.
func (m *APIAttestationProfile) FromProto(p *corev1.AttestationProfile) error {
	if p == nil {
		return errors.New("attestation profile is missing")
	}
	if p.GetHardwareClass() == "" {
		return errors.New("attestation profile has no hardware class")
	}
	if p.GetVersion() == "" {
		return errors.New("attestation profile has no version")
	}
	updatedAt, ok := requiredTime(p.GetUpdatedAt())
	if !ok {
		return errors.New("attestation profile has no valid update time")
	}
	var selection APIAttesterSelection
	err := selection.FromProto(p.GetSelection())
	if err != nil {
		return err
	}
	*m = APIAttestationProfile{
		HardwareClass: p.GetHardwareClass(),
		Version:       p.GetVersion(),
		Selection:     selection,
		UpdatedAt:     updatedAt,
		UpdatedBy:     p.GetUpdatedBy(),
	}
	return nil
}

// FromProtoForClass is FromProto that also requires the reply to describe
// the requested class.
func (m *APIAttestationProfile) FromProtoForClass(hardwareClass string, p *corev1.AttestationProfile) error {
	var out APIAttestationProfile
	err := out.FromProto(p)
	if err != nil {
		return err
	}
	if out.HardwareClass != hardwareClass {
		return fmt.Errorf("attestation profile reply is for class %q, not %q", out.HardwareClass, hardwareClass)
	}
	*m = out
	return nil
}
