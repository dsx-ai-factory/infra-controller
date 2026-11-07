/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	"fmt"
	"strings"
	"time"
)

func ExampleIpamer_NewPrefix() {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	ipamer := New(context.Background())
	prefix, err := ipamer.NewPrefix(ctx, "192.168.0.0/24")
	if err != nil {
		panic(err)
	}
	ip1, err := ipamer.AcquireIP(ctx, prefix.Cidr)
	if err != nil {
		panic(err)
	}
	ip2, err := ipamer.AcquireIP(ctx, prefix.Cidr)
	if err != nil {
		panic(err)
	}

	fmt.Printf("Super Prefix  : %s\n", prefix)
	fmt.Printf("Super Prefix IP1 : %s\n", ip1.IP.String())
	fmt.Printf("Super Prefix IP1 Parent : %s\n", ip1.ParentPrefix)
	fmt.Printf("Super Prefix IP2 : %s\n", ip2.IP.String())
	fmt.Printf("Super Prefix IP2 Parent : %s\n", ip2.ParentPrefix)
	// Output:
	// Super Prefix  : 192.168.0.0/24
	// Super Prefix IP1 : 192.168.0.1
	// Super Prefix IP1 Parent : 192.168.0.0/24
	// Super Prefix IP2 : 192.168.0.2
	// Super Prefix IP2 Parent : 192.168.0.0/24

	_, err = ipamer.ReleaseIP(ctx, ip2)
	if err != nil {
		panic(err)
	}

	_, err = ipamer.ReleaseIP(ctx, ip1)
	if err != nil {
		panic(err)
	}
	_, err = ipamer.DeletePrefix(ctx, prefix.Cidr)
	if err != nil {
		panic(err)
	}

}
func ExampleIpamer_AcquireChildPrefix() {
	ctx := context.Background()
	ipamer := New(context.Background())
	prefix, err := ipamer.NewPrefix(ctx, "2001:aabb::/48")
	if err != nil {
		panic(err)
	}
	cp1, err := ipamer.AcquireChildPrefix(ctx, prefix.Cidr, 64)
	if err != nil {
		panic(err)
	}
	cp2, err := ipamer.AcquireChildPrefix(ctx, prefix.Cidr, 72)
	if err != nil {
		panic(err)
	}
	ip21, err := ipamer.AcquireIP(ctx, cp2.Cidr)
	if err != nil {
		panic(err)
	}
	prefix = ipamer.PrefixFrom(ctx, prefix.Cidr)
	fmt.Printf("Super Prefix  : %s\n", prefix)
	fmt.Printf("Child Prefix 1: %s\n", cp1)
	fmt.Printf("Child Prefix 2: %s\n", cp2)
	fmt.Printf("Child Prefix 2 IP1: %s\n", ip21.IP)
	fmt.Printf("Super Prefix available child prefixes with 2 bytes: %d\n", prefix.Usage().AvailableSmallestPrefixes)
	fmt.Printf("Super Prefix available child prefixes: %s\n", strings.Join(prefix.Usage().AvailablePrefixes, ","))
	// Output:
	// Super Prefix  : 2001:aabb::/48
	// Child Prefix 1: 2001:aabb::/64
	// Child Prefix 2: 2001:aabb:0:1::/72
	// Child Prefix 2 IP1: 2001:aabb:0:1::1
	// Super Prefix available child prefixes with 2 bytes: 2147483647
	// Super Prefix available child prefixes: 2001:aabb:0:1:100::/72,2001:aabb:0:1:200::/71,2001:aabb:0:1:400::/70,2001:aabb:0:1:800::/69,2001:aabb:0:1:1000::/68,2001:aabb:0:1:2000::/67,2001:aabb:0:1:4000::/66,2001:aabb:0:1:8000::/65,2001:aabb:0:2::/63,2001:aabb:0:4::/62,2001:aabb:0:8::/61,2001:aabb:0:10::/60,2001:aabb:0:20::/59,2001:aabb:0:40::/58,2001:aabb:0:80::/57,2001:aabb:0:100::/56,2001:aabb:0:200::/55,2001:aabb:0:400::/54,2001:aabb:0:800::/53,2001:aabb:0:1000::/52,2001:aabb:0:2000::/51,2001:aabb:0:4000::/50,2001:aabb:0:8000::/49
	err = ipamer.ReleaseChildPrefix(ctx, cp1)
	if err != nil {
		panic(err)
	}

	_, err = ipamer.ReleaseIP(ctx, ip21)
	if err != nil {
		panic(err)
	}

	err = ipamer.ReleaseChildPrefix(ctx, cp2)
	if err != nil {
		panic(err)
	}
	_, err = ipamer.DeletePrefix(ctx, prefix.Cidr)
	if err != nil {
		panic(err)
	}
}
