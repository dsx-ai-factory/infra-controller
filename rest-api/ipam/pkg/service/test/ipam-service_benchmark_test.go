/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package test

import (
	"context"
	"log/slog"
	"os"

	"net/http"
	"net/http/httptest"
	"testing"

	"connectrpc.com/connect"

	goipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	v1 "github.com/NVIDIA/infra-controller/rest-api/ipam/api/v1"
	"github.com/NVIDIA/infra-controller/rest-api/ipam/api/v1/apiv1connect"
	"github.com/NVIDIA/infra-controller/rest-api/ipam/pkg/service"
	compress "github.com/klauspost/connect-compress/v2"
)

// BenchmarkGrpcImpact located in a separate package to prevent import cycles.
func BenchmarkGrpcImpact(b *testing.B) {
	ctx := context.Background()
	ipam := goipam.New(ctx)
	log := slog.New(slog.NewJSONHandler(os.Stdout, nil))
	mux := http.NewServeMux()
	mux.Handle(apiv1connect.NewIpamServiceHandler(
		service.New(log, ipam),
		compress.WithAll(compress.LevelBalanced),
	))

	server := httptest.NewUnstartedServer(mux)
	server.EnableHTTP2 = true
	server.StartTLS()
	defer server.Close()

	grpc := apiv1connect.NewIpamServiceClient(
		server.Client(),
		server.URL,
		connect.WithGRPC(),
		compress.WithAll(compress.LevelBalanced),
	)
	httpclient := apiv1connect.NewIpamServiceClient(
		server.Client(),
		server.URL,
		compress.WithAll(compress.LevelBalanced),
	)

	grpcUncompressed := apiv1connect.NewIpamServiceClient(
		server.Client(),
		server.URL,
		connect.WithGRPC(),
	)
	httpclientUncompressed := apiv1connect.NewIpamServiceClient(
		server.Client(),
		server.URL,
	)

	benchmarks := []struct {
		name string
		f    func()
	}{
		{
			name: "library",
			f: func() {
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
			},
		},
		{
			name: "grpc",
			f: func() {
				p, err := grpc.CreatePrefix(ctx, connect.NewRequest(&v1.CreatePrefixRequest{
					Cidr: "192.169.0.0/24",
				}))
				if err != nil {
					panic(err)
				}
				if p == nil {
					panic("Prefix nil")
				}
				_, err = grpc.DeletePrefix(ctx, connect.NewRequest(&v1.DeletePrefixRequest{
					Cidr: "192.169.0.0/24",
				}))
				if err != nil {
					panic(err)
				}
			},
		},
		{
			name: "grpc-no-compression",
			f: func() {
				p, err := grpcUncompressed.CreatePrefix(ctx, connect.NewRequest(&v1.CreatePrefixRequest{
					Cidr: "192.169.0.0/24",
				}))
				if err != nil {
					panic(err)
				}
				if p == nil {
					panic("Prefix nil")
				}
				_, err = grpcUncompressed.DeletePrefix(ctx, connect.NewRequest(&v1.DeletePrefixRequest{
					Cidr: "192.169.0.0/24",
				}))
				if err != nil {
					panic(err)
				}
			},
		},
		{
			name: "http",
			f: func() {
				p, err := httpclient.CreatePrefix(ctx, connect.NewRequest(&v1.CreatePrefixRequest{
					Cidr: "192.169.0.0/24",
				}))
				if err != nil {
					panic(err)
				}
				if p == nil {
					panic("Prefix nil")
				}
				_, err = httpclient.DeletePrefix(ctx, connect.NewRequest(&v1.DeletePrefixRequest{
					Cidr: "192.169.0.0/24",
				}))
				if err != nil {
					panic(err)
				}
			},
		},
		{
			name: "http-no-compression",
			f: func() {
				p, err := httpclientUncompressed.CreatePrefix(ctx, connect.NewRequest(&v1.CreatePrefixRequest{
					Cidr: "192.169.0.0/24",
				}))
				if err != nil {
					panic(err)
				}
				if p == nil {
					panic("Prefix nil")
				}
				_, err = httpclientUncompressed.DeletePrefix(ctx, connect.NewRequest(&v1.DeletePrefixRequest{
					Cidr: "192.169.0.0/24",
				}))
				if err != nil {
					panic(err)
				}
			},
		},
	}

	for _, bm := range benchmarks {
		bm := bm
		b.Run(bm.name, func(b *testing.B) {
			for i := 0; i < b.N; i++ {
				bm.f()
			}
		})
	}
	for n := 0; n < b.N; n++ {

	}
}
