/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	"net/netip"
	"testing"
)

func FuzzIpamer_AcquireIP(f *testing.F) {
	ctx := context.Background()
	tests := []struct {
		name       string
		prefixCIDR string
		want       string
	}{
		{
			name:       "Acquire next IP regularly",
			prefixCIDR: "192.168.1.0/24",
			want:       "192.168.1.1",
		},
		{
			name:       "Acquire next IPv6 regularly",
			prefixCIDR: "2001:db8:85a3::/124",
			want:       "2001:db8:85a3::1",
		},
		{
			name:       "Want next IP, but network is full",
			prefixCIDR: "192.168.4.0/32",
			want:       "",
		},
		{
			name:       "Want next IPv6, but network is full",
			prefixCIDR: "2001:db8:85a3::/128",
			want:       "",
		},
	}
	for _, tc := range tests {
		tc := tc
		f.Add(tc.prefixCIDR, tc.want)
	}

	f.Fuzz(func(t *testing.T, prefixCIDR, want string) {
		ipam := New(ctx)
		p, err := ipam.NewPrefix(ctx, prefixCIDR)

		if err == nil {
			prefix, err := netip.ParsePrefix(prefixCIDR)
			if err != nil {
				t.Error(err)
			}
			if prefix.Masked().String() != p.String() {
				if err != nil {
					t.Errorf("%q not equal %q", prefix.Masked().String(), p.String())
				}
			}
			ip, err := ipam.AcquireIP(ctx, p.Cidr)
			if err == nil && want != "" && want != ip.IP.String() {
				if err != nil {
					t.Errorf("%q not equal %q", want, ip.IP.String())
				}
			}
		}
	})
}
