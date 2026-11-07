/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	"fmt"
	"testing"
)

func BenchmarkNewPrefix(b *testing.B) {
	ctx := context.Background()
	benchWithBackends(b, func(b *testing.B, ipam *ipamer) {
		for n := 0; n < b.N; n++ {
			p, err := ipam.NewPrefix(ctx, "192.168.0.0/24")
			if err != nil {
				panic(err)
			}
			if p == nil {
				panic("Prefix nil")
			}
			_, err = ipam.DeletePrefix(ctx, p.Cidr)
			if err != nil {
				panic(err)
			}
		}
	})
}

func BenchmarkAcquireIP(b *testing.B) {
	ctx := context.Background()
	testCidr := "10.0.0.0/16"
	benchWithBackends(b, func(b *testing.B, ipam *ipamer) {
		p, err := ipam.NewPrefix(ctx, testCidr)
		if err != nil {
			panic(err)
		}
		for n := 0; n < b.N; n++ {
			ip, err := ipam.AcquireIP(ctx, p.Cidr)
			if err != nil {
				panic(err)
			}
			if ip == nil {
				panic("IP nil")
			}
			p, err = ipam.ReleaseIP(ctx, ip)
			if err != nil {
				panic(err)
			}
		}
		_, err = ipam.DeletePrefix(ctx, testCidr)
		if err != nil {
			b.Fatalf("error deleting prefix:%v", err)
		}
	})
}

func BenchmarkAcquireChildPrefix(b *testing.B) {
	ctx := context.Background()
	benchmarks := []struct {
		name         string
		parentLength uint8
		childLength  uint8
	}{
		{name: "8/14", parentLength: 8, childLength: 14},
		{name: "8/24", parentLength: 8, childLength: 24},
		{name: "16/18", parentLength: 16, childLength: 18},
		{name: "16/26", parentLength: 16, childLength: 26},
	}
	for _, bm := range benchmarks {
		test := bm
		benchWithBackends(b, func(b *testing.B, ipam *ipamer) {
			p, err := ipam.NewPrefix(ctx, fmt.Sprintf("192.168.0.0/%d", test.parentLength))
			if err != nil {
				panic(err)
			}
			for n := 0; n < b.N; n++ {
				p, err := ipam.AcquireChildPrefix(ctx, p.Cidr, test.childLength)
				if err != nil {
					panic(err)
				}
				err = ipam.ReleaseChildPrefix(ctx, p)
				if err != nil {
					panic(err)
				}
			}
			_, err = ipam.DeletePrefix(ctx, p.Cidr)
			if err != nil {
				b.Fatalf("error deleting prefix:%v", err)
			}
		})
	}
}

func BenchmarkPrefixOverlapping(b *testing.B) {
	existingPrefixes := []string{"192.168.0.0/24", "10.0.0.0/8"}
	newPrefixes := []string{"192.168.1.0/24", "11.0.0.0/8"}
	for n := 0; n < b.N; n++ {
		err := PrefixesOverlapping(existingPrefixes, newPrefixes)
		if err != nil {
			b.Errorf("PrefixOverLapping error:%v", err)
		}
	}
}
