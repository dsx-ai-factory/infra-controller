/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package service

import (
	"context"
	"fmt"
	"os"

	"log/slog"

	"net/http"
	"net/http/httptest"
	"testing"

	"connectrpc.com/connect"
	goipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	v1 "github.com/NVIDIA/infra-controller/rest-api/ipam/api/v1"
	"github.com/NVIDIA/infra-controller/rest-api/ipam/api/v1/apiv1connect"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestIPAMService_AcquireChildPrefix(t *testing.T) {
	for _, tc := range []struct {
		name     string
		request  *v1.AcquireChildPrefixRequest
		wantCIDR string
	}{
		{name: "IPv6 maximum", request: &v1.AcquireChildPrefixRequest{Cidr: "2001:db8::/64", Length: 128}, wantCIDR: "2001:db8::/128"},
		{name: "IPv6 length would wrap", request: &v1.AcquireChildPrefixRequest{Cidr: "2001:db8::/64", Length: 336}},
		{name: "IPv4 too long", request: &v1.AcquireChildPrefixRequest{Cidr: "192.0.2.0/24", Length: 33}},
		{name: "equal length remains unsupported", request: &v1.AcquireChildPrefixRequest{Cidr: "2001:db8::/64", Length: 64}},
		{name: "specific CIDR ignores length", request: &v1.AcquireChildPrefixRequest{Cidr: "2001:db8::/64", Length: 336, ChildCidr: new("2001:db8::/80")}, wantCIDR: "2001:db8::/80"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			ipamer := goipam.New(ctx)
			_, err := ipamer.NewPrefix(ctx, tc.request.Cidr)
			require.NoError(t, err)
			service := New(slog.Default(), ipamer)
			response, err := service.AcquireChildPrefix(ctx, connect.NewRequest(tc.request))
			if tc.wantCIDR == "" {
				require.Error(t, err)
				assert.Equal(t, connect.CodeInvalidArgument, connect.CodeOf(err))
				assert.Equal(t, uint64(0), ipamer.PrefixFrom(ctx, tc.request.Cidr).Usage().AcquiredPrefixes)
			} else {
				require.NoError(t, err)
				assert.Equal(t, tc.wantCIDR, response.Msg.Prefix.Cidr)
			}
		})
	}
}

func TestIpamService(t *testing.T) {
	t.Parallel()
	log := slog.New(slog.NewJSONHandler(os.Stdout, nil))
	mux := http.NewServeMux()
	mux.Handle(apiv1connect.NewIpamServiceHandler(
		New(log, goipam.New(context.Background())),
	))
	server := httptest.NewUnstartedServer(mux)
	server.EnableHTTP2 = true
	server.StartTLS()
	defer server.Close()

	connectClient := apiv1connect.NewIpamServiceClient(
		server.Client(),
		server.URL,
	)
	grpcClient := apiv1connect.NewIpamServiceClient(
		server.Client(),
		server.URL,
		connect.WithGRPC(),
	)
	clients := []apiv1connect.IpamServiceClient{connectClient, grpcClient}

	t.Run("CreateDeleteGetPrefix", func(t *testing.T) {
		counter := 0
		for _, client := range clients {
			result, err := client.CreatePrefix(context.Background(), connect.NewRequest(&v1.CreatePrefixRequest{
				Cidr: fmt.Sprintf("192.169.%d.0/24", counter),
			}))
			require.NoError(t, err)
			assert.Equal(t, result.Msg.Prefix.Cidr, fmt.Sprintf("192.169.%d.0/24", counter))

			getresult, err := client.GetPrefix(context.Background(), connect.NewRequest(&v1.GetPrefixRequest{
				Cidr: fmt.Sprintf("192.169.%d.0/24", counter),
			}))
			require.NoError(t, err)
			assert.Equal(t, getresult.Msg.Prefix.Cidr, fmt.Sprintf("192.169.%d.0/24", counter))

			deleteresult, err := client.DeletePrefix(context.Background(), connect.NewRequest(&v1.DeletePrefixRequest{
				Cidr: fmt.Sprintf("192.169.%d.0/24", counter),
			}))
			require.NoError(t, err)
			assert.Equal(t, deleteresult.Msg.Prefix.Cidr, fmt.Sprintf("192.169.%d.0/24", counter))

			_, err = client.GetPrefix(context.Background(), connect.NewRequest(&v1.GetPrefixRequest{
				Cidr: fmt.Sprintf("192.169.%d.0/24", counter),
			}))
			require.Error(t, err, fmt.Errorf("prefix:'192.169.%d.0/24' not found", counter))

			counter++
		}
	})

	t.Run("AcquireReleaseChildPrefix", func(t *testing.T) {
		counter := 0
		for _, client := range clients {
			result, err := client.CreatePrefix(context.Background(), connect.NewRequest(&v1.CreatePrefixRequest{
				Cidr: fmt.Sprintf("192.167.%d.0/24", counter),
			}))
			require.NoError(t, err)
			assert.Equal(t, result.Msg.Prefix.Cidr, fmt.Sprintf("192.167.%d.0/24", counter))

			acquireresult, err := client.AcquireChildPrefix(context.Background(), connect.NewRequest(&v1.AcquireChildPrefixRequest{
				Cidr:   fmt.Sprintf("192.167.%d.0/24", counter),
				Length: 28,
			}))
			require.NoError(t, err)
			assert.Equal(t, acquireresult.Msg.Prefix.Cidr, fmt.Sprintf("192.167.%d.0/28", counter))

			releaseresult, err := client.ReleaseChildPrefix(context.Background(), connect.NewRequest(&v1.ReleaseChildPrefixRequest{
				Cidr: acquireresult.Msg.Prefix.Cidr,
			}))
			require.NoError(t, err)
			assert.Equal(t, releaseresult.Msg.Prefix.Cidr, acquireresult.Msg.Prefix.Cidr)

			counter++
		}
	})

	t.Run("AcquireReleaseIP", func(t *testing.T) {
		counter := 0
		for _, client := range clients {
			result, err := client.CreatePrefix(context.Background(), connect.NewRequest(&v1.CreatePrefixRequest{
				Cidr: fmt.Sprintf("192.166.%d.0/24", counter),
			}))
			require.NoError(t, err)
			assert.Equal(t, result.Msg.Prefix.Cidr, fmt.Sprintf("192.166.%d.0/24", counter))

			acquireresult, err := client.AcquireIP(context.Background(), connect.NewRequest(&v1.AcquireIPRequest{
				PrefixCidr: fmt.Sprintf("192.166.%d.0/24", counter),
			}))
			require.NoError(t, err)
			assert.Equal(t, acquireresult.Msg.Ip.Ip, fmt.Sprintf("192.166.%d.1", counter))

			releaseresult, err := client.ReleaseIP(context.Background(), connect.NewRequest(&v1.ReleaseIPRequest{
				PrefixCidr: fmt.Sprintf("192.166.%d.0/24", counter),
				Ip:         acquireresult.Msg.Ip.Ip,
			}))
			require.NoError(t, err)
			assert.Equal(t, releaseresult.Msg.Ip.Ip, fmt.Sprintf("192.166.%d.1", counter))

			counter++
		}
	})

	t.Run("CreateDeleteGetPrefixNamespaced", func(t *testing.T) {
		counter := 0
		for _, client := range clients {
			namespace := fmt.Sprintf("testns-%d", counter)
			_, err := client.CreateNamespace(context.Background(), connect.NewRequest(&v1.CreateNamespaceRequest{Namespace: namespace}))
			require.NoError(t, err)

			result, err := client.CreatePrefix(context.Background(), connect.NewRequest(&v1.CreatePrefixRequest{
				Cidr:      "192.169.0.0/24",
				Namespace: &namespace,
			}))
			require.NoError(t, err)
			assert.Equal(t, result.Msg.Prefix.Cidr, "192.169.0.0/24")

			getresult, err := client.GetPrefix(context.Background(), connect.NewRequest(&v1.GetPrefixRequest{
				Cidr:      "192.169.0.0/24",
				Namespace: &namespace,
			}))
			require.NoError(t, err)
			assert.Equal(t, getresult.Msg.Prefix.Cidr, "192.169.0.0/24")

			deleteresult, err := client.DeletePrefix(context.Background(), connect.NewRequest(&v1.DeletePrefixRequest{
				Cidr:      "192.169.0.0/24",
				Namespace: &namespace,
			}))
			require.NoError(t, err)
			assert.Equal(t, deleteresult.Msg.Prefix.Cidr, "192.169.0.0/24", counter)

			_, err = client.GetPrefix(context.Background(), connect.NewRequest(&v1.GetPrefixRequest{
				Cidr:      "192.169.0.0/24",
				Namespace: &namespace,
			}))
			require.Error(t, err, "prefix:'192.169.0.0/24' not found")
			counter++
		}
	})
}

func TestIPAMService_PrefixUsage(t *testing.T) {
	for _, tc := range []struct {
		name         string
		length       uint8
		availableIPs uint64
	}{
		{name: "reservation fits", length: 126, availableIPs: 1},
		{name: "reservation exceeds prefix size", length: 127, availableIPs: 0},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			ipamer := goipam.New(ctx)
			root, err := ipamer.NewPrefix(ctx, "2001:db8::/120")
			require.NoError(t, err)
			allocation, err := ipamer.AcquireChildPrefix(ctx, root.Cidr, 122)
			require.NoError(t, err)
			subnet, err := ipamer.AcquireChildPrefix(ctx, allocation.Cidr, tc.length)
			require.NoError(t, err)

			service := New(slog.Default(), ipamer)
			response, err := service.PrefixUsage(ctx, connect.NewRequest(&v1.PrefixUsageRequest{Cidr: subnet.Cidr}))
			require.NoError(t, err)
			assert.Equal(t, tc.availableIPs, response.Msg.AvailableIps)
		})
	}
}
