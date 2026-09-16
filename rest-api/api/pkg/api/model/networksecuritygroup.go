// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"net"
	"strings"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"github.com/google/uuid"

	hutil "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const MaxNetworkSecurityGroupRules = 200
const NetworkSecurityGroupRulePriorityMin = 0
const NetworkSecurityGroupRulePriorityMax = 60000

// Action conversion maps

const APINetworkSecurityGroupRuleActionPermit = "PERMIT"
const APINetworkSecurityGroupRuleActionDeny = "DENY"

var NetworkSecurityGroupRuleProtobufActionFromAPIAction = map[string]corev1.NetworkSecurityGroupRuleAction{
	APINetworkSecurityGroupRuleActionPermit: corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_PERMIT,
	APINetworkSecurityGroupRuleActionDeny:   corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_DENY,
}

var NetworkSecurityGroupRuleAPIActionFromProtobufAction = map[corev1.NetworkSecurityGroupRuleAction]string{
	corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_PERMIT: APINetworkSecurityGroupRuleActionPermit,
	corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_DENY:   APINetworkSecurityGroupRuleActionDeny,
}

// Direction conversion maps

const APINetworkSecurityGroupRuleDirectionIngress = "INGRESS"
const APINetworkSecurityGroupRuleActionEgress = "EGRESS"

var NetworkSecurityGroupRuleProtobufDirectionFromAPIDirection = map[string]corev1.NetworkSecurityGroupRuleDirection{
	APINetworkSecurityGroupRuleDirectionIngress: corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_INGRESS,
	APINetworkSecurityGroupRuleActionEgress:     corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_EGRESS,
}

var NetworkSecurityGroupRuleAPIDirectionFromProtobufDirection = map[corev1.NetworkSecurityGroupRuleDirection]string{
	corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_INGRESS: APINetworkSecurityGroupRuleDirectionIngress,
	corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_EGRESS:  APINetworkSecurityGroupRuleActionEgress,
}

// Protocol conversion maps

const APINetworkSecurityGroupRuleProtocolAny = "ANY"
const APINetworkSecurityGroupRuleProtocolIcmp = "ICMP"
const APINetworkSecurityGroupRuleProtocolIcmp6 = "ICMP6"
const APINetworkSecurityGroupRuleProtocolTcp = "TCP"
const APINetworkSecurityGroupRuleProtocolUdp = "UDP"

var NetworkSecurityGroupRuleProtobufProtocolFromAPIProtocol = map[string]corev1.NetworkSecurityGroupRuleProtocol{
	APINetworkSecurityGroupRuleProtocolAny:   corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_ANY,
	APINetworkSecurityGroupRuleProtocolIcmp:  corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_ICMP,
	APINetworkSecurityGroupRuleProtocolIcmp6: corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_ICMP6,
	APINetworkSecurityGroupRuleProtocolTcp:   corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_TCP,
	APINetworkSecurityGroupRuleProtocolUdp:   corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_UDP,
}

var NetworkSecurityGroupRuleAPIProtocolFromProtobufProtocol = map[corev1.NetworkSecurityGroupRuleProtocol]string{
	corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_ANY:   APINetworkSecurityGroupRuleProtocolAny,
	corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_ICMP:  APINetworkSecurityGroupRuleProtocolIcmp,
	corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_ICMP6: APINetworkSecurityGroupRuleProtocolIcmp6,
	corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_TCP:   APINetworkSecurityGroupRuleProtocolTcp,
	corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_UDP:   APINetworkSecurityGroupRuleProtocolUdp,
}

// Propagation status maps

const APINetworkSecurityGroupPropagationDetailedStatusNone = "None"
const APINetworkSecurityGroupPropagationDetailedStatusPartial = "Partial"
const APINetworkSecurityGroupPropagationDetailedStatusFull = "Full"
const APINetworkSecurityGroupPropagationDetailedStatusUnknown = "Unknown"
const APINetworkSecurityGroupPropagationDetailedStatusError = "Error"

var NetworkSecurityGroupRuleAPIPropagationDetailedStatusFromProtobufPropagationStatus = map[corev1.NetworkSecurityGroupPropagationStatus]string{
	corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_NONE:    APINetworkSecurityGroupPropagationDetailedStatusNone,
	corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_PARTIAL: APINetworkSecurityGroupPropagationDetailedStatusPartial,
	corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL:    APINetworkSecurityGroupPropagationDetailedStatusFull,
	corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_UNKNOWN: APINetworkSecurityGroupPropagationDetailedStatusUnknown,
	corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_ERROR:   APINetworkSecurityGroupPropagationDetailedStatusError,
}

const APINetworkSecurityGroupPropagationStatusError = "Error"
const APINetworkSecurityGroupPropagationStatusSynchronizing = "Synchronizing"
const APINetworkSecurityGroupPropagationStatusSynchronized = "Synchronized"

var NetworkSecurityGroupRuleAPIPropagationStatusFromProtobufPropagationStatus = map[corev1.NetworkSecurityGroupPropagationStatus]string{
	corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_NONE:    APINetworkSecurityGroupPropagationStatusSynchronizing,
	corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_PARTIAL: APINetworkSecurityGroupPropagationStatusSynchronizing,
	corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL:    APINetworkSecurityGroupPropagationStatusSynchronized,
	corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_UNKNOWN: APINetworkSecurityGroupPropagationStatusError,
	corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_ERROR:   APINetworkSecurityGroupPropagationStatusError,
}

var (
	// Time when the NetworkSecurityGroup propagation object_id attribute will be deprecated
	networkSecurityGroupPropagationObjectIDDeprecationTime = time.Date(2026, time.September, 10, 0, 0, 0, 0, time.UTC)

	// Deprecations for the NetworkSecurityGroup model
	networkSecurityGroupPropagationDetailsDeprecations = []DeprecatedEntity{
		{
			OldValue:     "object_id",
			NewValue:     cutil.GetPtr("objectId"),
			Type:         DeprecationTypeAttribute,
			TakeActionBy: networkSecurityGroupPropagationObjectIDDeprecationTime,
		},
	}
)

// APINetworkSecurityGroupCreateRequest is the data structure to capture instance request to create a new NetworkSecurityGroup
type APINetworkSecurityGroupCreateRequest struct {
	// ID is the optional user-specified UUID of the NetworkSecurityGroup
	ID *uuid.UUID `json:"id"`
	// Name is the name of the NetworkSecurityGroup
	Name string `json:"name"`
	// Description is the description of the NetworkSecurityGroup
	Description *string `json:"description"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Rules is the list of NetworkSecurityGroupRuleAttributes for the NetworkSecurityGroup
	Rules []APINetworkSecurityGroupRule `json:"rules"`
	// StatefulEgress defines whether a NetworkSecurityGroup's egress rules will be automatically stateful
	StatefulEgress bool `json:"statefulEgress"`
	// Labels to be associated with the NetworkSecurityGroup
	Labels map[string]string `json:"labels"`
}

// Validate ensures the values in the request are acceptable.
// Per the proto-conversion convention, this is the universal
// pre-`ToProto` step: each rule is validated here so the request-shape
// `ToProto` can be a focused mapper that trusts its input.
func (req *APINetworkSecurityGroupCreateRequest) Validate(siteConfig *cdbm.SiteConfig) error {
	err := validation.ValidateStruct(req,
		validation.Field(&req.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&req.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&req.Description,
			validation.When(req.Description != nil, validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
	)
	if err != nil {
		return err
	}

	maxRules := MaxNetworkSecurityGroupRules
	if siteConfig != nil && siteConfig.MaxNetworkSecurityGroupRuleCount != nil {
		maxRules = *siteConfig.MaxNetworkSecurityGroupRuleCount
	}

	if len(req.Rules) > maxRules {
		return validation.Errors{
			"rules": fmt.Errorf("number of rules cannot exceed %d", maxRules),
		}
	}

	// Validate (and normalize) each rule up-front so the request-shape
	// `ToProto` can rely on the request being safe to translate.
	for i := range req.Rules {
		if err := req.Rules[i].Validate(); err != nil {
			return err
		}
	}

	if err := util.ValidateLabels(req.Labels); err != nil {
		return err
	}

	return nil
}

// APINetworkSecurityGroupUpdateRequest is the data structure to capture user request to update a NetworkSecurityGroup
type APINetworkSecurityGroupUpdateRequest struct {
	// Name is the name of the NetworkSecurityGroup
	Name *string `json:"name"`
	// Description is the description of the NetworkSecurityGroup
	Description *string `json:"description"`
	// StatefulEgress defines whether a NetworkSecurityGroup's egress rules will be automatically stateful
	StatefulEgress *bool `json:"statefulEgress"`
	// Rules is the list of NetworkSecurityGroupRuleAttributes for the NetworkSecurityGroup
	Rules []APINetworkSecurityGroupRule `json:"rules"`
	// Labels to be associted with the NetworkSecurityGroup
	Labels map[string]string `json:"labels"`
}

// Validate ensures the values in the request are acceptable.
// Per the proto-conversion convention, this is the universal
// pre-`ToProto` step: each rule is validated here so the request-shape
// `ToProto` can be a focused mapper that trusts its input.
func (req *APINetworkSecurityGroupUpdateRequest) Validate(siteConfig *cdbm.SiteConfig) error {
	err := validation.ValidateStruct(req,
		validation.Field(&req.Name,
			validation.When(req.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(req.Name != nil, validation.By(util.ValidateNameCharacters)),
			validation.When(req.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
		validation.Field(&req.Description,
			validation.When(req.Description != nil, validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
	)

	if err != nil {
		return err
	}

	maxRules := MaxNetworkSecurityGroupRules
	if siteConfig != nil && siteConfig.MaxNetworkSecurityGroupRuleCount != nil {
		maxRules = *siteConfig.MaxNetworkSecurityGroupRuleCount
	}

	if len(req.Rules) > maxRules {
		return validation.Errors{
			"rules": fmt.Errorf("number of rules cannot exceed %d", maxRules),
		}
	}

	// Validate (and normalize) each rule up-front so the request-shape
	// `ToProto` can rely on the request being safe to translate.
	for i := range req.Rules {
		if err := req.Rules[i].Validate(); err != nil {
			return err
		}
	}

	if err := util.ValidateLabels(req.Labels); err != nil {
		return err
	}

	return nil
}

// APINetworkSecurityGroup is the data structure to capture API representation of a NetworkSecurityGroup
type APINetworkSecurityGroup struct {
	// ID is the unique UUID v4 identifier for the NetworkSecurityGroup
	ID string `json:"id"`
	// Name is the name of the NetworkSecurityGroup
	Name string `json:"name"`
	// Description is the description of the NetworkSecurityGroup
	Description *string `json:"description"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// Tenant is the summary of the tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// Status is the status of the NetworkSecurityGroup
	Status string `json:"status"`
	// StatusHistory is the status detail records for the site over time
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// StatefulEgress defines whether a NetworkSecurityGroup's egress rules will be automatically stateful
	StatefulEgress bool `json:"statefulEgress"`
	// Rules is the list of NetworkSecurityGroupRuleAttributes for the NetworkSecurityGroup
	Rules []*APINetworkSecurityGroupRule `json:"rules"`
	// Labels is the set of labels/tags for the NetworkSecurityGroup
	Labels APILabels `json:"labels"`
	// Created indicates the ISO datetime string for when the NetworkSecurityGroup was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the NetworkSecurityGroup was last updated
	Updated time.Time `json:"updated"`
	// AttachmentStats holds the counts for objects that have
	// Attached the NSG if requested.
	AttachmentStats *APINetworkSecurityGroupStats `json:"attachmentStats"`
	// RuleCount hold the count of the number of rules in the NetworkSecurityGroup
	RuleCount int `json:"ruleCount"`
}

// ruleErr wraps a formatted message in the `validation.Errors{"rules": ...}`
// envelope that every rule-level failure uses. Keeping this here lets the
// individual validate* helpers (and the Validate body itself) stay
// one-liners while still producing the exact error shape callers expect.
func ruleErr(format string, args ...any) error {
	return validation.Errors{"rules": fmt.Errorf(format, args...)}
}

// validateRuleEnumValue uppercases *value in place and looks the result
// up in the provided enum map, returning a `ruleErr` with the field
// name interpolated if the value is not recognised. Used for the three
// near-identical Direction / Action / Protocol checks. `T any` is fine
// because we only care that the map key (the API string) is present.
func validateRuleEnumValue[T any](value *string, lookup map[string]T, fieldName string) error {
	*value = strings.ToUpper(*value)
	if _, found := lookup[*value]; !found {
		return ruleErr("unknown %s `%s`", fieldName, *value)
	}
	return nil
}

// validateRulePortRange runs the shared port-range parser against one
// side of a rule and wraps any parser error in the matching
// `unable to parse <label> port range` message. `label` is "source" or
// "destination". The parsed values themselves are reproduced in
// `ToProto`; this helper only gates well-formedness.
func validateRulePortRange(value *string, label string) error {
	if _, _, err := hutil.StringPtrToPortRangeUint32PtrPair(value); err != nil {
		return ruleErr("unable to parse %s port range in API request: %w", label, err)
	}
	return nil
}

// validateRulePrefix enforces that the source / destination prefix is
// both present (the only network option modelled today, so it's
// required) and a parseable CIDR. `label` is "source" or "destination"
// and is interpolated into both the missing-option and invalid-CIDR
// messages so the caller's error funnel keeps a consistent shape.
func validateRulePrefix(value *string, label string) error {
	if value == nil {
		return ruleErr("required %s network option not found in API request", label)
	}
	if _, _, err := net.ParseCIDR(*value); err != nil {
		return ruleErr("%s prefix `%s` is not valid", label, *value)
	}
	return nil
}

// validateRuleProtocolPortCompat rejects rules that pair a port-less
// protocol (Any / Icmp / Icmp6) with a source or destination port
// range. Pulled out of `Validate` so the high-level outline stays
// flat; the switch is small but the inline form drowned in the
// surrounding sequential checks.
func validateRuleProtocolPortCompat(rule *APINetworkSecurityGroupRule) error {
	switch rule.Protocol {
	case APINetworkSecurityGroupRuleProtocolAny, APINetworkSecurityGroupRuleProtocolIcmp, APINetworkSecurityGroupRuleProtocolIcmp6:
		if rule.SourcePortRange != nil || rule.DestinationPortRange != nil {
			return ruleErr("ports cannot be specified with protocol `%s`", rule.Protocol)
		}
	}
	return nil
}

// Validate checks the request-side fields of this rule and normalizes
// case for the enum-style fields (Direction / Action / Protocol).
// Per the proto-conversion convention this is the pre-`ToProto`
// validation step: it covers priority bounds, recognised enum values,
// protocol/port compatibility, port-range parseability, prefix CIDR
// shape, and the presence of source / destination network options.
// Once Validate has succeeded `ToProto` can act as a focused mapper.
func (rule *APINetworkSecurityGroupRule) Validate() error {
	if rule.Priority < NetworkSecurityGroupRulePriorityMin || rule.Priority > NetworkSecurityGroupRulePriorityMax {
		return ruleErr("priority `%d` must be between 0 and 60000", rule.Priority)
	}
	if err := validateRuleEnumValue(&rule.Direction, NetworkSecurityGroupRuleProtobufDirectionFromAPIDirection, "direction"); err != nil {
		return err
	}
	if err := validateRuleEnumValue(&rule.Action, NetworkSecurityGroupRuleProtobufActionFromAPIAction, "action"); err != nil {
		return err
	}
	if err := validateRuleEnumValue(&rule.Protocol, NetworkSecurityGroupRuleProtobufProtocolFromAPIProtocol, "protocol"); err != nil {
		return err
	}
	if err := validateRuleProtocolPortCompat(rule); err != nil {
		return err
	}
	if err := validateRulePortRange(rule.SourcePortRange, "source"); err != nil {
		return err
	}
	if err := validateRulePortRange(rule.DestinationPortRange, "destination"); err != nil {
		return err
	}
	if err := validateRulePrefix(rule.SourcePrefix, "source"); err != nil {
		return err
	}
	if err := validateRulePrefix(rule.DestinationPrefix, "destination"); err != nil {
		return err
	}
	return nil
}

// ToProto converts an API rule into the workflow proto attributes that
// the DB record wraps and that get sent to NICo. It trusts that
// `Validate` has run first — enum lookups, port ranges, and prefixes
// are assumed safe — so this method is a focused mapper and does not
// return errors.
func (rule *APINetworkSecurityGroupRule) ToProto() *corev1.NetworkSecurityGroupRuleAttributes {
	// Validate has normalized casing and proven these lookups succeed,
	// so we don't re-check the `found` results here.
	direction := NetworkSecurityGroupRuleProtobufDirectionFromAPIDirection[rule.Direction]
	action := NetworkSecurityGroupRuleProtobufActionFromAPIAction[rule.Action]
	protocol := NetworkSecurityGroupRuleProtobufProtocolFromAPIProtocol[rule.Protocol]

	// Validate already parsed both ranges successfully; the parse
	// helpers are total over well-formed input and a nil pointer
	// produces nil starts/ends.
	srcPortStart, srcPortEnd, _ := hutil.StringPtrToPortRangeUint32PtrPair(rule.SourcePortRange)
	dstPortStart, dstPortEnd, _ := hutil.StringPtrToPortRangeUint32PtrPair(rule.DestinationPortRange)

	attrs := &corev1.NetworkSecurityGroupRuleAttributes{
		Id:        rule.Name,
		Direction: direction,
		Protocol:  protocol,
		Action:    action,
		Priority:  uint32(rule.Priority),
		Ipv6:      false, // We have support for it in ACLs but pretty much nowhere else, so hide this for now.

		SrcPortStart: srcPortStart,
		SrcPortEnd:   srcPortEnd,
		DstPortStart: dstPortStart,
		DstPortEnd:   dstPortEnd,
	}

	if rule.SourcePrefix != nil {
		attrs.SourceNet = &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: *rule.SourcePrefix}
	}
	if rule.DestinationPrefix != nil {
		attrs.DestinationNet = &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: *rule.DestinationPrefix}
	}

	return attrs
}

// FromProto populates this rule from workflow proto attributes
// (typically the embedded value on a stored
// `cdbm.NetworkSecurityGroupRule`). A nil attrs is a no-op.
//
// Per the proto-conversion convention this method does not return
// errors. Unknown enum values leave the corresponding string field
// empty; half-defined port ranges and unrecognized network options
// leave the matching fields nil. Anything more aggressive belongs in
// a DB-integrity check before the data reaches this method.
func (rule *APINetworkSecurityGroupRule) FromProto(attrs *corev1.NetworkSecurityGroupRuleAttributes) {
	if attrs == nil {
		return
	}

	rule.Name = attrs.Id
	rule.Priority = int(attrs.Priority)

	// Unknown enum values fall through to the zero string. Callers
	// that need stricter handling should reject the entity at the DB
	// layer rather than the conversion layer.
	rule.Direction = NetworkSecurityGroupRuleAPIDirectionFromProtobufDirection[attrs.Direction]
	rule.Action = NetworkSecurityGroupRuleAPIActionFromProtobufAction[attrs.Action]
	rule.Protocol = NetworkSecurityGroupRuleAPIProtocolFromProtobufProtocol[attrs.Protocol]

	// Source / destination prefixes — currently the only network
	// options modelled on the API side. Anything else leaves the
	// matching field nil.
	rule.SourcePrefix = nil
	if src, ok := attrs.GetSourceNet().(*corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix); ok {
		prefix := src.SrcPrefix
		rule.SourcePrefix = &prefix
	}
	rule.DestinationPrefix = nil
	if dst, ok := attrs.GetDestinationNet().(*corev1.NetworkSecurityGroupRuleAttributes_DstPrefix); ok {
		prefix := dst.DstPrefix
		rule.DestinationPrefix = &prefix
	}

	// Port ranges round-trip only when both halves are present. A
	// half-defined range comes back as a nil API range; the rest of
	// the rule still loads.
	rule.SourcePortRange = nil
	if attrs.SrcPortStart != nil && attrs.SrcPortEnd != nil {
		s := fmt.Sprintf("%d-%d", *attrs.SrcPortStart, *attrs.SrcPortEnd)
		rule.SourcePortRange = &s
	}
	rule.DestinationPortRange = nil
	if attrs.DstPortStart != nil && attrs.DstPortEnd != nil {
		s := fmt.Sprintf("%d-%d", *attrs.DstPortStart, *attrs.DstPortEnd)
		rule.DestinationPortRange = &s
	}
}

// NewAPINetworkSecurityGroupRule constructs an APINetworkSecurityGroupRule
// from workflow proto attributes by calling FromProto. Returns nil for a
// nil attrs argument.
func NewAPINetworkSecurityGroupRule(attrs *corev1.NetworkSecurityGroupRuleAttributes) *APINetworkSecurityGroupRule {
	if attrs == nil {
		return nil
	}
	rule := &APINetworkSecurityGroupRule{}
	rule.FromProto(attrs)
	return rule
}

// NewAPINetworkSecurityGroup accepts a DB layer NetworkSecurityGroup object and returns an API object.
// Rule reconstruction is defensive (see (*APINetworkSecurityGroupRule).FromProto),
// so this constructor does not return an error.
func NewAPINetworkSecurityGroup(dsg *cdbm.NetworkSecurityGroup, dbsds []cdbm.StatusDetail) *APINetworkSecurityGroup {
	apisg := &APINetworkSecurityGroup{
		ID:             dsg.ID,
		Name:           dsg.Name,
		Description:    dsg.Description,
		SiteID:         dsg.SiteID.String(),
		TenantID:       dsg.TenantID.String(),
		Labels:         APILabels(dsg.Labels),
		Status:         dsg.Status,
		Created:        dsg.Created,
		Updated:        dsg.Updated,
		StatefulEgress: dsg.StatefulEgress,
	}

	if dsg.Site != nil {
		apisg.Site = NewAPISiteSummary(dsg.Site)
	}

	if dsg.Tenant != nil {
		apisg.Tenant = NewAPITenantSummary(dsg.Tenant)
	}

	apisg.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apisg.StatusHistory = append(apisg.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	rules := make([]*APINetworkSecurityGroupRule, len(dsg.Rules))

	for i, rule := range dsg.Rules {
		rules[i] = NewAPINetworkSecurityGroupRule(rule.NetworkSecurityGroupRuleAttributes)
	}

	apisg.Rules = rules
	apisg.RuleCount = len(rules)

	return apisg
}

// ToProto builds the workflow request that asks a Site to create a new
// NetworkSecurityGroup for this API request. `nsg` is the just-persisted
// DB record; its `ToProto()` is the source of the canonical Metadata
// (Name, Description, Labels) and the assigned ID. The request-shape
// fields (rule list, statefulEgress) are taken directly from the
// request because they're the caller's authoritative input for this
// create.
//
// The method trusts that the request has already been Validated and
// that the handler has performed any cross-context checks Validate
// cannot see; in particular, every rule has been Validated, so each
// `rule.ToProto()` is safe to call without checking errors.
func (req *APINetworkSecurityGroupCreateRequest) ToProto(nsg *cdbm.NetworkSecurityGroup) *corev1.CreateNetworkSecurityGroupRequest {
	nsgProto := nsg.ToProto()
	// The DB record has already been built from the request, so the
	// Metadata it produces (Name / Description / Labels) is the
	// canonical wire form. Re-use it here rather than rebuilding from
	// the request struct.
	rules := make([]*corev1.NetworkSecurityGroupRuleAttributes, len(req.Rules))
	for i := range req.Rules {
		rules[i] = req.Rules[i].ToProto()
	}
	return &corev1.CreateNetworkSecurityGroupRequest{
		Id:                   &nsg.ID,
		TenantOrganizationId: nsgProto.TenantOrganizationId,
		Metadata:             nsgProto.Metadata,
		NetworkSecurityGroupAttributes: &corev1.NetworkSecurityGroupAttributes{
			StatefulEgress: req.StatefulEgress,
			Rules:          rules,
		},
	}
}

// ToProto builds the workflow request that pushes this update's
// post-merge state to the Site. `nsg` is the already-updated DB
// record; its `ToProto()` provides the canonical Metadata and the
// post-merge rule list (the handler writes the request rules into the
// DB before calling this), keeping the wire payload aligned with what
// the database now holds.
//
// As with the create variant, this method trusts that `Validate` has
// run and any cross-context checks have been performed in the handler.
func (req *APINetworkSecurityGroupUpdateRequest) ToProto(nsg *cdbm.NetworkSecurityGroup) *corev1.UpdateNetworkSecurityGroupRequest {
	nsgProto := nsg.ToProto()
	return &corev1.UpdateNetworkSecurityGroupRequest{
		Id:                             nsgProto.Id,
		TenantOrganizationId:           nsgProto.TenantOrganizationId,
		Metadata:                       nsgProto.Metadata,
		NetworkSecurityGroupAttributes: nsgProto.Attributes,
	}
}

type APINetworkSecurityGroupRule struct {
	Name                 *string `json:"name"`
	Direction            string  `json:"direction"`
	SourcePortRange      *string `json:"sourcePortRange"`
	DestinationPortRange *string `json:"destinationPortRange"`
	Protocol             string  `json:"protocol"`
	Action               string  `json:"action"`
	Priority             int     `json:"priority"`
	SourcePrefix         *string `json:"sourcePrefix"`
	DestinationPrefix    *string `json:"destinationPrefix"`
}

// APINetworkSecurityGroupStats holds detailed usage stats for an NSG
type APINetworkSecurityGroupStats struct {
	// InUse is a convenience field that will be true
	// if TotalAttachmentCount > 0
	InUse bool `json:"inUse"`
	// VpcAttachmentCount holds the count of the number of VPCs that have the NSG directly attached.
	VpcAttachmentCount int `json:"directVpcAttachmentCount"`
	// InstanceAttachmentCount holds the count of the number of instances that have the NSG directly attached.
	InstanceAttachmentCount int `json:"directInstanceAttachmentCount"`
	// TotalAttachmentCount holds the total count of all objects that have
	// the NSG directly attached.
	TotalAttachmentCount int `json:"totalDirectAttachmentCount"`
}

// APINetworkSecurityGroupSummary is the data structure to capture API summary of a NetworkSecurityGroup
type APINetworkSecurityGroupSummary struct {
	// ID of the NetworkSecurityGroup
	ID string `json:"id"`
	// Name of the NetworkSecurityGroup
	Name string `json:"name"`
	// Description of the NetworkSecurityGroup
	Description *string `json:"description"`
	// Status is the status of the NetworkSecurityGroup
	Status string `json:"status"`
	// StatefulEgress defines whether a NetworkSecurityGroup's egress rules will be automatically stateful
	StatefulEgress bool `json:"statefulEgress"`
	// RuleCount hold the count of the number of rules in the NetworkSecurityGroup
	RuleCount int `json:"ruleCount"`
}

// NewAPINetworkSecurityGroupSummary accepts a DB layer NetworkSecurityGroup object returns an API layer object
func NewAPINetworkSecurityGroupSummary(dbsg *cdbm.NetworkSecurityGroup) *APINetworkSecurityGroupSummary {
	asg := APINetworkSecurityGroupSummary{
		ID:             dbsg.ID,
		Name:           dbsg.Name,
		Description:    dbsg.Description,
		Status:         dbsg.Status,
		RuleCount:      len(dbsg.Rules),
		StatefulEgress: dbsg.StatefulEgress,
	}

	return &asg
}

type APINetworkSecurityGroupPropagationDetails struct {
	// The ID of the object (VPC/Instance/etc) for these details
	ObjectIDDeprecated *string `json:"object_id,omitempty"`
	// The ID of the object (VPC/Instance/etc) for these details
	ObjectID string `json:"objectId"`
	// The detailed propagation status that was
	// actually returned from NICo
	DetailedStatus string `json:"detailedStatus"`
	// The simplified propagation status
	// that reduces the actual status to just
	// a few values.
	Status string `json:"status"`
	// Additional details for the status
	Details *string `json:"details"`
	// IDs of the instances involved in determining the
	// propagation status
	RelatedInstanceIds []string `json:"relatedInstanceIds"`
	// IDs of any instances associated with the ObjectID that have
	// not yet updated their NSG rules.
	UnpropagatedInstanceIds []string `json:"unpropagatedInstanceIds"`
	// Deprecations is the list of deprecations for the NetworkSecurityGroupPropagationDetails
	Deprecations []APIDeprecation `json:"deprecations,omitempty"`
}

func NewAPINetworkSecurityGroupPropagationDetails(s *cdbm.NetworkSecurityGroupPropagationDetails) *APINetworkSecurityGroupPropagationDetails {
	if s == nil {
		return nil
	}

	details := &APINetworkSecurityGroupPropagationDetails{
		ObjectID:                s.NetworkSecurityGroupPropagationObjectStatus.Id,
		Details:                 s.NetworkSecurityGroupPropagationObjectStatus.Details,
		RelatedInstanceIds:      s.NetworkSecurityGroupPropagationObjectStatus.RelatedInstanceIds,
		UnpropagatedInstanceIds: s.NetworkSecurityGroupPropagationObjectStatus.UnpropagatedInstanceIds,
	}

	if time.Now().Before(networkSecurityGroupPropagationObjectIDDeprecationTime) {
		details.ObjectIDDeprecated = cutil.GetPtr(s.NetworkSecurityGroupPropagationObjectStatus.Id)
	}

	status, found := NetworkSecurityGroupRuleAPIPropagationDetailedStatusFromProtobufPropagationStatus[s.Status]
	if !found {
		// We could return an error, but we should probably _not_ fail
		// a VPC/Instance/etc handler response just because a new status
		// arrived in the proto and we don't know about it; so, we can respond
		// with Unknown, since we don't know, and provide the details message
		// so that users, and we, know this is related to a mismatch between
		// site data and our expectations.
		status = APINetworkSecurityGroupPropagationDetailedStatusUnknown
		details.Details = cutil.GetPtr("Unknown status type reported from Site")
	}

	details.DetailedStatus = status

	status, found = NetworkSecurityGroupRuleAPIPropagationStatusFromProtobufPropagationStatus[s.Status]
	if !found {
		status = APINetworkSecurityGroupPropagationStatusError
	}

	details.Status = status

	for _, deprecation := range networkSecurityGroupPropagationDetailsDeprecations {
		details.Deprecations = append(details.Deprecations, NewAPIDeprecation(deprecation))
	}

	return details
}
