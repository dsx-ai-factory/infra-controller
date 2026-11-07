// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// NVExpansion - ExpectedMachine Expansion
type NVLinkLogicalPartitionExpansion interface{}

// NVLinkLogicalPartitionInterface - interface to NVLinkLogicalPartition
type NVLinkLogicalPartitionInterface interface {
	// List all the apis of NVLinkLogicalPartition here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	NVLinkLogicalPartitionExpansion
}
