// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"crypto/sha256"
	"fmt"
	"net/netip"
	"slices"
	"unicode/utf8"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/proto"
)

func invalid(format string, args ...any) error {
	return temporal.NewNonRetryableApplicationError(fmt.Sprintf(format, args...), "InvalidSitePrefixInventory", nil)
}

// validatePage validates the entire page before any record is reconciled.
// Nonfinal total_pages is an estimate, not a fixed collection boundary.
func validatePage(inventory *corev1.SitePrefixInventory) (string, error) {
	if inventory == nil || inventory.Timestamp == nil || inventory.Timestamp.CheckValid() != nil {
		return "", invalid("SitePrefix inventory requires a valid collection timestamp")
	}
	if inventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		if inventory.InventoryPage != nil || len(inventory.SitePrefixes) != 0 {
			return "", invalid("failed inventory must be unpaged and contain no resources")
		}
		return "", nil
	}
	if inventory.InventoryStatus != corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS {
		return "", invalid("unsupported SitePrefix inventory status")
	}
	page := inventory.InventoryPage
	if page == nil || page.CurrentPage < 1 || page.PageSize < 1 || page.TotalItems < 0 ||
		int64(page.TotalItems) != int64(len(page.ItemIds)) || int64(len(inventory.SitePrefixes)) > int64(page.PageSize) {
		return "", invalid("invalid SitePrefix page dimensions")
	}
	if page.TotalItems == 0 {
		if page.CurrentPage != 1 || page.TotalPages != 0 || len(inventory.SitePrefixes) != 0 {
			return "", invalid("invalid empty SitePrefix inventory")
		}
	} else if page.TotalPages < page.CurrentPage || page.TotalPages > page.TotalItems || len(inventory.SitePrefixes) == 0 {
		return "", invalid("invalid nonempty SitePrefix page")
	}
	for index, id := range page.ItemIds {
		parsed, err := uuid.Parse(id)
		if err != nil || parsed == uuid.Nil || parsed.String() != id || (index > 0 && page.ItemIds[index-1] >= id) {
			return "", invalid("inventory IDs must be canonical, unique UUIDs in order")
		}
	}
	previous := ""
	for _, prefix := range inventory.SitePrefixes {
		id := prefix.GetId().GetValue()
		_, found := slices.BinarySearch(page.ItemIds, id)
		if !found || id <= previous {
			return "", invalid("page contains an undeclared, duplicate, or unordered SitePrefix ID")
		}
		previous = id
		err := validatePrefix(prefix)
		if err != nil {
			return "", err
		}
	}
	wire, err := (proto.MarshalOptions{Deterministic: true}).Marshal(inventory)
	if err != nil {
		return "", invalid("cannot encode inventory: %v", err)
	}
	return fmt.Sprintf("%x", sha256.Sum256(wire)), nil
}

func validatePrefix(prefix *corev1.SitePrefix) error {
	if prefix == nil || prefix.Config == nil || prefix.Status == nil || prefix.Metadata == nil {
		return invalid("SitePrefix requires config, status, and metadata")
	}
	config, status, metadata := prefix.Config, prefix.Status, prefix.Metadata
	cidr, err := netip.ParsePrefix(config.Prefix)
	if err != nil || cidr != cidr.Masked() || cidr.Addr().Is4In6() {
		return invalid("SitePrefix %s requires a network-aligned CIDR", prefix.GetId().GetValue())
	}
	if config.RoutingScope != corev1.SitePrefixRoutingScope_SITE_PREFIX_ROUTING_SCOPE_DATACENTER_ONLY {
		return invalid("unsupported SitePrefix routing scope")
	}
	minName := 0
	switch status.Authority {
	case corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_OPERATOR_MANAGED:
		if config.TenantOrganizationId != nil || (status.LifecycleState != corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY &&
			status.LifecycleState != corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING) {
			return invalid("operator SitePrefix has an owner or unsupported lifecycle")
		}
	case corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED:
		minName = 2
		if config.GetTenantOrganizationId() == "" || !cidr.Addr().Is4() || cidr.Bits() < 8 || cidr.Bits() > 31 {
			return invalid("tenant SitePrefix requires an owner and RFC1918 IPv4 /8 through /31")
		}
		private := false
		for _, root := range []string{"10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"} {
			parent := netip.MustParsePrefix(root)
			private = private || (parent.Bits() <= cidr.Bits() && parent.Contains(cidr.Addr()))
		}
		if !private || status.LifecycleState < corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_PROVISIONING ||
			status.LifecycleState > corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_ERROR {
			return invalid("unsupported tenant SitePrefix range or lifecycle")
		}
	default:
		return invalid("unsupported SitePrefix authority")
	}
	if len(metadata.Name) < minName || len(metadata.Name) > 256 || !ascii(metadata.Name) ||
		len(metadata.Description) > 1024 || !utf8.ValidString(metadata.Description) || len(metadata.Labels) > 16 {
		return invalid("invalid SitePrefix metadata")
	}
	keys := make(map[string]bool)
	for _, label := range metadata.Labels {
		key := label.GetKey()
		if key == "" || len(key) > 255 || !ascii(key) || len(label.GetValue()) > 255 || keys[key] {
			return invalid("invalid SitePrefix metadata label")
		}
		keys[key] = true
	}
	return nil
}

func ascii(value string) bool {
	for _, char := range value {
		if char > 127 {
			return false
		}
	}
	return true
}
