// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"fmt"
	"os"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/simple"
)

func main() {
	// NICO_BASE_URL, NICO_ORG, and NICO_TOKEN are required.
	// See sdk/simple/README.md for local dev (kind) setup.
	client, err := simple.NewClientFromEnv()
	if err != nil {
		fmt.Println("Error creating client:", err)
		os.Exit(1)
	}
	ctx := context.Background()
	if siteID := os.Getenv("NICO_SITE_ID"); siteID != "" {
		client.SetSiteID(siteID)
	}
	if err := client.Authenticate(ctx); err != nil {
		fmt.Printf("Error authenticating: %v\n", err)
		os.Exit(1)
	}

	// Example 1: List all Instances
	fmt.Println("\nExample 1: Listing Instances...")
	paginationFilter := &simple.PaginationFilter{
		PageSize: simple.IntPtr(20),
	}
	instances, pagination, apiErr := client.GetInstances(ctx, nil, paginationFilter)
	if apiErr != nil {
		fmt.Printf("Error listing instances: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Found %d instances on this page (total: %d)\n", len(instances), pagination.Total)
	for i, inst := range instances {
		name := ""
		if inst.Name != nil {
			name = *inst.Name
		}
		status := ""
		if inst.Status != nil {
			status = string(*inst.Status)
		}
		fmt.Printf("  %d. ID=%s Name=%s Status=%s\n", i+1, inst.GetId(), name, status)
	}

	// Example 2: Get a specific Instance by ID (if any exist)
	if len(instances) > 0 {
		instanceID := instances[0].GetId()
		fmt.Printf("\nExample 2: Getting Instance %s...\n", instanceID)
		instance, apiErr := client.GetInstance(ctx, instanceID)
		if apiErr != nil {
			fmt.Printf("Error getting instance: %s\n", apiErr.Message)
			os.Exit(1)
		}
		name := ""
		if instance.Name != nil {
			name = *instance.Name
		}
		status := ""
		if instance.Status != nil {
			status = string(*instance.Status)
		}
		fmt.Printf("Retrieved Instance: ID=%s Name=%s Status=%s\n",
			instance.GetId(), name, status)
	}

	fmt.Println("\nInstance example completed successfully.")
}
