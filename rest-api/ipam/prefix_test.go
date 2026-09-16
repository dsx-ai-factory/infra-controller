/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	"errors"
	"fmt"
	"math"
	"net/netip"
	"reflect"
	"strings"
	"sync"
	"testing"

	"github.com/stretchr/testify/require"
	"golang.org/x/sync/errgroup"
)

// getHostAddresses will return all possible ipadresses a host can get in the given prefix.
// The IPs will be acquired by this method, so that the prefix has no free IPs afterwards.
func (i *ipamer) getHostAddresses(ctx context.Context, prefix string) ([]string, error) {
	hostAddresses := []string{}

	p, err := i.NewPrefix(ctx, prefix)
	if err != nil {
		return hostAddresses, err
	}

	// loop till AcquireIP signals that it has no ips left
	for {
		ip, err := i.AcquireIP(ctx, p.Cidr)
		if errors.Is(err, ErrNoIPAvailable) {
			return hostAddresses, nil
		}
		if err != nil {
			return nil, err
		}
		hostAddresses = append(hostAddresses, ip.IP.String())
	}
}

func TestIPRangeOverlapping(t *testing.T) {
	ctx := context.Background()
	i := New(context.Background())

	cidr := "10.10.10.0/24"
	_, err := i.NewPrefix(ctx, cidr)
	require.Nil(t, err)

	cidr = "10.10.10.1/24"
	_, err = i.NewPrefix(ctx, cidr)
	require.NotNil(t, err)
}

func TestIpamer_AcquireIP(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		prefixCIDR  string
		namespace   string
		existingips []string
	}
	tests := []struct {
		name   string
		fields fields
		want   *IP
	}{
		{
			name: "Acquire next IP regularly",
			fields: fields{
				prefixCIDR:  "192.168.1.0/24",
				existingips: []string{},
			},
			want: &IP{IP: netip.MustParseAddr("192.168.1.1"), ParentPrefix: "192.168.1.0/24"},
		},
		{
			name: "Acquire next namespaced IP regularly",
			fields: fields{
				prefixCIDR:  "192.168.1.0/24",
				namespace:   "my-namespace",
				existingips: []string{},
			},
			want: &IP{IP: netip.MustParseAddr("192.168.1.1"), ParentPrefix: "192.168.1.0/24"},
		},
		{
			name: "Acquire next IPv6 regularly",
			fields: fields{
				prefixCIDR:  "2001:0db8:85a3::/124",
				existingips: []string{},
			},
			want: &IP{IP: netip.MustParseAddr("2001:0db8:85a3::1"), ParentPrefix: "2001:0db8:85a3::/124"},
		},
		{
			name: "Acquire next namespaced IPv6 regularly",
			fields: fields{
				prefixCIDR:  "2001:0db8:85a3::/124",
				namespace:   "my-namespace",
				existingips: []string{},
			},
			want: &IP{IP: netip.MustParseAddr("2001:0db8:85a3::1"), ParentPrefix: "2001:0db8:85a3::/124"},
		},
		{
			name: "Want next IP, network already occupied a little",
			fields: fields{
				prefixCIDR:  "192.168.2.0/30",
				existingips: []string{"192.168.2.1"},
			},
			want: &IP{IP: netip.MustParseAddr("192.168.2.2"), ParentPrefix: "192.168.2.0/30"},
		},
		{
			name: "Want next IPv6, network already occupied a little",
			fields: fields{
				prefixCIDR:  "2001:0db8:85a3::/124",
				existingips: []string{"2001:db8:85a3::1"},
			},
			want: &IP{IP: netip.MustParseAddr("2001:db8:85a3::2"), ParentPrefix: "2001:0db8:85a3::/124"},
		},
		{
			name: "Want next IP, but network is full",
			fields: fields{
				prefixCIDR:  "192.168.3.0/30",
				existingips: []string{"192.168.3.1", "192.168.3.2"},
			},
			want: nil,
		},
		{
			name: "Want next IP, but network is full",
			fields: fields{
				prefixCIDR: "192.168.4.0/32",
			},
			want: nil,
		},
		{
			name: "Want next IPv6, but network is full",
			fields: fields{
				prefixCIDR:  "2001:0db8:85a3::/126",
				existingips: []string{"2001:db8:85a3::1", "2001:db8:85a3::2", "2001:db8:85a3::3"},
			},
			want: nil,
		},
		{
			name: "Want next IPv6, but network is full",
			fields: fields{
				prefixCIDR: "2001:0db8:85a3::/128",
			},
			want: nil,
		},
	}
	for _, tt := range tests {
		test := tt
		testWithBackends(t, func(t *testing.T, ipam *ipamer) {
			t.Logf("Running test: %s", test.name)

			ipam.namespace = test.fields.namespace
			p, err := ipam.NewPrefix(ctx, test.fields.prefixCIDR)
			if err != nil {
				t.Errorf("Could not create prefix: %v", err)
			}
			//t.Logf("Prefix:%#v", p)
			for _, ipString := range test.fields.existingips {
				p.ips[ipString] = true
			}

			var updatedPrefix Prefix
			updatedPrefix, err = ipam.storage.UpdatePrefix(ctx, *p, p.Namespace)
			if err != nil {
				t.Errorf("Could not update prefix: %v", err)
			}
			got, err := ipam.AcquireIP(ctx, updatedPrefix.Cidr)
			//t.Logf("acquireip got:%#v", got)
			if err != nil {
				t.Logf("error is: %s", err.Error())
			}
			if test.want == nil || got == nil {
				if !reflect.DeepEqual(got, test.want) {
					t.Errorf("Ipamer.AcquireIP() want or got is nil, got %v, want %v", got, test.want)
				}
			} else {
				if test.want.IP.Compare(got.IP) != 0 {
					t.Errorf("Ipamer.AcquireIP() got %v, want %v", got, test.want)
				}
			}
		})
	}
}

func TestIpamer_ReleaseIPFromPrefixIPv4(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix, err := ipam.NewPrefix(ctx, "192.168.0.0/24")
		require.Nil(t, err)
		require.NotNil(t, prefix)

		err = ipam.ReleaseIPFromPrefix(ctx, prefix.Cidr, "1.2.3.4")
		require.NotNil(t, err)
		//require.True(t, errors.As(err, &NotFoundError{}), "error must be of correct type")
		require.True(t, errors.Is(err, ErrNotFound), "error must be NotFound")
		require.Equal(t, "NotFound: unable to release ip:1.2.3.4 because it is not allocated in prefix:192.168.0.0/24", err.Error())

		err = ipam.ReleaseIPFromPrefix(ctx, "4.5.6.7/23", "1.2.3.4")
		require.NotNil(t, err)
		//require.True(t, errors.As(err, &NotFoundError{}), "error must be of correct type")
		require.True(t, errors.Is(err, ErrNotFound), "error must be NotFound")
		require.Equal(t, "NotFound: unable to find prefix for cidr:4.5.6.7/23", err.Error())
	})
}

func TestIpamer_ReleaseIPFromPrefixIPv6(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix, err := ipam.NewPrefix(ctx, "2001:0db8:85a3::/120")
		require.Nil(t, err)
		require.NotNil(t, prefix)

		err = ipam.ReleaseIPFromPrefix(ctx, prefix.Cidr, "1.2.3.4")
		require.NotNil(t, err)
		//require.True(t, errors.As(err, &NotFoundError{}), "error must be of correct type")
		require.True(t, errors.Is(err, ErrNotFound), "error must be NotFound")
		require.Equal(t, "NotFound: unable to release ip:1.2.3.4 because it is not allocated in prefix:2001:db8:85a3::/120", err.Error())

		err = ipam.ReleaseIPFromPrefix(ctx, prefix.Cidr, "1001:0db8:85a3::1")
		require.NotNil(t, err)
		//require.True(t, errors.As(err, &NotFoundError{}), "error must be of correct type")
		require.True(t, errors.Is(err, ErrNotFound), "error must be NotFound")
		require.Equal(t, "NotFound: unable to release ip:1001:0db8:85a3::1 because it is not allocated in prefix:2001:db8:85a3::/120", err.Error())

		err = ipam.ReleaseIPFromPrefix(ctx, "1001:0db8:85a3::/120", "1.2.3.4")
		require.NotNil(t, err)
		//require.True(t, errors.As(err, &NotFoundError{}), "error must be of correct type")
		require.True(t, errors.Is(err, ErrNotFound), "error must be NotFound")
		require.Equal(t, "NotFound: unable to find prefix for cidr:1001:0db8:85a3::/120", err.Error())
	})
}

func TestIpamer_AcquireSpecificIP(t *testing.T) {
	ctx := context.Background()
	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		// IPv4
		prefix, err := ipam.NewPrefix(ctx, "192.168.99.0/24")
		require.Nil(t, err)
		require.Equal(t, prefix.availableips(), uint64(256))
		// network an broadcast are blocked
		require.Equal(t, prefix.acquiredips(), uint64(2))
		ip1, err := ipam.AcquireSpecificIP(ctx, prefix.Cidr, "192.168.99.1")
		require.Nil(t, err)
		require.NotNil(t, ip1)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(3))
		ip2, err := ipam.AcquireSpecificIP(ctx, prefix.Cidr, "192.168.99.2")
		require.Nil(t, err)
		require.NotEqual(t, ip1, ip2)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(4))
		require.Equal(t, "192.168.99.1", ip1.IP.String())
		require.Equal(t, "192.168.99.2", ip2.IP.String())

		// Wish IP out of prefix
		ip3, err := ipam.AcquireSpecificIP(ctx, prefix.Cidr, "192.168.98.2")
		require.Nil(t, ip3)
		require.Error(t, err)
		require.Equal(t, "given ip:192.168.98.2 is not in 192.168.99.0/24", err.Error())

		// Wish IP is invalid
		ip4, err := ipam.AcquireSpecificIP(ctx, prefix.Cidr, "192.168.99.1.invalid")
		require.Nil(t, ip4)
		require.Error(t, err)
		require.Equal(t, "given ip:192.168.99.1.invalid in not valid", err.Error())

		// Cidr is invalid
		ip5, err := ipam.AcquireSpecificIP(ctx, "3.4.5.6/27", "192.168.99.1.invalid")
		require.Nil(t, ip5)
		require.Error(t, err)
		require.ErrorIs(t, err, ErrNotFound)
		require.Equal(t, "NotFound: unable to find prefix for cidr:3.4.5.6/27", err.Error())

		prefix, err = ipam.ReleaseIP(ctx, ip1)
		require.Nil(t, err)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(3))

		prefix, err = ipam.ReleaseIP(ctx, ip2)
		require.Nil(t, err)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(2))

		// IPv6
		prefix, err = ipam.NewPrefix(ctx, "2001:0db8:85a3::/120")
		require.Nil(t, err)
		require.Equal(t, prefix.availableips(), uint64(256))
		// network is blocked
		require.Equal(t, prefix.acquiredips(), uint64(1))
		ip1, err = ipam.AcquireSpecificIP(ctx, prefix.Cidr, "2001:db8:85a3::1")
		require.Nil(t, err)
		require.NotNil(t, ip1)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(2))
		ip2, err = ipam.AcquireSpecificIP(ctx, prefix.Cidr, "2001:0db8:85a3::2")
		require.Nil(t, err)
		require.NotEqual(t, ip1, ip2)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(3))
		require.Equal(t, "2001:db8:85a3::1", ip1.IP.String())
		require.Equal(t, "2001:db8:85a3::2", ip2.IP.String())

		// Wish IP out of prefix
		ip3, err = ipam.AcquireSpecificIP(ctx, prefix.Cidr, "2001:0db8:85a4::1")
		require.Nil(t, ip3)
		require.Error(t, err)
		require.Equal(t, "given ip:2001:0db8:85a4::1 is not in 2001:db8:85a3::/120", err.Error())

		// Cidr is invalid
		ip5, err = ipam.AcquireSpecificIP(ctx, "2001:0db8:95a3::/120", "2001:0db8:95a3::invalid")
		require.Nil(t, ip5)
		require.Error(t, err)
		require.ErrorIs(t, err, ErrNotFound)
		require.Equal(t, "NotFound: unable to find prefix for cidr:2001:0db8:95a3::/120", err.Error())

		prefix, err = ipam.ReleaseIP(ctx, ip1)
		require.Nil(t, err)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(2))

		prefix, err = ipam.ReleaseIP(ctx, ip2)
		require.Nil(t, err)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(1))
	})
}

func TestIpamer_AcquireIPCountsIPv4(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix, err := ipam.NewPrefix(ctx, "192.168.0.0/24")
		require.Nil(t, err)
		require.Equal(t, prefix.availableips(), uint64(256))
		// network and broadcast are blocked
		require.Equal(t, prefix.acquiredips(), uint64(2))
		ip1, err := ipam.AcquireIP(ctx, prefix.Cidr)
		require.Nil(t, err)
		require.NotNil(t, ip1)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(3))
		ip2, err := ipam.AcquireIP(ctx, prefix.Cidr)
		require.Nil(t, err)
		require.NotEqual(t, ip1, ip2)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(4))
		require.True(t, strings.HasPrefix(ip1.IP.String(), "192.168.0"))
		require.True(t, strings.HasPrefix(ip2.IP.String(), "192.168.0"))

		prefix, err = ipam.ReleaseIP(ctx, ip1)
		require.Nil(t, err)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(3))

		prefix, err = ipam.ReleaseIP(ctx, ip2)
		require.Nil(t, err)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(2))
	})
}

func TestIpamer_AcquireIPCountsIPv6(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix, err := ipam.NewPrefix(ctx, "2001:0db8:85a3::/120")
		require.Nil(t, err)
		require.Equal(t, prefix.availableips(), uint64(256))
		// network is blocked
		require.Equal(t, prefix.acquiredips(), uint64(1))
		ip1, err := ipam.AcquireIP(ctx, prefix.Cidr)
		require.Nil(t, err)
		require.NotNil(t, ip1)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(2))
		ip2, err := ipam.AcquireIP(ctx, prefix.Cidr)
		require.Nil(t, err)
		require.NotEqual(t, ip1, ip2)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(3))
		require.True(t, strings.HasPrefix(ip1.IP.String(), "2001:db8:85a3::"))
		require.True(t, strings.HasPrefix(ip2.IP.String(), "2001:db8:85a3::"))

		prefix, err = ipam.ReleaseIP(ctx, ip1)
		require.Nil(t, err)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(2))

		prefix, err = ipam.ReleaseIP(ctx, ip2)
		require.Nil(t, err)
		require.Equal(t, prefix.availableips(), uint64(256))
		require.Equal(t, prefix.acquiredips(), uint64(1))
	})
}

func TestIpamer_AcquireChildPrefixFragmented(t *testing.T) {
	ctx := context.Background()
	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		allPrefixes, err := ipam.storage.ReadAllPrefixes(ctx, ipam.namespace)
		require.NoError(t, err)
		require.Equal(t, 0, len(allPrefixes))

		// Create Prefix with /20
		prefix, err := ipam.NewPrefix(ctx, "192.168.0.0/20")
		require.NoError(t, err)
		s, _ := prefix.availablePrefixes()
		require.Equal(t, 1024, int(s))
		require.Equal(t, 0, int(prefix.acquiredPrefixes()))
		require.Equal(t, 0, int(prefix.Usage().AcquiredPrefixes))

		// Acquire first half 192.168.0.0/21 = 192.168.0.0 - 192.168.7.254
		c1, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 21)
		require.NoError(t, err)
		require.NotNil(t, c1)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		s, _ = prefix.availablePrefixes()
		require.Equal(t, 512, int(s))
		require.Equal(t, 1, int(prefix.acquiredPrefixes()))
		require.Equal(t, 1, int(prefix.Usage().AcquiredPrefixes))

		// acquire 1/4the of the rest 192.168.8.0/22 = 192.168.8.0 - 192.168.11.254
		c2, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 22)
		require.NoError(t, err)
		require.NotNil(t, c2)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		s, a := prefix.availablePrefixes()
		// Next free must be 192.168.12.0/22
		require.Equal(t, []string{"192.168.12.0/22"}, a)
		require.Equal(t, 256, int(s))
		require.Equal(t, 2, int(prefix.acquiredPrefixes()))
		require.Equal(t, 2, int(prefix.Usage().AcquiredPrefixes))

		// acquire impossible size
		_, err = ipam.AcquireChildPrefix(ctx, prefix.Cidr, 21)
		require.EqualError(t, err, "no prefix found in 192.168.0.0/20 with length:21, but 192.168.12.0/22 is available")

		// Release small, first half acquired
		err = ipam.ReleaseChildPrefix(ctx, c2)
		require.NoError(t, err)

		// acquire /28
		c3, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 28)
		require.NoError(t, err)
		require.NotNil(t, c3)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		s, a = prefix.availablePrefixes()
		require.Equal(t, []string{"192.168.8.16/28", "192.168.8.32/27", "192.168.8.64/26", "192.168.8.128/25", "192.168.9.0/24", "192.168.10.0/23", "192.168.12.0/22"}, a)
		require.Equal(t, 508, int(s))
		require.Equal(t, 2, int(prefix.acquiredPrefixes()))
		require.Equal(t, 2, int(prefix.Usage().AcquiredPrefixes))

		// acquire impossible size
		_, err = ipam.AcquireChildPrefix(ctx, prefix.Cidr, 21)
		require.EqualError(t, err, "no prefix found in 192.168.0.0/20 with length:21, but 192.168.8.16/28,192.168.8.32/27,192.168.8.64/26,192.168.8.128/25,192.168.9.0/24,192.168.10.0/23,192.168.12.0/22 are available")

		// acquire a /22 which must be possible
		c4, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 22)
		require.NoError(t, err)
		require.NotNil(t, c4)
		require.Equal(t, c4.String(), "192.168.12.0/22")

	})
}

func TestIpamer_AcquireChildPrefixCounts(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		allPrefixes, err := ipam.storage.ReadAllPrefixes(ctx, ipam.namespace)
		require.Nil(t, err)
		require.Equal(t, 0, len(allPrefixes))

		prefix, err := ipam.NewPrefix(ctx, "192.168.0.0/20")
		require.Nil(t, err)
		s, _ := prefix.availablePrefixes()
		require.Equal(t, s, uint64(1024))
		require.Equal(t, prefix.acquiredPrefixes(), uint64(0))
		require.Equal(t, prefix.Usage().AcquiredPrefixes, uint64(0))

		usage := prefix.Usage()
		require.Equal(t, "ip:2/4096", usage.String())

		allPrefixes, err = ipam.storage.ReadAllPrefixes(ctx, ipam.namespace)
		require.Nil(t, err)
		require.Equal(t, 1, len(allPrefixes))

		c1, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 22)
		require.Nil(t, err)
		require.NotNil(t, c1)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		s, _ = prefix.availablePrefixes()
		require.Equal(t, uint64(768), s)
		require.Equal(t, uint64(1), prefix.acquiredPrefixes())
		require.Equal(t, uint64(1), prefix.Usage().AcquiredPrefixes)

		usage = prefix.Usage()
		require.Equal(t, "ip:2/4096 prefixes alloc:1 avail:768", usage.String())

		allPrefixes, err = ipam.storage.ReadAllPrefixes(ctx, ipam.namespace)
		require.Nil(t, err)
		require.Equal(t, 2, len(allPrefixes))

		c2, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 22)
		require.Nil(t, err)
		require.NotNil(t, c2)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		s, _ = prefix.availablePrefixes()
		require.Equal(t, uint64(512), s)
		require.Equal(t, prefix.acquiredPrefixes(), uint64(2))
		require.Equal(t, prefix.Usage().AcquiredPrefixes, uint64(2))
		require.True(t, strings.HasSuffix(c1.Cidr, "/22"))
		require.True(t, strings.HasSuffix(c2.Cidr, "/22"))
		require.True(t, strings.HasPrefix(c1.Cidr, "192.168."))
		require.True(t, strings.HasPrefix(c2.Cidr, "192.168."))
		allPrefixes, err = ipam.storage.ReadAllPrefixes(ctx, ipam.namespace)
		require.Nil(t, err)
		require.Equal(t, 3, len(allPrefixes))

		err = ipam.ReleaseChildPrefix(ctx, c1)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)

		require.NoError(t, err)
		s, _ = prefix.availablePrefixes()
		require.Equal(t, uint64(768), s)
		require.Equal(t, uint64(1), prefix.acquiredPrefixes())
		allPrefixes, err = ipam.storage.ReadAllPrefixes(ctx, ipam.namespace)
		require.Nil(t, err)
		require.Equal(t, 2, len(allPrefixes))

		err = ipam.ReleaseChildPrefix(ctx, c2)
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)

		require.NoError(t, err)
		s, _ = prefix.availablePrefixes()
		require.Equal(t, uint64(1024), s)
		require.Equal(t, uint64(0), prefix.acquiredPrefixes())
		require.Equal(t, prefix.Usage().AcquiredPrefixes, uint64(0))
		allPrefixes, err = ipam.storage.ReadAllPrefixes(ctx, ipam.namespace)
		require.Nil(t, err)
		require.Equal(t, 1, len(allPrefixes))

		err = ipam.ReleaseChildPrefix(ctx, c1)
		require.Errorf(t, err, "unable to release prefix %s:delete prefix:%s not found", c1.Cidr)

		c3, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 22)
		require.Nil(t, err)
		require.NotNil(t, c2)
		ip1, err := ipam.AcquireIP(ctx, c3.Cidr)
		require.Nil(t, err)
		require.NotNil(t, ip1)

		err = ipam.ReleaseChildPrefix(ctx, c3)
		require.Errorf(t, err, "prefix %s has ips, deletion not possible", c3.Cidr)

		c3, err = ipam.ReleaseIP(ctx, ip1)
		require.Nil(t, err)

		err = ipam.ReleaseChildPrefix(ctx, c3)
		require.Nil(t, err)

		allPrefixes, err = ipam.storage.ReadAllPrefixes(ctx, ipam.namespace)
		require.Nil(t, err)
		require.Equal(t, 1, len(allPrefixes))
	})
}

// unittest to verify the count of subnets in a prefix
func TestIpamer_SubnetcountTest(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		// Creating IPBlock
		prefix, err := ipam.NewPrefix(ctx, "192.168.0.0/20")
		require.Nil(t, err)
		require.Equal(t, 0, int(ipam.GetSubnetCount(ctx, prefix.Cidr)))

		// Creating Allocation 1
		childPrefix1, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 21)
		require.Nil(t, err)

		require.Equal(t, 0, int(ipam.GetSubnetCount(ctx, prefix.Cidr)))
		require.Equal(t, 0, int(ipam.GetSubnetCount(ctx, childPrefix1.Cidr)))

		// Creating Allocation 2
		childPrefix2, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 21)
		require.Nil(t, err)
		require.Equal(t, 0, int(ipam.GetSubnetCount(ctx, prefix.Cidr)))
		require.Equal(t, 0, int(ipam.GetSubnetCount(ctx, childPrefix2.Cidr)))

		// Creating Subnets for Allocation 1
		childPrefix1_subnet, err := ipam.AcquireChildPrefix(ctx, childPrefix1.Cidr, 24)
		require.Nil(t, err)
		require.Equal(t, 1, int(ipam.GetSubnetCount(ctx, prefix.Cidr)))
		require.Equal(t, 1, int(ipam.GetSubnetCount(ctx, childPrefix1.Cidr)))
		require.Equal(t, 0, int(ipam.GetSubnetCount(ctx, childPrefix2.Cidr)))
		require.Equal(t, 1, int(ipam.GetSubnetCount(ctx, childPrefix1_subnet.Cidr)))

		_, err = ipam.AcquireChildPrefix(ctx, childPrefix1.Cidr, 24)
		require.Nil(t, err)
		require.Equal(t, 2, int(ipam.GetSubnetCount(ctx, prefix.Cidr)))
		require.Equal(t, 2, int(ipam.GetSubnetCount(ctx, childPrefix1.Cidr)))
		require.Equal(t, 0, int(ipam.GetSubnetCount(ctx, childPrefix2.Cidr)))

		_, err = ipam.AcquireChildPrefix(ctx, childPrefix1.Cidr, 24)
		require.Nil(t, err)
		require.Equal(t, 3, int(ipam.GetSubnetCount(ctx, prefix.Cidr)))
		require.Equal(t, 3, int(ipam.GetSubnetCount(ctx, childPrefix1.Cidr)))
		require.Equal(t, 0, int(ipam.GetSubnetCount(ctx, childPrefix2.Cidr)))

		_, err = ipam.AcquireChildPrefix(ctx, childPrefix2.Cidr, 24)
		require.Nil(t, err)
		require.Equal(t, 4, int(ipam.GetSubnetCount(ctx, prefix.Cidr)))
		require.Equal(t, 3, int(ipam.GetSubnetCount(ctx, childPrefix1.Cidr)))
		require.Equal(t, 1, int(ipam.GetSubnetCount(ctx, childPrefix2.Cidr)))

		_, err = ipam.AcquireChildPrefix(ctx, childPrefix2.Cidr, 24)
		require.Nil(t, err)
		require.Equal(t, 5, int(ipam.GetSubnetCount(ctx, prefix.Cidr)))
		require.Equal(t, 3, int(ipam.GetSubnetCount(ctx, childPrefix1.Cidr)))
		require.Equal(t, 2, int(ipam.GetSubnetCount(ctx, childPrefix2.Cidr)))

	})
}

func TestIpamer_AcquireChildPrefixIPv4(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix, err := ipam.NewPrefix(ctx, "192.168.0.0/20")
		require.Nil(t, err)
		s, _ := prefix.availablePrefixes()
		require.Equal(t, uint64(1024), s)
		require.Equal(t, prefix.acquiredPrefixes(), uint64(0))

		// Same length
		cp, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 20)
		require.NotNil(t, err)
		require.Equal(t, "given length:20 must be greater than prefix length:20", err.Error())
		require.Nil(t, cp)

		// working length
		cp, err = ipam.AcquireChildPrefix(ctx, prefix.Cidr, 21)
		require.Nil(t, err)
		require.NotNil(t, cp)
		require.True(t, strings.HasPrefix(cp.Cidr, "192.168."))
		require.True(t, strings.HasSuffix(cp.Cidr, "/21"))
		require.Equal(t, prefix.Cidr, cp.ParentCidr)

		// No more ChildPrefixes
		cp, err = ipam.AcquireChildPrefix(ctx, prefix.Cidr, 21)
		require.Nil(t, err)
		require.NotNil(t, cp)
		cp, err = ipam.AcquireChildPrefix(ctx, prefix.Cidr, 21)
		require.NotNil(t, err)
		require.Equal(t, "no prefix found in 192.168.0.0/20 with length:21", err.Error())
		require.Nil(t, cp)

		// Prefix has ips
		p2, err := ipam.NewPrefix(ctx, "10.0.0.0/24")
		require.Nil(t, err)
		s, _ = p2.availablePrefixes()
		require.Equal(t, uint64(64), s)
		require.Equal(t, p2.acquiredPrefixes(), uint64(0))
		ip, err := ipam.AcquireIP(ctx, p2.Cidr)
		require.Nil(t, err)
		require.NotNil(t, ip)
		cp2, err := ipam.AcquireChildPrefix(ctx, p2.Cidr, 25)
		require.NotNil(t, err)
		require.Equal(t, "prefix 10.0.0.0/24 has ips, acquire child prefix not possible", err.Error())
		require.Nil(t, cp2)

		// Prefix has Childs, AcquireIP wont work
		p3, err := ipam.NewPrefix(ctx, "172.17.0.0/24")
		require.Nil(t, err)
		s, _ = p3.availablePrefixes()
		require.Equal(t, uint64(64), s)
		require.Equal(t, p3.acquiredPrefixes(), uint64(0))
		cp3, err := ipam.AcquireChildPrefix(ctx, p3.Cidr, 25)
		require.Nil(t, err)
		require.NotNil(t, cp3)
		p3 = ipam.PrefixFrom(ctx, p3.Cidr)
		ip, err = ipam.AcquireIP(ctx, p3.Cidr)
		require.NotNil(t, err)
		require.Equal(t, "prefix 172.17.0.0/24 has childprefixes, acquire ip not possible", err.Error())
		require.Nil(t, ip)

		// Release Parent Prefix must not work
		err = ipam.ReleaseChildPrefix(ctx, p3)
		require.NotNil(t, err)
		require.Equal(t, "prefix 172.17.0.0/24 is no child prefix", err.Error())
	})
}

func TestIpamer_AcquireChildPrefixIPv6(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix, err := ipam.NewPrefix(ctx, "2001:0db8:85a3::/116")
		require.Nil(t, err)
		s, _ := prefix.availablePrefixes()
		require.Equal(t, uint64(1024), s)
		require.Equal(t, prefix.acquiredPrefixes(), uint64(0))

		// Same length
		cp, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 116)
		require.NotNil(t, err)
		require.Equal(t, "given length:116 must be greater than prefix length:116", err.Error())
		require.Nil(t, cp)

		// working length
		cp, err = ipam.AcquireChildPrefix(ctx, prefix.Cidr, 117)
		require.Nil(t, err)
		require.NotNil(t, cp)
		require.True(t, strings.HasPrefix(cp.Cidr, "2001:db8:85a3:"))
		require.True(t, strings.HasSuffix(cp.Cidr, "/117"))
		require.Equal(t, prefix.Cidr, cp.ParentCidr)

		// No more ChildPrefixes
		cp, err = ipam.AcquireChildPrefix(ctx, prefix.Cidr, 117)
		require.Nil(t, err)
		require.NotNil(t, cp)
		cp, err = ipam.AcquireChildPrefix(ctx, prefix.Cidr, 117)
		require.NotNil(t, err)
		require.Equal(t, "no prefix found in 2001:db8:85a3::/116 with length:117", err.Error())
		require.Nil(t, cp)

		// Prefix has ips
		p2, err := ipam.NewPrefix(ctx, "2001:0db8:95a3::/120")
		require.Nil(t, err)
		s, _ = p2.availablePrefixes()
		require.Equal(t, uint64(64), s)
		require.Equal(t, p2.acquiredPrefixes(), uint64(0))
		ip, err := ipam.AcquireIP(ctx, p2.Cidr)
		require.Nil(t, err)
		require.NotNil(t, ip)
		cp2, err := ipam.AcquireChildPrefix(ctx, p2.Cidr, 121)
		require.NotNil(t, err)
		require.Equal(t, "prefix 2001:db8:95a3::/120 has ips, acquire child prefix not possible", err.Error())
		require.Nil(t, cp2)

		// Prefix has Childs, AcquireIP wont work
		p3, err := ipam.NewPrefix(ctx, "2001:0db8:75a3::/120")
		require.Nil(t, err)
		s, _ = p3.availablePrefixes()
		require.Equal(t, uint64(64), s)
		require.Equal(t, p3.acquiredPrefixes(), uint64(0))
		cp3, err := ipam.AcquireChildPrefix(ctx, p3.Cidr, 121)
		require.Nil(t, err)
		require.NotNil(t, cp3)
		p3 = ipam.PrefixFrom(ctx, p3.Cidr)
		ip, err = ipam.AcquireIP(ctx, p3.Cidr)
		require.NotNil(t, err)
		require.Equal(t, "prefix 2001:db8:75a3::/120 has childprefixes, acquire ip not possible", err.Error())
		require.Nil(t, ip)

		// Release Parent Prefix must not work
		err = ipam.ReleaseChildPrefix(ctx, p3)
		require.NotNil(t, err)
		require.Equal(t, "prefix 2001:db8:75a3::/120 is no child prefix", err.Error())
	})
}

func TestIpamer_AcquireSpecificChildPrefixIPv4(t *testing.T) {
	ctx := context.Background()
	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix, err := ipam.NewPrefix(ctx, "192.168.0.0/20")
		require.Nil(t, err)
		s, _ := prefix.availablePrefixes()
		require.Equal(t, uint64(1024), s)
		require.Equal(t, prefix.acquiredPrefixes(), uint64(0))

		// Same length
		cp, err := ipam.AcquireSpecificChildPrefix(ctx, prefix.Cidr, "192.168.0.0/20")
		require.NotNil(t, err)
		require.Equal(t, "given length:20 must be greater than prefix length:20", err.Error())
		require.Nil(t, cp)

		// working length
		cp, err = ipam.AcquireSpecificChildPrefix(ctx, prefix.Cidr, "192.168.0.0/21")
		require.Nil(t, err)
		require.NotNil(t, cp)
		require.Equal(t, cp.Cidr, "192.168.0.0/21")
		require.Equal(t, prefix.Cidr, cp.ParentCidr)

		// specific prefix not available
		cp, err = ipam.AcquireSpecificChildPrefix(ctx, prefix.Cidr, "192.168.8.0/21")
		require.Nil(t, err)
		require.NotNil(t, cp)
		cp, err = ipam.AcquireSpecificChildPrefix(ctx, prefix.Cidr, "192.168.8.0/21")
		require.NotNil(t, err)
		require.Equal(t, "specific prefix 192.168.8.0/21 is not available in prefix 192.168.0.0/20", err.Error())
		require.Nil(t, cp)

		// Prefix has ips
		p2, err := ipam.NewPrefix(ctx, "10.0.0.0/24")
		require.Nil(t, err)
		s, _ = p2.availablePrefixes()
		require.Equal(t, uint64(64), s)
		require.Equal(t, p2.acquiredPrefixes(), uint64(0))
		ip, err := ipam.AcquireIP(ctx, p2.Cidr)
		require.Nil(t, err)
		require.NotNil(t, ip)
		cp2, err := ipam.AcquireSpecificChildPrefix(ctx, p2.Cidr, "10.0.0.0/25")
		require.NotNil(t, err)
		require.Equal(t, "prefix 10.0.0.0/24 has ips, acquire child prefix not possible", err.Error())
		require.Nil(t, cp2)

		// Prefix has Childs, AcquireIP wont work
		p3, err := ipam.NewPrefix(ctx, "172.17.0.0/24")
		require.Nil(t, err)
		s, _ = p3.availablePrefixes()
		require.Equal(t, uint64(64), s)
		require.Equal(t, p3.acquiredPrefixes(), uint64(0))
		cp3, err := ipam.AcquireSpecificChildPrefix(ctx, p3.Cidr, "172.17.0.0/25")
		require.Nil(t, err)
		require.NotNil(t, cp3)
		p3 = ipam.PrefixFrom(ctx, p3.Cidr)
		ip, err = ipam.AcquireIP(ctx, p3.Cidr)
		require.NotNil(t, err)
		require.Equal(t, "prefix 172.17.0.0/24 has childprefixes, acquire ip not possible", err.Error())
		require.Nil(t, ip)
	})
}

func TestIpamer_AcquireSpecificChildPrefixIPv6(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix, err := ipam.NewPrefix(ctx, "2001:0db8:85a3::/116")
		require.Nil(t, err)
		s, _ := prefix.availablePrefixes()
		require.Equal(t, uint64(1024), s)
		require.Equal(t, prefix.acquiredPrefixes(), uint64(0))

		// Same length
		cp, err := ipam.AcquireSpecificChildPrefix(ctx, prefix.Cidr, "2001:0db8:85a3::/116")
		require.NotNil(t, err)
		require.Equal(t, "given length:116 must be greater than prefix length:116", err.Error())
		require.Nil(t, cp)

		// working length
		cp, err = ipam.AcquireSpecificChildPrefix(ctx, prefix.Cidr, "2001:0db8:85a3::/117")
		require.Nil(t, err)
		require.NotNil(t, cp)
		require.Equal(t, "2001:db8:85a3::/117", cp.Cidr)
		require.Equal(t, prefix.Cidr, cp.ParentCidr)

		// specific prefix not available
		cp, err = ipam.AcquireSpecificChildPrefix(ctx, prefix.Cidr, "2001:0db8:85a3::0800/117")
		require.Nil(t, err)
		require.NotNil(t, cp)
		require.Equal(t, cp.Cidr, "2001:db8:85a3::800/117")
		cp, err = ipam.AcquireSpecificChildPrefix(ctx, prefix.Cidr, "2001:0db8:85a3::0800/117")
		require.NotNil(t, err)
		require.Equal(t, "specific prefix 2001:0db8:85a3::0800/117 is not available in prefix 2001:db8:85a3::/116", err.Error())
		require.Nil(t, cp)

		// Prefix has ips
		p2, err := ipam.NewPrefix(ctx, "2001:0db8:95a3::/120")
		require.Nil(t, err)
		s, _ = p2.availablePrefixes()
		require.Equal(t, uint64(64), s)
		require.Equal(t, p2.acquiredPrefixes(), uint64(0))
		ip, err := ipam.AcquireIP(ctx, p2.Cidr)
		require.Nil(t, err)
		require.NotNil(t, ip)
		cp2, err := ipam.AcquireSpecificChildPrefix(ctx, p2.Cidr, "2001:0db8:95a3::/121")
		require.NotNil(t, err)
		require.Equal(t, "prefix 2001:db8:95a3::/120 has ips, acquire child prefix not possible", err.Error())
		require.Nil(t, cp2)

		// Prefix has Childs, AcquireIP wont work
		p3, err := ipam.NewPrefix(ctx, "2001:0db8:75a3::/120")
		require.Nil(t, err)
		s, _ = p3.availablePrefixes()
		require.Equal(t, uint64(64), s)
		require.Equal(t, p3.acquiredPrefixes(), uint64(0))
		cp3, err := ipam.AcquireSpecificChildPrefix(ctx, p3.Cidr, "2001:0db8:75a3::/121")
		require.Nil(t, err)
		require.NotNil(t, cp3)
		p3 = ipam.PrefixFrom(ctx, p3.Cidr)
		ip, err = ipam.AcquireIP(ctx, p3.Cidr)
		require.NotNil(t, err)
		require.Equal(t, "prefix 2001:db8:75a3::/120 has childprefixes, acquire ip not possible", err.Error())
		require.Nil(t, ip)

		// Release Parent Prefix must not work
		err = ipam.ReleaseChildPrefix(ctx, p3)
		require.NotNil(t, err)
		require.Equal(t, "prefix 2001:db8:75a3::/120 is no child prefix", err.Error())
	})
}

func TestIpamer_AcquireChildPrefixNoDuplicatesUntilFullIPv6(t *testing.T) {
	ctx := context.Background()
	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix, err := ipam.NewPrefix(ctx, "2001:0db8:85a3::/112")
		require.Nil(t, err)
		s, _ := prefix.availablePrefixes()
		require.Equal(t, uint64(16384), s)
		require.Equal(t, uint64(0), prefix.acquiredPrefixes())

		uniquePrefixes := make(map[string]bool)
		// acquire all /120 prefixes (2^8 = 256)
		for i := 0; i < 256; i++ {
			cp, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 120)
			require.Nil(t, err)
			require.NotNil(t, cp)
			require.True(t, strings.HasPrefix(cp.Cidr, "2001:db8:85a3:"))
			require.True(t, strings.HasSuffix(cp.Cidr, "/120"))
			require.Equal(t, prefix.Cidr, cp.ParentCidr)
			_, ok := uniquePrefixes[cp.String()]
			require.False(t, ok)
			uniquePrefixes[cp.String()] = true
		}
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		require.Equal(t, 256, len(uniquePrefixes))
		s, _ = prefix.availablePrefixes()
		require.Equal(t, uint64(0), s)
		require.Equal(t, prefix.acquiredPrefixes(), uint64(256))

	})
}
func TestIpamer_AcquireChildPrefixNoDuplicatesUntilFullIPv4(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix, err := ipam.NewPrefix(ctx, "192.168.0.0/16")
		require.Nil(t, err)
		s, _ := prefix.availablePrefixes()
		require.Equal(t, uint64(16384), s)
		require.Equal(t, uint64(0), prefix.acquiredPrefixes())

		uniquePrefixes := make(map[string]bool)
		// acquire all /24 prefixes (2^8 = 256)
		for i := 0; i < 256; i++ {
			cp, err := ipam.AcquireChildPrefix(ctx, prefix.Cidr, 24)
			require.Nil(t, err)
			require.NotNil(t, cp)
			require.True(t, strings.HasPrefix(cp.Cidr, "192.168."))
			require.True(t, strings.HasSuffix(cp.Cidr, "/24"))
			require.Equal(t, prefix.Cidr, cp.ParentCidr)
			_, ok := uniquePrefixes[cp.String()]
			require.False(t, ok)
			uniquePrefixes[cp.String()] = true
		}
		prefix = ipam.PrefixFrom(ctx, prefix.Cidr)
		require.Equal(t, 256, len(uniquePrefixes))
		s, _ = prefix.availablePrefixes()
		require.Equal(t, uint64(0), s)
		require.Equal(t, prefix.acquiredPrefixes(), uint64(256))

	})
}

func TestPrefix_Availableips(t *testing.T) {

	tests := []struct {
		name string
		Cidr string
		want uint64
	}{
		{
			name: "large",
			Cidr: "192.168.0.0/20",
			want: 4096,
		},
		{
			name: "small",
			Cidr: "192.168.0.0/24",
			want: 256,
		},
		{
			name: "smaller",
			Cidr: "192.168.0.0/25",
			want: 128,
		},
		{
			name: "smaller",
			Cidr: "192.168.0.0/30",
			want: 4,
		},
		{
			name: "smaller IPv6",
			Cidr: "2001:0db8:85a3::/126",
			want: 4,
		},
		{
			name: "large IPv6",
			Cidr: "2001:0db8:85a3::/116",
			want: 4096,
		},
		{
			name: "IPv6 below reporting boundary",
			Cidr: "2001:db8::/98",
			want: 1073741824,
		},
		{
			name: "IPv6 at reporting boundary",
			Cidr: "2001:db8::/97",
			want: 2147483647,
		},
		{
			name: "IPv6 above reporting boundary",
			Cidr: "2001:db8::/96",
			want: 2147483647,
		},
	}
	for _, tt := range tests {
		test := tt
		t.Run(tt.name, func(t *testing.T) {
			p := &Prefix{
				Cidr: test.Cidr,
			}
			if got := p.availableips(); got != test.want {
				t.Errorf("Prefix.Availableips() = %v, want %v", got, test.want)
			}
		})
	}
}

func TestIpamer_PrefixesOverlapping(t *testing.T) {
	ctx := context.Background()

	tests := []struct {
		name             string
		existingPrefixes []string
		newPrefixes      []string
		wantErr          bool
		errorString      string
	}{
		{
			name:             "simple",
			existingPrefixes: []string{"192.168.0.0/24"},
			newPrefixes:      []string{"192.168.1.0/24"},
			wantErr:          false,
			errorString:      "",
		},
		{
			name:             "simple IPv6",
			existingPrefixes: []string{"2001:0db8:85a3::/126"},
			newPrefixes:      []string{"2001:0db8:85a4::/126"},
			wantErr:          false,
			errorString:      "",
		},
		{
			name:             "one overlap IPv6",
			existingPrefixes: []string{"2001:0db8:85a3::/126", "2001:0db8:85a4::/126"},
			newPrefixes:      []string{"2001:0db8:85a4::/126"},
			wantErr:          true,
			errorString:      "2001:db8:85a4::/126 overlaps 2001:db8:85a4::/126",
		},
		{
			name:             "one overlap",
			existingPrefixes: []string{"192.168.0.0/24", "192.168.1.0/24"},
			newPrefixes:      []string{"192.168.1.0/24"},
			wantErr:          true,
			errorString:      "192.168.1.0/24 overlaps 192.168.1.0/24",
		},
		{
			name:             "one overlap",
			existingPrefixes: []string{"192.168.0.0/24", "192.168.1.0/24"},
			newPrefixes:      []string{"192.168.0.0/23"},
			wantErr:          true,
			errorString:      "192.168.0.0/23 overlaps 192.168.0.0/24",
		},
		{
			name:             "one overlap",
			existingPrefixes: []string{"192.168.0.0/23", "192.168.2.0/23"},
			newPrefixes:      []string{"192.168.3.0/24"},
			wantErr:          true,
			errorString:      "192.168.3.0/24 overlaps 192.168.2.0/23",
		},
		{
			name:             "one overlap",
			existingPrefixes: []string{"192.168.128.0/25"},
			newPrefixes:      []string{"192.168.128.0/27"},
			wantErr:          true,
			errorString:      "192.168.128.0/27 overlaps 192.168.128.0/25",
		},
	}
	for _, tt := range tests {
		test := tt
		testWithBackends(t, func(t *testing.T, ipam *ipamer) {
			for _, ep := range test.existingPrefixes {
				p, err := ipam.NewPrefix(ctx, ep)
				if err != nil {
					t.Errorf("Newprefix on ExistingPrefix failed:%v", err)
				}
				if p == nil {
					t.Errorf("Newprefix on ExistingPrefix returns nil")
				}
			}
			err := PrefixesOverlapping(test.existingPrefixes, test.newPrefixes)
			if test.wantErr && err == nil {
				t.Errorf("Ipamer.PrefixesOverlapping() expected error but err was nil")
			}
			if test.wantErr && err != nil && err.Error() != test.errorString {
				t.Errorf("Ipamer.PrefixesOverlapping() error = %v, wantErr %v, errorString = %v", err, test.wantErr, test.errorString)
			}
			if !test.wantErr && err != nil {
				t.Errorf("Ipamer.PrefixesOverlapping() error = %v, wantErr %v", err, test.wantErr)
			}
		})
	}
}

func TestIpamer_NewPrefix(t *testing.T) {
	ctx := context.Background()

	tests := []struct {
		name        string
		cidr        string
		wantcidr    string
		wantErr     bool
		errorString string
	}{
		{
			name:     "valid Prefix",
			cidr:     "192.168.0.0/24",
			wantcidr: "192.168.0.0/24",
			wantErr:  false,
		},
		{
			name:     "valid Prefix, not in canocical form",
			cidr:     "192.169.0.1/24",
			wantcidr: "192.169.0.0/24",
			wantErr:  false,
		},
		{
			name:     "valid Prefix, not in canocical form",
			cidr:     "192.167.10.0/16",
			wantcidr: "192.167.0.0/16",
			wantErr:  false,
		},
		{
			name:        "invalid Prefix",
			cidr:        "192.168.0.0/33",
			wantErr:     true,
			errorString: "unable to parse cidr:192.168.0.0/33 netip.ParsePrefix(\"192.168.0.0/33\"): prefix length out of range",
		},
		{
			name:     "valid IPv6 Prefix",
			cidr:     "2001:0db8:85a3::/120",
			wantcidr: "2001:db8:85a3::/120",
			wantErr:  false,
		},
		{
			name:     "valid IPv6 Prefix, not in canocical form",
			cidr:     "2001:0db8:85a4::2/120",
			wantcidr: "2001:db8:85a4::/120",
			wantErr:  false,
		},
		{
			name:        "invalid IPv6 Prefix length",
			cidr:        "2001:0db8:85a3::/129",
			wantErr:     true,
			errorString: "unable to parse cidr:2001:0db8:85a3::/129 netip.ParsePrefix(\"2001:0db8:85a3::/129\"): prefix length out of range",
		},
		{
			name:        "invalid IPv6 Prefix length",
			cidr:        "2001:0db8:85a3:::/120",
			wantErr:     true,
			errorString: "unable to parse cidr:2001:0db8:85a3:::/120 netip.ParsePrefix(\"2001:0db8:85a3:::/120\"): ParseAddr(\"2001:0db8:85a3:::\"): each colon-separated field must have at least one digit (at \":\")",
		},
	}
	for _, tt := range tests {
		test := tt
		testWithBackends(t, func(t *testing.T, ipam *ipamer) {
			got, err := ipam.NewPrefix(ctx, test.cidr)
			if (err != nil) != test.wantErr {
				t.Errorf("Ipamer.NewPrefix(ctx, ) error = %v, wantErr %v", err, test.wantErr)
				return
			}
			/* this error fails
			if (err != nil) && test.errorString != err.Error() {
				t.Errorf("Ipamer.NewPrefix(ctx, ) error = %v, errorString %v", err, test.errorString)
				return
			}*/

			if err != nil {
				return
			}
			if got.Cidr != test.wantcidr {
				t.Errorf("Ipamer.NewPrefix(ctx, ) cidr = %v, want %v", got.Cidr, test.wantcidr)
			}
		})
	}
}

func TestIpamer_DeletePrefix(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		// IPv4
		prefix, err := ipam.NewPrefix(ctx, "192.168.0.0/20")
		require.Nil(t, err)
		s, _ := prefix.availablePrefixes()
		require.Equal(t, uint64(1024), s)
		require.Equal(t, prefix.acquiredPrefixes(), uint64(0))
		require.Equal(t, prefix.Usage().AcquiredPrefixes, uint64(0))

		ip, err := ipam.AcquireIP(ctx, prefix.Cidr)
		require.Nil(t, err)
		require.NotNil(t, ip)

		_, err = ipam.DeletePrefix(ctx, prefix.Cidr)
		require.NotNil(t, err)
		require.Equal(t, "prefix 192.168.0.0/20 has ips, delete prefix not possible", err.Error())

		// IPv6
		prefix, err = ipam.NewPrefix(ctx, "2001:0db8:85a3::/120")
		require.Nil(t, err)
		s, _ = prefix.availablePrefixes()
		require.Equal(t, uint64(64), s)
		require.Equal(t, prefix.acquiredPrefixes(), uint64(0))
		require.Equal(t, prefix.Usage().AcquiredPrefixes, uint64(0))

		ip, err = ipam.AcquireIP(ctx, prefix.Cidr)
		require.Nil(t, err)
		require.NotNil(t, ip)

		_, err = ipam.DeletePrefix(ctx, prefix.Cidr)
		require.NotNil(t, err)
		require.Equal(t, "prefix 2001:db8:85a3::/120 has ips, delete prefix not possible", err.Error())

		_, err = ipam.ReleaseIP(ctx, ip)
		require.Nil(t, err)
		_, err = ipam.DeletePrefix(ctx, prefix.Cidr)
		require.Nil(t, err)
	})
}

func TestIpamer_PrefixFrom(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix := ipam.PrefixFrom(ctx, "192.168.0.0/20")
		require.Nil(t, prefix)

		prefix, err := ipam.NewPrefix(ctx, "192.168.0.0/20")
		require.Nil(t, err)
		require.NotNil(t, prefix)

		prefix = ipam.PrefixFrom(ctx, "192.168.0.0/20")
		require.NotNil(t, prefix)

		// non canonical form still returns the same prefix
		prefix2 := ipam.PrefixFrom(ctx, "10.0.5.0/8")
		require.Nil(t, prefix2)

		prefix2a, err := ipam.NewPrefix(ctx, "10.8.0.0/8")
		require.Nil(t, err)
		require.NotNil(t, prefix2a)

		prefix2b := ipam.PrefixFrom(ctx, "10.2.0.0/8")
		require.NotNil(t, prefix2b)
		require.Equal(t, prefix2a.Cidr, prefix2b.Cidr)
	})
}

func TestIpamerAcquireIP(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		cidr := "10.0.0.0/16"
		p, err := ipam.NewPrefix(ctx, cidr)
		require.NoError(t, err)
		for n := 0; n < 10; n++ {
			if len(p.ips) != 2 {
				t.Fatalf("expected 2 ips in prefix, got %d", len(p.ips))
			}
			ip, err := ipam.AcquireIP(ctx, p.Cidr)
			require.NoError(t, err)
			require.NotNil(t, ip, "IP is nil")
			p, err = ipam.ReleaseIP(ctx, ip)
			require.NoError(t, err)
		}
		_, err = ipam.DeletePrefix(ctx, cidr)
		require.NoError(t, err, "error deleting prefix:%v", err)
	})
}

func TestIpamerAcquireIPv6(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		cidr := "2001:0db8:85a3::/120"
		p, err := ipam.NewPrefix(ctx, cidr)
		require.NoError(t, err)
		for n := 0; n < 10; n++ {
			if len(p.ips) != 1 {
				t.Fatalf("expected 1 ips in prefix, got %d", len(p.ips))
			}
			ip, err := ipam.AcquireIP(ctx, p.Cidr)
			require.NoError(t, err)
			require.NotNil(t, ip, "IP is nil")
			p, err = ipam.ReleaseIP(ctx, ip)
			require.NoError(t, err)
		}
		_, err = ipam.DeletePrefix(ctx, cidr)
		require.NoError(t, err, "error deleting prefix:%v", err)
	})
}
func TestIpamerAcquireAlreadyAquiredIPv4(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		cidr := "192.168.0.0/16"
		p, err := ipam.NewPrefix(ctx, cidr)
		require.NoError(t, err)
		ip, err := ipam.AcquireSpecificIP(ctx, p.Cidr, "192.168.2.4")
		require.NoError(t, err)
		require.NotNil(t, ip, "IP is nil")
		require.Equal(t, ip.IP.String(), "192.168.2.4")
		_, err = ipam.AcquireSpecificIP(ctx, p.Cidr, "192.168.2.4")
		require.ErrorIs(t, err, ErrAlreadyAllocated)
		require.EqualError(t, err, "AlreadyAllocatedError: given ip:192.168.2.4 is already allocated")

		_, err = ipam.ReleaseIP(ctx, ip)
		require.NoError(t, err)
		_, err = ipam.DeletePrefix(ctx, cidr)
		require.NoError(t, err)
	})
}
func TestIpamerAcquireAlreadyAquiredIPv6(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		cidr := "2001:0db8:85a3::/64"
		p, err := ipam.NewPrefix(ctx, cidr)
		require.NoError(t, err)
		ip, err := ipam.AcquireSpecificIP(ctx, p.Cidr, "2001:0db8:85a3::1")
		require.NoError(t, err)
		require.NotNil(t, ip, "IP is nil")
		require.Equal(t, ip.IP.String(), "2001:db8:85a3::1")
		_, err = ipam.AcquireSpecificIP(ctx, p.Cidr, "2001:0db8:85a3::1")
		require.ErrorIs(t, err, ErrAlreadyAllocated)
		require.EqualError(t, err, "AlreadyAllocatedError: given ip:2001:db8:85a3::1 is already allocated")

		_, err = ipam.ReleaseIP(ctx, ip)
		require.NoError(t, err)
		_, err = ipam.DeletePrefix(ctx, cidr)
		require.NoError(t, err)
	})
}
func TestGetHostAddresses(t *testing.T) {
	ctx := context.Background()
	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		cidr := "4.1.0.0/24"
		ips, err := ipam.getHostAddresses(ctx, cidr)
		require.NoError(t, err)
		require.NotNil(t, ips)
		require.Equal(t, 254, len(ips))

		ip, err := ipam.AcquireIP(ctx, cidr)
		require.Error(t, err)
		require.ErrorIs(t, err, ErrNoIPAvailable)
		require.Equal(t, "NoIPAvailableError: no more ips in prefix: 4.1.0.0/24 left, length of prefix.ips: 256", err.Error())
		require.Nil(t, ip)

		cidr = "3.1.0.0/26"
		ips, err = ipam.getHostAddresses(ctx, cidr)
		require.NoError(t, err)
		require.NotNil(t, ips)
		require.Equal(t, 62, len(ips))

		ip, err = ipam.AcquireIP(ctx, cidr)
		require.Error(t, err)
		require.ErrorIs(t, err, ErrNoIPAvailable)
		require.Equal(t, "NoIPAvailableError: no more ips in prefix: 3.1.0.0/26 left, length of prefix.ips: 64", err.Error())
		require.Nil(t, ip)
	})
}

func TestGetHostAddressesIPv6(t *testing.T) {
	ctx := context.Background()
	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		cidr := "2001:0db8:85a3::/120"
		ips, err := ipam.getHostAddresses(ctx, cidr)
		require.NoError(t, err)
		require.NotNil(t, ips)
		require.Equal(t, 255, len(ips))

		ip, err := ipam.AcquireIP(ctx, cidr)
		require.Error(t, err)
		require.ErrorIs(t, err, ErrNoIPAvailable)
		require.Equal(t, "NoIPAvailableError: no more ips in prefix: 2001:db8:85a3::/120 left, length of prefix.ips: 256", err.Error())
		require.Nil(t, ip)

		cidr = "2001:0db8:95a3::/122"
		ips, err = ipam.getHostAddresses(ctx, cidr)
		require.NoError(t, err)
		require.NotNil(t, ips)
		require.Equal(t, 63, len(ips))

		ip, err = ipam.AcquireIP(ctx, cidr)
		require.Error(t, err)
		require.ErrorIs(t, err, ErrNoIPAvailable)
		require.Equal(t, "NoIPAvailableError: no more ips in prefix: 2001:db8:95a3::/122 left, length of prefix.ips: 64", err.Error())
		require.Nil(t, ip)
	})
}
func TestPrefixDeepCopy(t *testing.T) {

	p1 := &Prefix{
		Cidr:                   "4.1.1.0/24",
		ParentCidr:             "4.1.0.0/16",
		availableChildPrefixes: map[string]bool{},
		isParent:               true,
		ips:                    map[string]bool{},
		version:                2,
	}

	p1.availableChildPrefixes["4.1.2.0/24"] = true
	p1.ips["4.1.1.1"] = true
	p1.ips["4.1.1.2"] = true

	p2 := p1.deepCopy()

	require.False(t, p1 == p2)
	require.Equal(t, p1, p2)
	require.False(t, &(p1.availableChildPrefixes) == &(p2.availableChildPrefixes))
	require.False(t, &(p1.ips) == &(p2.ips))
}

func TestGob(t *testing.T) {
	ctx := context.Background()
	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix, err := ipam.NewPrefix(ctx, "192.168.0.0/24")
		require.Nil(t, err)
		require.NotNil(t, prefix)

		data, err := prefix.GobEncode()
		require.NoError(t, err)

		newPrefix := &Prefix{}
		err = newPrefix.GobDecode(data)
		require.NoError(t, err)
		require.Equal(t, prefix, newPrefix)
	})
}

func TestPrefix_availablePrefixes(t *testing.T) {
	tests := []struct {
		name                   string
		cidr                   string
		availableChildPrefixes map[string]bool
		want                   uint64
		wantPrefixes           []string
	}{
		{
			name:                   "one child prefix",
			cidr:                   "192.168.0.0/20",
			availableChildPrefixes: map[string]bool{"192.168.0.0/22": false},
			want:                   512 + 256,
		},
		{
			name:                   "two child prefixes",
			cidr:                   "192.168.0.0/16",
			availableChildPrefixes: map[string]bool{"192.168.0.0/22": false, "192.168.0.0/26": false},
			want:                   8192 + 4096 + 2048 + 1024 + 512 + 256,
		},
		{
			name:                   "four child prefixes",
			cidr:                   "192.168.0.0/16",
			availableChildPrefixes: map[string]bool{"192.168.0.0/22": false, "192.168.0.0/26": false, "192.168.128.0/26": false, "192.168.196.0/26": false},
			want:                   4096 + 3*2048 + 3*1024 + 3*512 + 3*256 + 2*128 + 2*64 + 2*32 + 2*16,
		},
		{
			name: "simple ipv6",
			cidr: "2001:0db8:85a3::/120",
			want: 64,
		},
		{
			name:                   "one child prefix ipv6",
			cidr:                   "2001:0db8:85a3::/120",
			availableChildPrefixes: map[string]bool{"2001:0db8:85a3::/122": false},
			want:                   32 + 16,
		},
		{
			name:         "ipv6 count below cap",
			cidr:         "2001:db8::/96",
			want:         1 << 30,
			wantPrefixes: []string{"2001:db8::/96"},
		},
		{
			name:         "ipv6 count exceeds uint64",
			cidr:         "2001:db8::/48",
			want:         math.MaxInt32,
			wantPrefixes: []string{"2001:db8::/48"},
		},
		{
			name: "ipv6 fragments exceed cap and keep all prefixes",
			cidr: "2001:db8::/94",
			availableChildPrefixes: map[string]bool{
				"2001:db8::1:0:0/96": false,
				"2001:db8::3:0:0/97": false,
			},
			want: math.MaxInt32,
			wantPrefixes: []string{
				"2001:db8::/96",
				"2001:db8::2:0:0/96",
				"2001:db8::3:8000:0/97",
			},
		},
	}
	for _, tt := range tests {
		test := tt
		t.Run(tt.name, func(t *testing.T) {
			p := &Prefix{
				Cidr:                   test.cidr,
				availableChildPrefixes: test.availableChildPrefixes,
			}
			got, avpfxs := p.availablePrefixes()
			t.Logf("available prefixes: %v", avpfxs)
			if test.wantPrefixes != nil {
				require.Equal(t, test.wantPrefixes, avpfxs)
			}

			if test.want != got {
				t.Errorf("Prefix.availablePrefixes() = %d, want %d", got, test.want)
			}

			got2 := p.Usage().AvailableSmallestPrefixes
			if test.want != got2 {
				t.Errorf("Prefix.availablePrefixes() = %d, want %d", got2, test.want)
			}
		})
	}
}

func TestAcquireIPParallel(t *testing.T) {
	ctx := context.Background()
	ipsCount := 50
	g, _ := errgroup.WithContext(context.Background())

	mu := sync.Mutex{}
	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		p, err := ipam.NewPrefix(ctx, "192.169.0.0/20")
		if err != nil {
			panic(err)
		}
		ips := make(map[string]bool)
		for n := 0; n < ipsCount; n++ {
			g.Go(func() error {
				ip, err := ipam.AcquireIP(ctx, p.Cidr)
				if err != nil {
					return err
				}
				if ip == nil {
					return fmt.Errorf("ip is nil")
				}

				mu.Lock()
				defer mu.Unlock()
				_, ok := ips[ip.IP.String()]
				if ok {
					return fmt.Errorf("duplicate ip:%s allocated", ip.IP.String())
				}
				ips[ip.IP.String()] = true

				return nil
			})
		}

		err = g.Wait()
		if err != nil {
			t.Fatal(err)
		}
		for ip := range ips {
			err := ipam.ReleaseIPFromPrefix(ctx, p.Cidr, ip)
			if err != nil {
				t.Error(err)
			}
		}
		_, err = ipam.DeletePrefix(ctx, p.Cidr)
		if err != nil {
			t.Error(err)
		}
	})
}

func Test_ipamer_DumpAndLoad(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		prefix, err := ipam.NewPrefix(ctx, "192.168.0.0/24")
		require.Nil(t, err)
		require.NotNil(t, prefix)

		data, err := ipam.Dump(ctx)
		require.Nil(t, err)
		require.NotEmpty(t, data)

		t.Log(data)

		err = ipam.Load(ctx, data)
		require.Error(t, err)
		require.Equal(t, "prefixes exist, please drop existing data before loading", err.Error())

		err = ipam.storage.DeleteAllPrefixes(ctx, ipam.namespace)
		require.NoError(t, err)
		err = ipam.Load(ctx, data)
		require.NoError(t, err)

		newPrefix := ipam.PrefixFrom(ctx, prefix.Cidr)

		require.Equal(t, prefix, newPrefix)
	})
}

func TestIpamer_ReadAllPrefixCidrs(t *testing.T) {
	ctx := context.Background()

	testWithBackends(t, func(t *testing.T, ipam *ipamer) {
		const cidr = "192.168.0.0/20"

		prefix, err := ipam.NewPrefix(ctx, cidr)
		require.Nil(t, err)
		require.NotNil(t, prefix)

		cidrs, err := ipam.ReadAllPrefixCidrs(ctx)
		require.Nil(t, err)
		require.NotNil(t, cidrs)
		require.Equal(t, 1, len(cidrs))
		require.Equal(t, cidr, cidrs[0])
	})
}

func TestPrefix_Network(t *testing.T) {
	tests := []struct {
		name    string
		cidr    string
		want    netip.Addr
		wantErr bool
	}{
		{
			name:    "simple",
			cidr:    "192.168.0.0/16",
			want:    netip.MustParseAddr("192.168.0.0"),
			wantErr: false,
		},
	}
	for _, tt := range tests {
		tt := tt
		t.Run(tt.name, func(t *testing.T) {
			p := &Prefix{
				Cidr: tt.cidr,
			}
			got, err := p.Network()
			if (err != nil) != tt.wantErr {
				t.Errorf("Prefix.Network() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if !reflect.DeepEqual(got, tt.want) {
				t.Errorf("Prefix.Network() = %v, want %v", got, tt.want)
			}
		})
	}
}
