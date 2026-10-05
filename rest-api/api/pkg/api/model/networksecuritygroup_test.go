// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"

	cutil "github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/dsx-ai-factory/infra-controller/rest-api/proto/core/gen/v1"
)

// A helper only for tests.  Ignores potential conversion errors.
func getIntPtrToUint32Ptr(i *int) *uint32 {
	if i == nil {
		return nil
	}

	i32 := uint32(*i)

	return &i32
}

func TestAPINetworkSecurityGroupRuleConversions(t *testing.T) {

	directionEnumLimit := len(corev1.NetworkSecurityGroupRuleDirection_value)
	protocolEnumLimit := len(corev1.NetworkSecurityGroupRuleProtocol_value)
	actionEnumLimit := len(corev1.NetworkSecurityGroupRuleAction_value)

	srcPortStarts := []*uint32{
		nil,
		getIntPtrToUint32Ptr(cutil.GetPtr(50)),
	}

	srcPortEnds := []*uint32{
		nil,
		getIntPtrToUint32Ptr(cutil.GetPtr(50)),
	}

	dstPortStarts := []*uint32{
		nil,
		getIntPtrToUint32Ptr(cutil.GetPtr(80)),
	}

	dstPortEnds := []*uint32{
		nil,
		getIntPtrToUint32Ptr(cutil.GetPtr(80)),
	}

	allRules := []*cdbm.NetworkSecurityGroupRule{}
	validRules := []*cdbm.NetworkSecurityGroupRule{}

	// Generate all the rules combinations

	for dI := range directionEnumLimit {
		for pI := range protocolEnumLimit {
			for aI := range actionEnumLimit {
				for _, sps := range srcPortStarts {
					for _, spe := range srcPortEnds {
						for _, dps := range dstPortStarts {
							for _, dpe := range dstPortEnds {

								d := corev1.NetworkSecurityGroupRuleDirection(uint32(dI))
								p := corev1.NetworkSecurityGroupRuleProtocol(uint32(pI))
								a := corev1.NetworkSecurityGroupRuleAction(uint32(aI))

								newRule := &cdbm.NetworkSecurityGroupRule{
									NetworkSecurityGroupRuleAttributes: &corev1.NetworkSecurityGroupRuleAttributes{
										Id:             cutil.GetPtr(uuid.NewString()),
										Direction:      d,
										Protocol:       p,
										Action:         a,
										Priority:       55,
										Ipv6:           false,
										SrcPortStart:   sps,
										SrcPortEnd:     spe,
										DstPortStart:   dps,
										DstPortEnd:     dpe,
										SourceNet:      &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: "0.0.0.0/0"},
										DestinationNet: &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: "1.1.1.1/0"},
									},
								}

								allRules = append(allRules, newRule)

								if d != corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_INVALID &&
									p != corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_INVALID &&
									// These fixture prefixes are IPv4, so ICMP6 is invalid.
									p != corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_ICMP6 &&
									a != corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_INVALID &&
									// src/dst start and end pairs are mutually required.
									// Either start and end or both nil or neither is allowed to be nil.
									((sps == nil) == (spe == nil)) &&
									((dps == nil) == (dpe == nil)) &&
									// Exclude rules that have invalid port + protocol combinations.
									!((sps != nil || dps != nil) &&
										(p == corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_ANY ||
											p == corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_ICMP ||
											p == corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_ICMP6)) {

									validRules = append(validRules, newRule)
									continue
								}
							}
						}
					}
				}

			}
		}
	}

	// The set of valid rules should not be as big
	// as the set of all the generated rules.
	assert.True(t, len(allRules) > len(validRules))

	// Round-trip the valid rules through FromProto and ToProto. After
	// the layered-proto-conversion refactor, FromProto / ToProto no
	// longer return errors: they trust their input. The valid set
	// here is exactly the set of inputs the previous-stage validation
	// (Validate) would have allowed through.
	for i, rule := range validRules {
		apiRule := NewAPINetworkSecurityGroupRule(rule.NetworkSecurityGroupRuleAttributes)

		assert.NotNil(t, apiRule, "expected non-nil API rule for valid proto attrs")
		require.NoError(t, apiRule.Validate())

		newAttrs := apiRule.ToProto()

		assert.True(t,
			proto.Equal(validRules[i].NetworkSecurityGroupRuleAttributes, newAttrs),
			fmt.Sprintf("\nNICo Rule\n\n%+v\n\nAPI Rule\n\n%+v\n\nNew NICo Rule\n\n%+v\n", rule, apiRule, newAttrs),
		)
	}

	// Test some invalid API request cases.
	// The first entry of each axis is a known-good value; the rest
	// are invalid. With Validate now owning request validation, we
	// drive each rule through `Validate()` instead of `ToProto()`,
	// and assert that any non-zero index along any axis produces an
	// error. The "known good" combo (all axes at index 0) must
	// round-trip cleanly through ToProto and FromProto.
	directions := []string{APINetworkSecurityGroupRuleDirectionIngress, "outer space", ""}
	protocol := []string{APINetworkSecurityGroupRuleProtocolTcp, "MPLS", ""}
	actions := []string{APINetworkSecurityGroupRuleActionPermit, "explode", ""}
	priorities := []int{0, -1, 99999}

	srcPortRanges := []string{"80-81", "abc", "a-b", "-70", "70-"}
	dstPortRanges := []string{"90-91", "xyz", "d-e", "-90", "90-"}
	srcPrefixes := []*string{cutil.GetPtr("0.0.0.0/0"), nil}
	dstPrefixes := []*string{cutil.GetPtr("1.1.1.1/0"), nil}

	for dI, d := range directions {
		for pI, p := range protocol {
			for aI, a := range actions {
				for srI, sr := range srcPortRanges {
					for drI, dr := range dstPortRanges {
						for spI, sp := range srcPrefixes {
							for dpI, dp := range dstPrefixes {
								for prioI, prio := range priorities {

									rule := &APINetworkSecurityGroupRule{
										Direction:            d,
										Protocol:             p,
										Action:               a,
										SourcePortRange:      cutil.GetPtr(sr),
										DestinationPortRange: cutil.GetPtr(dr),
										SourcePrefix:         sp,
										DestinationPrefix:    dp,
										Priority:             prio,
									}

									failMsg := fmt.Sprintf("\n%v\n%v\n%v\n%v\n%v\n%v\n%v\n%d\n\n%+v\n", d, p, a, sr, dr, sp, dp, prio, rule)

									err := rule.Validate()

									// If this rule has all the known good entries,
									// it should have passed (validated successfully)
									// and ToProto/FromProto should be symmetric.
									if dI == 0 && pI == 0 && aI == 0 && srI == 0 && drI == 0 && spI == 0 && dpI == 0 && prioI == 0 {
										assert.Nil(t, err, failMsg)

										nicoAttrs := rule.ToProto()

										apiRule := NewAPINetworkSecurityGroupRule(nicoAttrs)

										// Compare the original rule to the one that
										// came out of the double-conversion.
										assert.Equal(t, rule, apiRule, failMsg)

									} else {
										// For every other case Validate should fail.
										// The combos ensure that each bad property
										// gets tested with every other property set to
										// a known good value.
										assert.NotNil(t, err, failMsg)
									}
								}
							}
						}
					}
				}
			}
		}
	}
}

func TestAPINetworkSecurityGroupRule_Validate(t *testing.T) {
	tests := []struct {
		name        string
		source      string
		destination string
		protocol    string
		wantIPv6    bool
		wantError   string
	}{
		{name: "IPv6 TCP", source: "2001:db8:1::/64", destination: "2001:db8:2::/64", protocol: "TCP", wantIPv6: true},
		{name: "IPv6 ICMP6 with normalized casing", source: "::/0", destination: "2001:db8::/64", protocol: "icmp6", wantIPv6: true},
		{name: "IPv4-mapped IPv6 stays IPv6", source: "::ffff:192.0.2.0/120", destination: "::ffff:198.51.100.0/120", protocol: "TCP", wantIPv6: true},
		{name: "mixed prefix families", source: "192.0.2.0/24", destination: "2001:db8::/64", protocol: "TCP", wantError: "rules: source and destination prefixes must use the same IP version."},
		{name: "ICMP with IPv6", source: "::/0", destination: "2001:db8::/64", protocol: "ICMP", wantError: "rules: protocol `ICMP` cannot be used with IPv6 prefixes."},
		{name: "ICMP6 with IPv4", source: "0.0.0.0/0", destination: "192.0.2.0/24", protocol: "ICMP6", wantError: "rules: protocol `ICMP6` cannot be used with IPv4 prefixes."},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			rule := APINetworkSecurityGroupRule{
				Direction:         APINetworkSecurityGroupRuleDirectionIngress,
				Action:            APINetworkSecurityGroupRuleActionPermit,
				Protocol:          test.protocol,
				SourcePrefix:      &test.source,
				DestinationPrefix: &test.destination,
			}
			err := rule.Validate()
			if test.wantError != "" {
				require.EqualError(t, err, test.wantError)
				return
			}
			require.NoError(t, err)
			attrs := rule.ToProto()
			assert.Equal(t, test.wantIPv6, attrs.Ipv6)
			assert.Equal(t, test.source, attrs.GetSrcPrefix())
			assert.Equal(t, test.destination, attrs.GetDstPrefix())
			assert.Equal(t, rule, *NewAPINetworkSecurityGroupRule(attrs))
		})
	}
}

func TestAPINetworkSecurityGroupCreateRequest_Validate(t *testing.T) {

	// Rule-level validation now runs inside the parent Validate, so
	// the fixture rule must itself be valid for the "ok" cases below
	// to pass.
	rules := []APINetworkSecurityGroupRule{
		{
			Direction:         APINetworkSecurityGroupRuleDirectionIngress,
			SourcePortRange:   cutil.GetPtr("80-81"),
			Protocol:          APINetworkSecurityGroupRuleProtocolTcp,
			Action:            APINetworkSecurityGroupRuleActionPermit,
			SourcePrefix:      cutil.GetPtr("0.0.0.0/0"),
			DestinationPrefix: cutil.GetPtr("0.0.0.0/0"),
		},
	}

	tests := []struct {
		desc       string
		obj        APINetworkSecurityGroupCreateRequest
		siteConfig *cdbm.SiteConfig
		expectErr  bool
	}{
		{
			desc:      "ok when only required fields are provided",
			obj:       APINetworkSecurityGroupCreateRequest{Name: "test", SiteID: uuid.New().String()},
			expectErr: false,
		},
		{
			desc:      "ok when all fields are provided",
			obj:       APINetworkSecurityGroupCreateRequest{ID: cutil.GetPtr(uuid.New()), Name: "test", Description: cutil.GetPtr("test"), SiteID: uuid.New().String(), StatefulEgress: true, Rules: rules},
			expectErr: false,
		},
		{
			desc:      "error when required fields are not provided",
			obj:       APINetworkSecurityGroupCreateRequest{Name: "test", Rules: rules},
			expectErr: true,
		},
		{
			desc:       "error when too many rules are sent",
			obj:        APINetworkSecurityGroupCreateRequest{Name: "test", Rules: rules},
			siteConfig: &cdbm.SiteConfig{MaxNetworkSecurityGroupRuleCount: cutil.GetPtr(0)},
			expectErr:  true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate(tc.siteConfig)
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestAPINetworkSecurityGroupUpdateRequest_Validate(t *testing.T) {

	// Rule-level validation now runs inside the parent Validate, so
	// the fixture rule must itself be valid for the "ok" cases below
	// to pass.
	rules := []APINetworkSecurityGroupRule{
		{
			Direction:         APINetworkSecurityGroupRuleDirectionIngress,
			SourcePortRange:   cutil.GetPtr("80-81"),
			Protocol:          APINetworkSecurityGroupRuleProtocolTcp,
			Action:            APINetworkSecurityGroupRuleActionPermit,
			SourcePrefix:      cutil.GetPtr("0.0.0.0/0"),
			DestinationPrefix: cutil.GetPtr("0.0.0.0/0"),
		},
	}

	tests := []struct {
		desc       string
		obj        APINetworkSecurityGroupUpdateRequest
		siteConfig *cdbm.SiteConfig
		expectErr  bool
	}{
		{
			desc:      "ok when only some fields are provided",
			obj:       APINetworkSecurityGroupUpdateRequest{Name: cutil.GetPtr("updatedname")},
			expectErr: false,
		},
		{
			desc:      "ok when all fields are provided",
			obj:       APINetworkSecurityGroupUpdateRequest{Name: cutil.GetPtr("updatedname"), Description: cutil.GetPtr("updated"), StatefulEgress: cutil.GetPtr(true), Rules: rules},
			expectErr: false,
		},
		{
			desc:       "error when too many rules are sent",
			obj:        APINetworkSecurityGroupUpdateRequest{Name: cutil.GetPtr("updatedname"), Description: cutil.GetPtr("updated"), Rules: rules},
			siteConfig: &cdbm.SiteConfig{MaxNetworkSecurityGroupRuleCount: cutil.GetPtr(0)},
			expectErr:  true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate(tc.siteConfig)
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestAPINetworkSecurityGroupNew(t *testing.T) {
	rules := []*cdbm.NetworkSecurityGroupRule{
		{
			NetworkSecurityGroupRuleAttributes: &corev1.NetworkSecurityGroupRuleAttributes{
				Action:         corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_PERMIT,
				Direction:      corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_INGRESS,
				SrcPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(0)),
				SrcPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(100)),
				Protocol:       corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_TCP,
				SourceNet:      &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: "0.0.0.0/0"},
				DestinationNet: &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: "0.0.0.0/0"},
			},
		},
	}

	dbSG := &cdbm.NetworkSecurityGroup{
		ID:             uuid.NewString(),
		Name:           "test",
		StatefulEgress: true,
		Rules:          rules,
		Description:    cutil.GetPtr("test"),
		SiteID:         uuid.New(),
		TenantID:       uuid.New(),
		Status:         cdbm.NetworkSecurityGroupStatusReady,
		Created:        cdb.GetCurTime(),
		Updated:        cdb.GetCurTime(),
	}
	dbsds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dbSG.ID,
			Status:   cdbm.NetworkSecurityGroupStatusReady,
			Created:  time.Now(),
			Updated:  time.Now(),
		},
	}

	tests := []struct {
		desc  string
		dbObj *cdbm.NetworkSecurityGroup
		dbSds []cdbm.StatusDetail
	}{
		{
			desc:  "test creating API NetworkSecurityGroup",
			dbObj: dbSG,
			dbSds: dbsds,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPINetworkSecurityGroup(tc.dbObj, tc.dbSds)
			assert.Equal(t, tc.dbObj.ID, got.ID)
		})
	}
}

func TestAPINetworkSecurityGroupNewSummary(t *testing.T) {
	rules := []*cdbm.NetworkSecurityGroupRule{
		{
			NetworkSecurityGroupRuleAttributes: &corev1.NetworkSecurityGroupRuleAttributes{
				Action:         corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_PERMIT,
				Direction:      corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_INGRESS,
				SrcPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(0)),
				SrcPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(100)),
				Protocol:       corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_TCP,
				SourceNet:      &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: "0.0.0.0/0"},
				DestinationNet: &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: "0.0.0.0/0"},
			},
		},
	}

	dbSG := &cdbm.NetworkSecurityGroup{
		ID:             uuid.NewString(),
		Name:           "test",
		StatefulEgress: true,
		Rules:          rules,
		Description:    cutil.GetPtr("test"),
		SiteID:         uuid.New(),
		TenantID:       uuid.New(),
		Status:         cdbm.NetworkSecurityGroupStatusReady,
		Created:        cdb.GetCurTime(),
		Updated:        cdb.GetCurTime(),
	}

	tests := []struct {
		desc  string
		dbObj *cdbm.NetworkSecurityGroup
		dbSds []cdbm.StatusDetail
	}{
		{
			desc:  "test creating API NetworkSecurityGroupSummary",
			dbObj: dbSG,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPINetworkSecurityGroupSummary(tc.dbObj)
			assert.Equal(t, tc.dbObj.ID, got.ID)
			assert.True(t, len(tc.dbObj.Rules) > 0, "Add some rules for the NSG for this test.")
			assert.Equal(t, len(tc.dbObj.Rules), got.RuleCount)
		})
	}
}

func TestNewAPINetworkSecurityGroupRule(t *testing.T) {
	t.Run("nil attrs returns nil rule", func(t *testing.T) {
		rule := NewAPINetworkSecurityGroupRule(nil)
		assert.Nil(t, rule)
	})

	t.Run("unknown enum produces an empty enum field but a non-nil rule", func(t *testing.T) {
		// FromProto is defensive: an unrecognized direction enum
		// leaves the corresponding API field empty rather than
		// erroring. Any stricter handling belongs in a DB-integrity
		// check, not in the conversion layer.
		attrs := &corev1.NetworkSecurityGroupRuleAttributes{
			Direction: corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_INVALID,
		}
		rule := NewAPINetworkSecurityGroupRule(attrs)
		assert.NotNil(t, rule)
		assert.Equal(t, "", rule.Direction)
	})

	t.Run("valid attrs produce a populated rule", func(t *testing.T) {
		ruleID := cutil.GetPtr("rule-id")
		attrs := &corev1.NetworkSecurityGroupRuleAttributes{
			Id:             ruleID,
			Direction:      corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_INGRESS,
			Action:         corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_PERMIT,
			Protocol:       corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_TCP,
			Priority:       55,
			SourceNet:      &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: "0.0.0.0/0"},
			DestinationNet: &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: "1.1.1.1/0"},
		}
		rule := NewAPINetworkSecurityGroupRule(attrs)
		assert.NotNil(t, rule)
		assert.Equal(t, APINetworkSecurityGroupRuleDirectionIngress, rule.Direction)
		assert.Equal(t, APINetworkSecurityGroupRuleActionPermit, rule.Action)
		assert.Equal(t, APINetworkSecurityGroupRuleProtocolTcp, rule.Protocol)
		assert.Equal(t, 55, rule.Priority)
	})
}

func TestAPINetworkSecurityGroupCreateRequest_Validate_RuleErrors(t *testing.T) {
	// The parent Validate must surface rule-level validation failures
	// so that callers never reach a request-shape ToProto with a bad
	// rule. The simplest signal is a known-bad direction.
	rules := []APINetworkSecurityGroupRule{
		{Direction: "nope", Protocol: APINetworkSecurityGroupRuleProtocolTcp, Action: APINetworkSecurityGroupRuleActionPermit, SourcePrefix: cutil.GetPtr("0.0.0.0/0"), DestinationPrefix: cutil.GetPtr("0.0.0.0/0")},
	}
	req := APINetworkSecurityGroupCreateRequest{Name: "test", SiteID: uuid.New().String(), Rules: rules}
	err := req.Validate(nil)
	assert.Error(t, err)
}

func TestAPINetworkSecurityGroupCreateRequest_ToProto(t *testing.T) {
	// Build a request and the corresponding (just-persisted) DB
	// record. The DB record provides the canonical Metadata; the
	// request-shape ToProto sources rules / statefulEgress from the
	// request and the wire envelope (ID / Metadata) from the entity.
	siteID := uuid.New()
	tenantID := uuid.New()
	requestedID := uuid.New()
	req := APINetworkSecurityGroupCreateRequest{
		ID:             &requestedID,
		Name:           "test-nsg",
		Description:    cutil.GetPtr("desc"),
		SiteID:         siteID.String(),
		StatefulEgress: true,
		Rules: []APINetworkSecurityGroupRule{
			{
				Direction:         APINetworkSecurityGroupRuleDirectionIngress,
				Protocol:          APINetworkSecurityGroupRuleProtocolTcp,
				Action:            APINetworkSecurityGroupRuleActionPermit,
				SourcePrefix:      cutil.GetPtr("0.0.0.0/0"),
				DestinationPrefix: cutil.GetPtr("1.1.1.1/0"),
			},
			{
				Direction:         APINetworkSecurityGroupRuleDirectionIngress,
				Protocol:          APINetworkSecurityGroupRuleProtocolTcp,
				Action:            APINetworkSecurityGroupRuleActionPermit,
				SourcePrefix:      cutil.GetPtr("2001:db8:1::/64"),
				DestinationPrefix: cutil.GetPtr("2001:db8:2::/64"),
			},
		},
		Labels: map[string]string{"env": "test"},
	}

	require.NoError(t, req.Validate(nil))

	nsg := &cdbm.NetworkSecurityGroup{
		ID:             requestedID.String(),
		Name:           req.Name,
		Description:    req.Description,
		SiteID:         siteID,
		TenantID:       tenantID,
		TenantOrg:      "tenant-org",
		Labels:         req.Labels,
		StatefulEgress: req.StatefulEgress,
	}

	got := req.ToProto(nsg)
	require.NotNil(t, got)
	assert.Equal(t, requestedID.String(), *got.Id)
	assert.Equal(t, "tenant-org", got.TenantOrganizationId)
	require.NotNil(t, got.Metadata)
	assert.Equal(t, req.Name, got.Metadata.Name)
	assert.Equal(t, *req.Description, got.Metadata.Description)
	require.NotNil(t, got.NetworkSecurityGroupAttributes)
	assert.Equal(t, req.StatefulEgress, got.NetworkSecurityGroupAttributes.StatefulEgress)
	require.Len(t, got.NetworkSecurityGroupAttributes.Rules, 2)
	assert.False(t, got.NetworkSecurityGroupAttributes.Rules[0].Ipv6)
	assert.True(t, got.NetworkSecurityGroupAttributes.Rules[1].Ipv6)
}

func TestAPINetworkSecurityGroupUpdateRequest_ToProto(t *testing.T) {
	// The update flow writes the request data into the DB record
	// before calling ToProto, so the DB record's `ToProto()` is the
	// canonical wire form for both Metadata and Attributes. We mimic
	// that here by setting the post-merge fields directly on the
	// `nsg` argument.
	siteID := uuid.New()
	tenantID := uuid.New()
	nsg := &cdbm.NetworkSecurityGroup{
		ID:             uuid.NewString(),
		Name:           "updated-name",
		Description:    cutil.GetPtr("updated-desc"),
		SiteID:         siteID,
		TenantID:       tenantID,
		TenantOrg:      "tenant-org",
		Labels:         map[string]string{"env": "prod"},
		StatefulEgress: true,
		Rules: []*cdbm.NetworkSecurityGroupRule{
			{
				NetworkSecurityGroupRuleAttributes: &corev1.NetworkSecurityGroupRuleAttributes{
					Direction: corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_INGRESS,
					Action:    corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_PERMIT,
					Protocol:  corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_TCP,
				},
			},
		},
	}

	req := APINetworkSecurityGroupUpdateRequest{
		Name:           cutil.GetPtr("updated-name"),
		Description:    cutil.GetPtr("updated-desc"),
		StatefulEgress: cutil.GetPtr(true),
	}

	got := req.ToProto(nsg)
	assert.NotNil(t, got)
	assert.Equal(t, nsg.ID, got.Id)
	assert.Equal(t, "tenant-org", got.TenantOrganizationId)
	assert.NotNil(t, got.Metadata)
	assert.Equal(t, nsg.Name, got.Metadata.Name)
	assert.Equal(t, *nsg.Description, got.Metadata.Description)
	assert.NotNil(t, got.NetworkSecurityGroupAttributes)
	assert.Equal(t, nsg.StatefulEgress, got.NetworkSecurityGroupAttributes.StatefulEgress)
	assert.Equal(t, 1, len(got.NetworkSecurityGroupAttributes.Rules))
}
