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

func TestValidatePage(t *testing.T) {
	tests := []struct {
		name   string
		change func(*corev1.SitePrefixInventory)
		valid  bool
	}{
		{"operator", func(_ *corev1.SitePrefixInventory) {}, true},
		{"empty success", func(i *corev1.SitePrefixInventory) {
			i.SitePrefixes = nil
			i.InventoryPage.ItemIds = nil
			i.InventoryPage.TotalItems, i.InventoryPage.TotalPages = 0, 0
		}, true},
		{"unpaged failure", func(i *corev1.SitePrefixInventory) {
			i.InventoryStatus = corev1.InventoryStatus_INVENTORY_STATUS_FAILED
			i.InventoryPage, i.SitePrefixes = nil, nil
		}, true},
		{"failure with resources", func(i *corev1.SitePrefixInventory) {
			i.InventoryStatus = corev1.InventoryStatus_INVENTORY_STATUS_FAILED
		}, false},
		{"no timestamp", func(i *corev1.SitePrefixInventory) { i.Timestamp = nil }, false},
		{"missing page", func(i *corev1.SitePrefixInventory) { i.InventoryPage = nil }, false},
		{"incorrect count", func(i *corev1.SitePrefixInventory) { i.InventoryPage.TotalItems++ }, false},
		{"undeclared ID", func(i *corev1.SitePrefixInventory) { i.SitePrefixes[0].Id.Value = uuid.NewString() }, false},
		{"missing prefix", func(i *corev1.SitePrefixInventory) { i.SitePrefixes[0] = nil }, false},
		{"unspecified authority", func(i *corev1.SitePrefixInventory) { i.SitePrefixes[0].Status.Authority = 0 }, false},
		{"unspecified state", func(i *corev1.SitePrefixInventory) { i.SitePrefixes[0].Status.LifecycleState = 0 }, false},
		{"operator provisioning", func(i *corev1.SitePrefixInventory) { i.SitePrefixes[0].Status.LifecycleState = 1 }, false},
		{"operator owner", func(i *corev1.SitePrefixInventory) {
			i.SitePrefixes[0].Config.TenantOrganizationId = cutil.GetPtr("tenant")
		}, false},
		{"unsupported routing", func(i *corev1.SitePrefixInventory) { i.SitePrefixes[0].Config.RoutingScope = 0 }, false},
		{"host bits", func(i *corev1.SitePrefixInventory) { i.SitePrefixes[0].Config.Prefix = "10.0.0.1/24" }, false},
		{"operator v6", func(i *corev1.SitePrefixInventory) { i.SitePrefixes[0].Config.Prefix = "fd00::/64" }, true},
		{"metadata absent", func(i *corev1.SitePrefixInventory) { i.SitePrefixes[0].Metadata = nil }, false},
		{"metadata bounds", func(i *corev1.SitePrefixInventory) { i.SitePrefixes[0].Metadata.Name = strings.Repeat("x", 257) }, false},
		{"duplicate label", func(i *corev1.SitePrefixInventory) {
			i.SitePrefixes[0].Metadata.Labels = []*corev1.Label{{Key: "x"}, {Key: "x"}}
		}, false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			inventory := testInventory(testPrefix())
			tt.change(inventory)
			_, err := validatePage(inventory)
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

func TestValidatePrefix(t *testing.T) {
	tests := []struct {
		cidr  string
		valid bool
	}{
		{"10.0.0.0/8", true}, {"172.16.0.0/12", true}, {"192.168.0.0/31", true},
		{"10.0.0.0/7", false}, {"10.0.0.0/32", false}, {"192.168.0.0/15", false},
		{"100.64.0.0/10", false}, {"203.0.113.0/24", false}, {"fd00::/64", false},
	}
	for _, tt := range tests {
		t.Run(tt.cidr, func(t *testing.T) {
			prefix := testPrefix()
			prefix.Status.Authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
			prefix.Config.TenantOrganizationId = cutil.GetPtr("tenant")
			prefix.Config.Prefix = tt.cidr
			err := validatePrefix(prefix)
			if tt.valid {
				require.NoError(t, err)
			} else {
				require.Error(t, err)
			}
		})
	}
}
