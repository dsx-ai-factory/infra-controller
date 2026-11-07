// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// SpectrumXPartitionExpansion - SpectrumX Partition expansion hook for future APIs
type SpectrumXPartitionExpansion interface{}

// SpectrumXPartitionInterface - Interface for SpectrumX Partition site-agent manager.
//
// There is no RegisterSubscriber because create and delete reach Core through the generic
// gRPC proxy rather than per-Site CRUD workflows, so this manager only collects inventory.
type SpectrumXPartitionInterface interface {
	Init()
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	SpectrumXPartitionExpansion
}
