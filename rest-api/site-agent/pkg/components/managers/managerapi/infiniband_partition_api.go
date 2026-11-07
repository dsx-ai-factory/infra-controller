// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// InfiniBandPartitionExpansion - InfiniBandPartition Expansion
type InfiniBandPartitionExpansion interface{}

// InfiniBandPartitionInterface - interface to InfiniBandPartition
type InfiniBandPartitionInterface interface {
	// List all the apis of InfiniBandPartition here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	InfiniBandPartitionExpansion
}
