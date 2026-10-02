// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"strings"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/types/known/timestamppb"
)

func testPrefix() *corev1.SitePrefix {
	return &corev1.SitePrefix{
		Id: &corev1.SitePrefixId{Value: uuid.NewString()},
		Config: &corev1.SitePrefixConfig{Prefix: "10.0.0.0/24",
			RoutingScope: corev1.SitePrefixRoutingScope_SITE_PREFIX_ROUTING_SCOPE_DATACENTER_ONLY},
		Status: &corev1.SitePrefixStatus{
			Authority:      corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_OPERATOR_MANAGED,
			LifecycleState: corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY},
		Metadata: &corev1.Metadata{Name: "test-prefix", Description: "reported by Core"},
	}
}

func testInventory(prefix *corev1.SitePrefix) *corev1.SitePrefixInventory {
	return &corev1.SitePrefixInventory{
		Timestamp: timestamppb.Now(), InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		InventoryPage: &corev1.InventoryPage{CurrentPage: 1, TotalPages: 1, TotalItems: 1, PageSize: 25,
			ItemIds: []string{prefix.Id.Value}},
		SitePrefixes: []*corev1.SitePrefix{prefix},
	}
}

func TestValidatePrefix(t *testing.T) {
	type testCase struct {
		name   string
		change func(*corev1.SitePrefix) *corev1.SitePrefix
		valid  bool
	}
	tests := []testCase{
		{"operator", func(p *corev1.SitePrefix) *corev1.SitePrefix { return p }, true},
		{"missing prefix", func(_ *corev1.SitePrefix) *corev1.SitePrefix { return nil }, false},
		{"missing ID", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Id = nil; return p }, false},
		{"invalid ID", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Id.Value = "invalid"; return p }, false},
		{"nil UUID", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Id.Value = uuid.Nil.String(); return p }, false},
		{"noncanonical ID", func(p *corev1.SitePrefix) *corev1.SitePrefix {
			p.Id.Value = "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA"
			return p
		}, false},
		{"missing config", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Config = nil; return p }, false},
		{"missing status", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Status = nil; return p }, false},
		{"unspecified authority", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Status.Authority = 0; return p }, false},
		{"unspecified state", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Status.LifecycleState = 0; return p }, false},
		{"operator provisioning", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Status.LifecycleState = 1; return p }, false},
		{"operator owner", func(p *corev1.SitePrefix) *corev1.SitePrefix {
			p.Config.TenantOrganizationId = cutil.GetPtr("tenant")
			return p
		}, false},
		{"unsupported routing", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Config.RoutingScope = 0; return p }, false},
		{"host bits", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Config.Prefix = "10.0.0.1/24"; return p }, false},
		{"operator v6", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Config.Prefix = "fd00::/64"; return p }, true},
		{"metadata absent", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Metadata = nil; return p }, false},
		{"metadata bounds", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Metadata.Name = strings.Repeat("x", 257); return p }, false},
		{"duplicate label", func(p *corev1.SitePrefix) *corev1.SitePrefix {
			p.Metadata.Labels = []*corev1.Label{{Key: "x"}, {Key: "x"}}
			return p
		}, false},
	}
	for _, tt := range []struct {
		cidr  string
		valid bool
	}{
		{"10.0.0.0/8", true}, {"172.16.0.0/12", true}, {"192.168.0.0/31", true},
		{"10.0.0.0/7", false}, {"10.0.0.0/32", false}, {"192.168.0.0/15", false},
		{"100.64.0.0/10", false}, {"203.0.113.0/24", false}, {"fd00::/64", false},
	} {
		tests = append(tests, testCase{"tenant " + tt.cidr, func(p *corev1.SitePrefix) *corev1.SitePrefix {
			p.Status.Authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
			p.Config.TenantOrganizationId = cutil.GetPtr("tenant")
			p.Config.Prefix = tt.cidr
			return p
		}, tt.valid})
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := validatePrefix(tt.change(testPrefix()))
			if tt.valid {
				require.NoError(t, err)
			} else {
				var applicationErr *temporal.ApplicationError
				require.ErrorAs(t, err, &applicationErr)
				require.True(t, applicationErr.NonRetryable())
			}
		})
	}
}
