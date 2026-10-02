// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"fmt"
	"net/netip"
	"unicode/utf8"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"go.temporal.io/sdk/temporal"
)

func invalid(format string, args ...any) error {
	return temporal.NewNonRetryableApplicationError(fmt.Sprintf(format, args...), "InvalidSitePrefixInventory", nil)
}

func validatePrefix(prefix *corev1.SitePrefix) error {
	if prefix == nil || prefix.Config == nil || prefix.Status == nil || prefix.Metadata == nil {
		return invalid("SitePrefix requires config, status, and metadata")
	}
	id, err := uuid.Parse(prefix.GetId().GetValue())
	if err != nil || id == uuid.Nil || id.String() != prefix.GetId().GetValue() {
		return invalid("SitePrefix requires a canonical, nonzero UUID")
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
