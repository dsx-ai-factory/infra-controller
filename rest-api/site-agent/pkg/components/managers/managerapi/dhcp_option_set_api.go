// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// DhcpOptionSetExpansion - DhcpOptionSet expansion hook for future APIs
type DhcpOptionSetExpansion interface{}

// DhcpOptionSetInterface - Interface for DhcpOptionSet site-agent manager.
//
// There is no RegisterSubscriber because create, update and delete reach Core through the
// generic gRPC proxy, so this manager only collects inventory.
type DhcpOptionSetInterface interface {
	Init()
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	DhcpOptionSetExpansion
}
