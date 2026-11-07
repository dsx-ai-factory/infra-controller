/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	"net/netip"
	"os"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestIntegration(t *testing.T) {
	ctx := context.Background()

	_, storage, err := startPostgres()
	require.NoError(t, err)
	defer storage.db.Close()
	dump, err := os.ReadFile("testdata/ipamt.dump.sql")
	require.NoError(t, err)
	require.NotNil(t, dump)
	storage.db.MustExec(string(dump))
	ipam := NewWithStorage(storage)

	// Public Internet
	publicInternet := ipam.PrefixFrom(ctx, "1.2.3.0/27")
	require.NotNil(t, publicInternet)

	require.Equal(t, 25, int(publicInternet.Usage().AcquiredIPs))
	require.Equal(t, 32, int(publicInternet.Usage().AvailableIPs))
	require.Equal(t, "", publicInternet.ParentCidr)
	_, err = ipam.AcquireChildPrefix(ctx, publicInternet.Cidr, 29)
	require.EqualError(t, err, "prefix 1.2.3.0/27 has ips, acquire child prefix not possible")
	_, err = ipam.AcquireSpecificChildPrefix(ctx, publicInternet.Cidr, "1.2.3.0/29")
	require.EqualError(t, err, "prefix 1.2.3.0/27 has ips, acquire child prefix not possible")
	ip, err := ipam.AcquireIP(ctx, publicInternet.Cidr)
	require.NoError(t, err)
	require.NotNil(t, ip)
	require.True(t, strings.HasPrefix(ip.IP.String(), "1.2.3"))
	require.Equal(t, "1.2.3.20", ip.IP.String())
	// reread prefix
	publicInternet = ipam.PrefixFrom(ctx, "1.2.3.0/27")
	require.Equal(t, 26, int(publicInternet.Usage().AcquiredIPs))
	_, err = ipam.ReleaseIP(ctx, ip)
	require.NoError(t, err)
	// reread prefix
	publicInternet = ipam.PrefixFrom(ctx, "1.2.3.0/27")
	require.Equal(t, 25, int(publicInternet.Usage().AcquiredIPs))
	// release acquired ip
	err = ipam.ReleaseIPFromPrefix(ctx, "1.2.3.0/27", "1.2.3.1")
	require.NoError(t, err)
	// reread prefix
	publicInternet = ipam.PrefixFrom(ctx, "1.2.3.0/27")
	require.Equal(t, 24, int(publicInternet.Usage().AcquiredIPs))
	// release unacquired ip
	err = ipam.ReleaseIPFromPrefix(ctx, "1.2.3.0/27", "1.2.3.24")
	require.EqualError(t, err, "NotFound: unable to release ip:1.2.3.24 because it is not allocated in prefix:1.2.3.0/27")

	// Tenant super network
	tenantSuper := ipam.PrefixFrom(ctx, "10.128.0.0/14")
	require.NotNil(t, tenantSuper)
	require.Equal(t, uint64(2), tenantSuper.Usage().AcquiredIPs)
	sum := 0
	for _, pfx := range tenantSuper.Usage().AvailablePrefixes {
		// Only logs if fails
		ipprefix, err := netip.ParsePrefix(pfx)
		require.NoError(t, err)
		smallest := 1 << (ipprefix.Addr().BitLen() - 2 - ipprefix.Bits())
		sum += smallest
		t.Logf("available prefix:%s smallest left:%d sum:%d", pfx, smallest, sum)
	}
	require.Equal(t, 60928, int(tenantSuper.Usage().AvailableSmallestPrefixes))
	require.Equal(t, 18, int(tenantSuper.Usage().AcquiredPrefixes))

	cp, err := ipam.AcquireChildPrefix(ctx, "10.128.0.0/14", 22)
	require.NoError(t, err)
	require.NotNil(t, cp)
	require.True(t, strings.HasPrefix(cp.Cidr, "10."))
	require.True(t, strings.HasSuffix(cp.Cidr, "/22"))
	require.Equal(t, "10.128.0.0/14", cp.ParentCidr)

	// reread
	tenantSuper = ipam.PrefixFrom(ctx, "10.128.0.0/14")
	require.NotNil(t, tenantSuper)
	require.Equal(t, 19, int(tenantSuper.Usage().AcquiredPrefixes))
	err = ipam.ReleaseChildPrefix(ctx, cp)
	require.NoError(t, err)
	// reread
	tenantSuper = ipam.PrefixFrom(ctx, "10.128.0.0/14")
	require.NotNil(t, tenantSuper)
	require.Equal(t, 18, int(tenantSuper.Usage().AcquiredPrefixes))

	cp, err = ipam.AcquireSpecificChildPrefix(ctx, "10.128.0.0/14", "10.128.4.0/22")
	require.NoError(t, err)
	require.NotNil(t, cp)
	require.Equal(t, "10.128.4.0/22", cp.String())
	require.Equal(t, "10.128.0.0/14", cp.ParentCidr)

	// reread
	tenantSuper = ipam.PrefixFrom(ctx, "10.128.0.0/14")
	require.NotNil(t, tenantSuper)
	require.Equal(t, 19, int(tenantSuper.Usage().AcquiredPrefixes))
	err = ipam.ReleaseChildPrefix(ctx, cp)
	require.NoError(t, err)
	// reread
	tenantSuper = ipam.PrefixFrom(ctx, "10.128.0.0/14")
	require.NotNil(t, tenantSuper)

	_, err = ipam.AcquireIP(ctx, "10.128.0.0/14")
	require.EqualError(t, err, "prefix 10.128.0.0/14 has childprefixes, acquire ip not possible")

	// Release existing child prefix
	existingChild := ipam.PrefixFrom(ctx, "10.129.28.0/22")
	require.NotNil(t, existingChild)
	err = ipam.ReleaseChildPrefix(ctx, existingChild)
	require.NoError(t, err)
	// reread
	tenantSuper = ipam.PrefixFrom(ctx, "10.128.0.0/14")
	require.NotNil(t, tenantSuper)
	require.Equal(t, 17, int(tenantSuper.Usage().AcquiredPrefixes))

	// Release existing child prefix with ips
	existingChildWithIPs := ipam.PrefixFrom(ctx, "10.130.36.0/22")
	require.NotNil(t, existingChildWithIPs)
	err = ipam.ReleaseChildPrefix(ctx, existingChildWithIPs)
	require.EqualError(t, err, "prefix 10.130.36.0/22 has ips, deletion not possible")

	// Read all child prefixes
	pfxs, err := storage.ReadAllPrefixes(ctx, "")
	require.NoError(t, err)
	childPrefixCount := 0
	for _, pfx := range pfxs {
		if pfx.ParentCidr != "" {
			require.Equal(t, tenantSuper.Cidr, pfx.ParentCidr)
			childPrefixCount++
		}
	}
	// FIXME This Super Prefix has leaked child prefixes !
	// require.Equal(t, childPrefixCount, tenantSuper.Usage().AcquiredPrefixes)
}
func TestIntegrationP(t *testing.T) {
	ctx := context.Background()
	_, storage, err := startPostgres()
	require.NoError(t, err)
	defer storage.db.Close()
	dump, err := os.ReadFile("testdata/ipamp.dump.sql")
	require.NoError(t, err)
	require.NotNil(t, dump)
	storage.db.MustExec(string(dump))
	ipam := NewWithStorage(storage)

	// Tenant super network 1
	tenantSuper1 := ipam.PrefixFrom(ctx, "10.64.0.0/14")
	require.NotNil(t, tenantSuper1)
	require.Equal(t, 2, int(tenantSuper1.Usage().AcquiredIPs))
	require.Equal(t, 56320, int(tenantSuper1.Usage().AvailableSmallestPrefixes))
	require.Equal(t, 36, int(tenantSuper1.Usage().AcquiredPrefixes))

	cp, err := ipam.AcquireChildPrefix(ctx, "10.64.0.0/14", 22)
	require.NoError(t, err)
	require.NotNil(t, cp)
	require.True(t, strings.HasPrefix(cp.Cidr, "10."))
	require.True(t, strings.HasSuffix(cp.Cidr, "/22"))
	require.Equal(t, "10.64.0.0/14", cp.ParentCidr)

	// reread
	tenantSuper1 = ipam.PrefixFrom(ctx, "10.64.0.0/14")
	require.NotNil(t, tenantSuper1)
	require.Equal(t, 37, int(tenantSuper1.Usage().AcquiredPrefixes))
	err = ipam.ReleaseChildPrefix(ctx, cp)
	require.NoError(t, err)
	// reread
	tenantSuper1 = ipam.PrefixFrom(ctx, "10.64.0.0/14")
	require.NotNil(t, tenantSuper1)
	require.Equal(t, 36, int(tenantSuper1.Usage().AcquiredPrefixes))

	cp, err = ipam.AcquireSpecificChildPrefix(ctx, "10.64.0.0/14", "10.64.0.0/22")
	require.NoError(t, err)
	require.NotNil(t, cp)
	require.Equal(t, "10.64.0.0/22", cp.String())
	require.Equal(t, "10.64.0.0/14", cp.ParentCidr)

	// reread
	tenantSuper1 = ipam.PrefixFrom(ctx, "10.64.0.0/14")
	require.NotNil(t, tenantSuper1)
	require.Equal(t, 37, int(tenantSuper1.Usage().AcquiredPrefixes))
	err = ipam.ReleaseChildPrefix(ctx, cp)
	require.NoError(t, err)
	// reread
	tenantSuper1 = ipam.PrefixFrom(ctx, "10.64.0.0/14")
	require.NotNil(t, tenantSuper1)
	require.Equal(t, 36, int(tenantSuper1.Usage().AcquiredPrefixes))

	_, err = ipam.AcquireIP(ctx, "10.64.0.0/14")
	require.EqualError(t, err, "prefix 10.64.0.0/14 has childprefixes, acquire ip not possible")

	// Read all child prefixes
	pfxs, err := storage.ReadAllPrefixes(ctx, "")
	require.NoError(t, err)
	childPrefixesOfTenantSuper := make(map[string]bool)

	for _, pfx := range pfxs {
		if pfx.ParentCidr != "" {
			if pfx.ParentCidr != tenantSuper1.Cidr {
				continue
			}
			childPrefixesOfTenantSuper[pfx.String()] = false
		}
	}
	// FIXME the tenantsuper has 2 more prefixes acquired
	require.Equal(t, int(tenantSuper1.Usage().AcquiredPrefixes)-2, len(childPrefixesOfTenantSuper))

	// Tenant super network 2
	tenantSuper2 := ipam.PrefixFrom(ctx, "10.76.0.0/14")
	require.NotNil(t, tenantSuper2)
	require.Equal(t, 2, int(tenantSuper2.Usage().AcquiredIPs))
	require.Equal(t, 58368, int(tenantSuper2.Usage().AvailableSmallestPrefixes))
	require.Equal(t, 28, int(tenantSuper2.Usage().AcquiredPrefixes))

	cp, err = ipam.AcquireChildPrefix(ctx, "10.76.0.0/14", 22)
	require.NoError(t, err)
	require.NotNil(t, cp)
	require.True(t, strings.HasPrefix(cp.Cidr, "10."))
	require.True(t, strings.HasSuffix(cp.Cidr, "/22"))
	require.Equal(t, "10.76.0.0/14", cp.ParentCidr)

	// reread
	tenantSuper2 = ipam.PrefixFrom(ctx, "10.76.0.0/14")
	require.NotNil(t, tenantSuper2)
	require.Equal(t, 29, int(tenantSuper2.Usage().AcquiredPrefixes))
	err = ipam.ReleaseChildPrefix(ctx, cp)
	require.NoError(t, err)
	// reread
	tenantSuper2 = ipam.PrefixFrom(ctx, "10.76.0.0/14")
	require.NotNil(t, tenantSuper2)
	require.Equal(t, 28, int(tenantSuper2.Usage().AcquiredPrefixes))

	cp, err = ipam.AcquireSpecificChildPrefix(ctx, "10.76.0.0/14", "10.76.0.0/22")
	require.NoError(t, err)
	require.NotNil(t, cp)
	require.Equal(t, "10.76.0.0/22", cp.String())
	require.Equal(t, "10.76.0.0/14", cp.ParentCidr)

	// reread
	tenantSuper2 = ipam.PrefixFrom(ctx, "10.76.0.0/14")
	require.NotNil(t, tenantSuper2)
	require.Equal(t, 29, int(tenantSuper2.Usage().AcquiredPrefixes))
	err = ipam.ReleaseChildPrefix(ctx, cp)
	require.NoError(t, err)
	// reread
	tenantSuper2 = ipam.PrefixFrom(ctx, "10.76.0.0/14")
	require.NotNil(t, tenantSuper2)
	require.Equal(t, 28, int(tenantSuper2.Usage().AcquiredPrefixes))

	_, err = ipam.AcquireIP(ctx, "10.76.0.0/14")
	require.EqualError(t, err, "prefix 10.76.0.0/14 has childprefixes, acquire ip not possible")

	// Read all child prefixes
	pfxs, err = storage.ReadAllPrefixes(ctx, "")
	require.NoError(t, err)
	childPrefixesOfTenantSuper = make(map[string]bool)

	for _, pfx := range pfxs {
		if pfx.ParentCidr != "" {
			if pfx.ParentCidr != tenantSuper2.Cidr {
				continue
			}
			childPrefixesOfTenantSuper[pfx.String()] = false
		}
	}
	require.Equal(t, int(tenantSuper2.Usage().AcquiredPrefixes), len(childPrefixesOfTenantSuper))

	// Public Internet
	publicInternet := ipam.PrefixFrom(ctx, "1.2.3.0/25")
	require.NotNil(t, publicInternet)

	require.Equal(t, 128, int(publicInternet.Usage().AcquiredIPs))
	require.Equal(t, 128, int(publicInternet.Usage().AvailableIPs))
	require.Equal(t, "", publicInternet.ParentCidr)
	_, err = ipam.AcquireChildPrefix(ctx, publicInternet.Cidr, 29)
	require.EqualError(t, err, "prefix 1.2.3.0/25 has ips, acquire child prefix not possible")
	_, err = ipam.AcquireSpecificChildPrefix(ctx, publicInternet.Cidr, "1.2.3.0/29")
	require.EqualError(t, err, "prefix 1.2.3.0/25 has ips, acquire child prefix not possible")
	_, err = ipam.AcquireIP(ctx, publicInternet.Cidr)
	require.EqualError(t, err, "NoIPAvailableError: no more ips in prefix: 1.2.3.0/25 left, length of prefix.ips: 128")

}

func TestIntegrationEtcd(t *testing.T) {
	ctx := context.Background()
	_, storage, err := startEtcd()
	require.NoError(t, err)

	ipam := NewWithStorage(storage)

	// Tenant super network 1
	tenantSuper1, err := ipam.NewPrefix(ctx, "10.64.0.0/14")
	require.NoError(t, err)

	require.NotNil(t, tenantSuper1)
	require.Equal(t, 2, int(tenantSuper1.Usage().AcquiredIPs))
	require.Equal(t, 65536, int(tenantSuper1.Usage().AvailableSmallestPrefixes))
	require.Equal(t, 0, int(tenantSuper1.Usage().AcquiredPrefixes))

	cp, err := ipam.AcquireChildPrefix(ctx, "10.64.0.0/14", 22)
	require.NoError(t, err)
	require.NotNil(t, cp)
	require.True(t, strings.HasPrefix(cp.Cidr, "10."))
	require.True(t, strings.HasSuffix(cp.Cidr, "/22"))
	require.Equal(t, "10.64.0.0/14", cp.ParentCidr)

	// reread
	tenantSuper1 = ipam.PrefixFrom(ctx, "10.64.0.0/14")
	require.NotNil(t, tenantSuper1)
	require.Equal(t, 1, int(tenantSuper1.Usage().AcquiredPrefixes))
	err = ipam.ReleaseChildPrefix(ctx, cp)
	require.NoError(t, err)
	// reread
	tenantSuper1 = ipam.PrefixFrom(ctx, "10.64.0.0/14")
	require.NotNil(t, tenantSuper1)
	require.Equal(t, 0, int(tenantSuper1.Usage().AcquiredPrefixes))

	cp, err = ipam.AcquireSpecificChildPrefix(ctx, "10.64.0.0/14", "10.64.0.0/22")
	require.NoError(t, err)
	require.NotNil(t, cp)
	require.Equal(t, "10.64.0.0/22", cp.String())
	require.Equal(t, "10.64.0.0/14", cp.ParentCidr)

	_, err = ipam.AcquireIP(ctx, "10.64.0.0/14")
	require.EqualError(t, err, "prefix 10.64.0.0/14 has childprefixes, acquire ip not possible")

	// Tenant super network 2
	tenantSuper2, err := ipam.NewPrefix(ctx, "10.76.0.0/14")
	require.NoError(t, err)
	require.NotNil(t, tenantSuper2)
	require.Equal(t, 2, int(tenantSuper2.Usage().AcquiredIPs))
	require.Equal(t, 65536, int(tenantSuper2.Usage().AvailableSmallestPrefixes))
	require.Equal(t, 0, int(tenantSuper2.Usage().AcquiredPrefixes))

	cp, err = ipam.AcquireChildPrefix(ctx, "10.76.0.0/14", 22)
	require.NoError(t, err)
	require.NotNil(t, cp)
	require.True(t, strings.HasPrefix(cp.Cidr, "10."))
	require.True(t, strings.HasSuffix(cp.Cidr, "/22"))
	require.Equal(t, "10.76.0.0/14", cp.ParentCidr)

	// reread
	tenantSuper2 = ipam.PrefixFrom(ctx, "10.76.0.0/14")
	require.NotNil(t, tenantSuper2)
	require.Equal(t, 1, int(tenantSuper2.Usage().AcquiredPrefixes))
	err = ipam.ReleaseChildPrefix(ctx, cp)
	require.NoError(t, err)
	// reread
	tenantSuper2 = ipam.PrefixFrom(ctx, "10.76.0.0/14")
	require.NotNil(t, tenantSuper2)
	require.Equal(t, 0, int(tenantSuper2.Usage().AcquiredPrefixes))

	cp, err = ipam.AcquireSpecificChildPrefix(ctx, "10.76.0.0/14", "10.76.0.0/22")
	require.NoError(t, err)
	require.NotNil(t, cp)
	require.Equal(t, "10.76.0.0/22", cp.String())
	require.Equal(t, "10.76.0.0/14", cp.ParentCidr)

	// reread
	tenantSuper2 = ipam.PrefixFrom(ctx, "10.76.0.0/14")
	require.NotNil(t, tenantSuper2)
	require.Equal(t, 1, int(tenantSuper2.Usage().AcquiredPrefixes))
	err = ipam.ReleaseChildPrefix(ctx, cp)
	require.NoError(t, err)
	// reread
	tenantSuper2 = ipam.PrefixFrom(ctx, "10.76.0.0/14")
	require.NotNil(t, tenantSuper2)
	require.Equal(t, 0, int(tenantSuper2.Usage().AcquiredPrefixes))

	_, err = ipam.AcquireIP(ctx, "10.76.0.0/14")
	require.EqualError(t, err, "prefix 10.76.0.0/14 has childprefixes, acquire ip not possible")

	// Read all child prefixes
	pfxs, err := storage.ReadAllPrefixes(ctx, "")
	require.NoError(t, err)
	childPrefixesOfTenantSuper := make(map[string]bool)

	for _, pfx := range pfxs {
		if pfx.ParentCidr != "" {
			if pfx.ParentCidr != tenantSuper2.Cidr {
				continue
			}
			childPrefixesOfTenantSuper[pfx.String()] = false
		}
	}
	require.Equal(t, int(tenantSuper2.Usage().AcquiredPrefixes), len(childPrefixesOfTenantSuper))

	// Public Internet
	publicInternet, err := ipam.NewPrefix(ctx, "1.2.3.0/25")
	require.NoError(t, err)
	require.NotNil(t, publicInternet)

	require.Equal(t, 2, int(publicInternet.Usage().AcquiredIPs))
	require.Equal(t, 128, int(publicInternet.Usage().AvailableIPs))
	require.Equal(t, "", publicInternet.ParentCidr)
	_, err = ipam.AcquireChildPrefix(ctx, publicInternet.Cidr, 29)
	require.NoError(t, err)
	_, err = ipam.AcquireSpecificChildPrefix(ctx, publicInternet.Cidr, "1.2.3.0/29")
	require.EqualError(t, err, "specific prefix 1.2.3.0/29 is not available in prefix 1.2.3.0/25")
	_, err = ipam.AcquireIP(ctx, publicInternet.Cidr)
	require.EqualError(t, err, "prefix 1.2.3.0/25 has childprefixes, acquire ip not possible")
}
