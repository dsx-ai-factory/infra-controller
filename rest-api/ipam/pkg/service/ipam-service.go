/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package service

import (
	"context"
	"fmt"
	"log/slog"
	"net/netip"

	"connectrpc.com/connect"
	goipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	v1 "github.com/NVIDIA/infra-controller/rest-api/ipam/api/v1"
)

type IPAMService struct {
	log    *slog.Logger
	ipamer goipam.Ipamer
}

func New(log *slog.Logger, ipamer goipam.Ipamer) *IPAMService {
	return &IPAMService{
		log:    log,
		ipamer: ipamer,
	}
}

func (i *IPAMService) CreatePrefix(ctx context.Context, req *connect.Request[v1.CreatePrefixRequest]) (*connect.Response[v1.CreatePrefixResponse], error) {
	i.log.Debug("createprefix", "req", req)
	if req.Msg.Namespace != nil {
		ctx = goipam.NewContextWithNamespace(ctx, *req.Msg.Namespace)
	}
	resp, err := i.ipamer.NewPrefix(ctx, req.Msg.Cidr)
	if err != nil {
		i.log.Error("createprefix", "error", err)
		return nil, connect.NewError(connect.CodeInvalidArgument, err)
	}
	return &connect.Response[v1.CreatePrefixResponse]{
		Msg: &v1.CreatePrefixResponse{
			Prefix: &v1.Prefix{
				Cidr:       resp.Cidr,
				ParentCidr: resp.ParentCidr,
			},
		},
	}, nil
}
func (i *IPAMService) DeletePrefix(ctx context.Context, req *connect.Request[v1.DeletePrefixRequest]) (*connect.Response[v1.DeletePrefixResponse], error) {
	i.log.Debug("deleteprefix", "req", req)
	if req.Msg.Namespace != nil {
		ctx = goipam.NewContextWithNamespace(ctx, *req.Msg.Namespace)
	}
	resp, err := i.ipamer.DeletePrefix(ctx, req.Msg.Cidr)
	if err != nil {
		i.log.Error("deleteprefix", "error", err)
		return nil, connect.NewError(connect.CodeInvalidArgument, err)
	}

	return &connect.Response[v1.DeletePrefixResponse]{
		Msg: &v1.DeletePrefixResponse{
			Prefix: &v1.Prefix{
				Cidr:       resp.Cidr,
				ParentCidr: resp.ParentCidr,
			},
		},
	}, nil
}
func (i *IPAMService) GetPrefix(ctx context.Context, req *connect.Request[v1.GetPrefixRequest]) (*connect.Response[v1.GetPrefixResponse], error) {
	i.log.Debug("getprefix", "req", req)
	if req.Msg.Namespace != nil {
		ctx = goipam.NewContextWithNamespace(ctx, *req.Msg.Namespace)
	}
	resp := i.ipamer.PrefixFrom(ctx, req.Msg.Cidr)
	if resp == nil {
		return &connect.Response[v1.GetPrefixResponse]{}, connect.NewError(connect.CodeNotFound, fmt.Errorf("prefix:%q not found", req.Msg.Cidr))
	}

	return &connect.Response[v1.GetPrefixResponse]{
		Msg: &v1.GetPrefixResponse{
			Prefix: &v1.Prefix{
				Cidr:       resp.Cidr,
				ParentCidr: resp.ParentCidr,
			},
		},
	}, nil
}
func (i *IPAMService) ListPrefixes(ctx context.Context, req *connect.Request[v1.ListPrefixesRequest]) (*connect.Response[v1.ListPrefixesResponse], error) {
	i.log.Debug("listprefixes", "req", req)
	if req.Msg.Namespace != nil {
		ctx = goipam.NewContextWithNamespace(ctx, *req.Msg.Namespace)
	}
	resp, err := i.ipamer.ReadAllPrefixCidrs(ctx)
	if err != nil {
		i.log.Error("listprefixes", "error", err)
		return nil, connect.NewError(connect.CodeInvalidArgument, err)
	}

	var result []*v1.Prefix
	for _, cidr := range resp {
		p := i.ipamer.PrefixFrom(ctx, cidr)
		if p == nil {
			i.log.Warn("skipping nil prefix of cidr", "cidr", cidr)
			continue
		}
		result = append(result, &v1.Prefix{Cidr: cidr, ParentCidr: p.ParentCidr})
	}
	return &connect.Response[v1.ListPrefixesResponse]{
		Msg: &v1.ListPrefixesResponse{
			Prefixes: result,
		},
	}, nil
}

func (i *IPAMService) AcquireChildPrefix(ctx context.Context, req *connect.Request[v1.AcquireChildPrefixRequest]) (*connect.Response[v1.AcquireChildPrefixResponse], error) {
	i.log.Debug("acquirechildprefix", "req", req)
	if req.Msg.Namespace != nil {
		ctx = goipam.NewContextWithNamespace(ctx, *req.Msg.Namespace)
	}
	var resp *goipam.Prefix
	var err error
	if req.Msg.ChildCidr != nil {
		resp, err = i.ipamer.AcquireSpecificChildPrefix(ctx, req.Msg.Cidr, *req.Msg.ChildCidr)
		if err != nil {
			i.log.Error("acquirechildprefix", "error", err)
			return nil, connect.NewError(connect.CodeInvalidArgument, err)
		}
	} else {
		prefix, parseErr := netip.ParsePrefix(req.Msg.Cidr)
		if parseErr != nil {
			return nil, connect.NewError(connect.CodeInvalidArgument, parseErr)
		}
		// Check the address family limit before narrowing the requested length to uint8.
		if req.Msg.Length > uint32(prefix.Addr().BitLen()) {
			return nil, connect.NewError(connect.CodeInvalidArgument, fmt.Errorf("prefix length must be at most %d", prefix.Addr().BitLen()))
		}
		resp, err = i.ipamer.AcquireChildPrefix(ctx, req.Msg.Cidr, uint8(req.Msg.Length))
		if err != nil {
			i.log.Error("acquirechildprefix", "error", err)
			return nil, connect.NewError(connect.CodeInvalidArgument, err)
		}
	}
	return &connect.Response[v1.AcquireChildPrefixResponse]{
		Msg: &v1.AcquireChildPrefixResponse{
			Prefix: &v1.Prefix{
				Cidr:       resp.Cidr,
				ParentCidr: resp.ParentCidr,
			},
		},
	}, nil
}

func (i *IPAMService) ReleaseChildPrefix(ctx context.Context, req *connect.Request[v1.ReleaseChildPrefixRequest]) (*connect.Response[v1.ReleaseChildPrefixResponse], error) {
	i.log.Debug("releasechildprefix", "req", req)
	if req.Msg.Namespace != nil {
		ctx = goipam.NewContextWithNamespace(ctx, *req.Msg.Namespace)
	}
	prefix := i.ipamer.PrefixFrom(ctx, req.Msg.Cidr)
	if prefix == nil {
		return nil, connect.NewError(connect.CodeNotFound, fmt.Errorf("prefix:%q not found", req.Msg.Cidr))
	}

	err := i.ipamer.ReleaseChildPrefix(ctx, prefix)
	if err != nil {
		i.log.Error("releasechildprefix", "error", err)
		return nil, connect.NewError(connect.CodeInvalidArgument, err)
	}
	return &connect.Response[v1.ReleaseChildPrefixResponse]{
		Msg: &v1.ReleaseChildPrefixResponse{
			Prefix: &v1.Prefix{
				Cidr:       prefix.Cidr,
				ParentCidr: prefix.ParentCidr,
			},
		},
	}, nil
}
func (i *IPAMService) AcquireIP(ctx context.Context, req *connect.Request[v1.AcquireIPRequest]) (*connect.Response[v1.AcquireIPResponse], error) {
	i.log.Debug("acquireip", "req", req)
	if req.Msg.Namespace != nil {
		ctx = goipam.NewContextWithNamespace(ctx, *req.Msg.Namespace)
	}
	var resp *goipam.IP
	var err error
	if req.Msg.Ip != nil {
		resp, err = i.ipamer.AcquireSpecificIP(ctx, req.Msg.PrefixCidr, *req.Msg.Ip)
		if err != nil {
			i.log.Error("acquireip", "error", err)
			return nil, connect.NewError(connect.CodeInvalidArgument, err)
		}
	} else {
		resp, err = i.ipamer.AcquireIP(ctx, req.Msg.PrefixCidr)
		if err != nil {
			i.log.Error("acquireip", "error", err)
			return nil, connect.NewError(connect.CodeInvalidArgument, err)
		}
	}
	return &connect.Response[v1.AcquireIPResponse]{
		Msg: &v1.AcquireIPResponse{
			Ip: &v1.IP{
				Ip:           resp.IP.String(),
				ParentPrefix: resp.ParentPrefix,
			},
		},
	}, nil
}
func (i *IPAMService) ReleaseIP(ctx context.Context, req *connect.Request[v1.ReleaseIPRequest]) (*connect.Response[v1.ReleaseIPResponse], error) {
	i.log.Debug("releaseip", "req", req)
	if req.Msg.Namespace != nil {
		ctx = goipam.NewContextWithNamespace(ctx, *req.Msg.Namespace)
	}
	netip, err := netip.ParseAddr(req.Msg.Ip)
	if err != nil {
		i.log.Error("releaseip", "error", err)
		return nil, connect.NewError(connect.CodeInvalidArgument, err)
	}
	ip := &goipam.IP{
		IP:           netip,
		ParentPrefix: req.Msg.PrefixCidr,
	}
	resp, err := i.ipamer.ReleaseIP(ctx, ip)
	if err != nil {
		i.log.Error("releaseip", "error", err)
		return nil, connect.NewError(connect.CodeInvalidArgument, err)
	}
	return &connect.Response[v1.ReleaseIPResponse]{
		Msg: &v1.ReleaseIPResponse{
			Ip: &v1.IP{
				Ip:           req.Msg.Ip,
				ParentPrefix: resp.ParentCidr,
			},
		},
	}, nil
}
func (i *IPAMService) Dump(ctx context.Context, req *connect.Request[v1.DumpRequest]) (*connect.Response[v1.DumpResponse], error) {
	i.log.Debug("dump", "req", req)
	if req.Msg.Namespace != nil {
		ctx = goipam.NewContextWithNamespace(ctx, *req.Msg.Namespace)
	}
	dump, err := i.ipamer.Dump(ctx)
	if err != nil {
		i.log.Error("dump", "error", err)
		return nil, connect.NewError(connect.CodeInvalidArgument, err)
	}
	return &connect.Response[v1.DumpResponse]{
		Msg: &v1.DumpResponse{
			Dump: dump,
		},
	}, nil
}

func (i *IPAMService) Load(ctx context.Context, req *connect.Request[v1.LoadRequest]) (*connect.Response[v1.LoadResponse], error) {
	i.log.Debug("load", "req", req)
	if req.Msg.Namespace != nil {
		ctx = goipam.NewContextWithNamespace(ctx, *req.Msg.Namespace)
	}
	err := i.ipamer.Load(ctx, req.Msg.Dump)
	if err != nil {
		i.log.Error("load", "error", err)
		return nil, connect.NewError(connect.CodeInvalidArgument, err)
	}
	return &connect.Response[v1.LoadResponse]{}, nil
}
func (i *IPAMService) PrefixUsage(ctx context.Context, req *connect.Request[v1.PrefixUsageRequest]) (*connect.Response[v1.PrefixUsageResponse], error) {
	i.log.Debug("prefixusage", "req", req)
	if req.Msg.Namespace != nil {
		ctx = goipam.NewContextWithNamespace(ctx, *req.Msg.Namespace)
	}
	p := i.ipamer.PrefixFrom(ctx, req.Msg.Cidr)
	if p == nil {
		return nil, connect.NewError(connect.CodeInvalidArgument, fmt.Errorf("prefix:%q not found", req.Msg.Cidr))
	}
	u := p.Usage()

	subnetCount := i.ipamer.GetSubnetCount(ctx, req.Msg.Cidr)

	// A subnet's reserved address count can exceed the prefix size; do not wrap below zero.
	u.AvailableIPs -= min(u.AvailableIPs, 3*subnetCount)

	return &connect.Response[v1.PrefixUsageResponse]{
		Msg: &v1.PrefixUsageResponse{
			AvailableIps:              u.AvailableIPs,
			AcquiredIps:               u.AcquiredIPs,
			AvailableSmallestPrefixes: u.AvailableSmallestPrefixes,
			AvailablePrefixes:         u.AvailablePrefixes,
			AcquiredPrefixes:          u.AcquiredPrefixes,
			AcquiredSubnets:           subnetCount,
		},
	}, nil
}

func (i *IPAMService) CreateNamespace(ctx context.Context, req *connect.Request[v1.CreateNamespaceRequest]) (*connect.Response[v1.CreateNamespaceResponse], error) {
	i.log.Debug("createnamespace", "req", req)
	err := i.ipamer.CreateNamespace(ctx, req.Msg.Namespace)
	if err != nil {
		return nil, err
	}
	return &connect.Response[v1.CreateNamespaceResponse]{}, nil
}

func (i *IPAMService) DeleteNamespace(ctx context.Context, req *connect.Request[v1.DeleteNamespaceRequest]) (*connect.Response[v1.DeleteNamespaceResponse], error) {
	i.log.Debug("deletenamespace", "req", req)
	err := i.ipamer.DeleteNamespace(ctx, req.Msg.Namespace)
	if err != nil {
		return nil, err
	}
	return &connect.Response[v1.DeleteNamespaceResponse]{}, nil
}

func (i *IPAMService) ListNamespaces(ctx context.Context, req *connect.Request[v1.ListNamespacesRequest]) (*connect.Response[v1.ListNamespacesResponse], error) {
	i.log.Debug("", "req", req)
	res, err := i.ipamer.ListNamespaces(ctx)
	if err != nil {
		return nil, err
	}
	return &connect.Response[v1.ListNamespacesResponse]{
		Msg: &v1.ListNamespacesResponse{
			Namespace: res,
		},
	}, nil
}
