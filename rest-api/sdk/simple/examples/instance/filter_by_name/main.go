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
	// NICO_INSTANCE_NAME is optional; defaults to "test-instance".
	// NICO_SITE_ID and NICO_VPC_ID are optional for testing.
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
	if vpcID := os.Getenv("NICO_VPC_ID"); vpcID != "" {
		client.SetVpcID(vpcID)
	}
	if err := client.Authenticate(ctx); err != nil {
		fmt.Printf("Error authenticating: %v\n", err)
		os.Exit(1)
	}

	// Example 1: Get all instances (no filter, no pagination)
	fmt.Println("Example 1: Getting all instances...")
	instances, pagination, apiErr := client.GetInstances(ctx, nil, nil)
	if apiErr != nil {
		fmt.Printf("Error getting instances: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Found %d instances (Total: %d)\n", len(instances), pagination.Total)
	for _, instance := range instances {
		name := ""
		if instance.Name != nil {
			name = *instance.Name
		}
		status := ""
		if instance.Status != nil {
			status = string(*instance.Status)
		}
		fmt.Printf("  - Name: %s, ID: %s, Status: %s\n", name, instance.GetId(), status)
	}

	// Example 2: Get instances with pagination only
	fmt.Println("\nExample 2: Getting instances with pagination...")
	pageNumber := 1
	pageSize := 10
	orderBy := "created"
	paginationFilter := &simple.PaginationFilter{
		PageNumber: &pageNumber,
		PageSize:   &pageSize,
		OrderBy:    &orderBy,
	}
	instances, pagination, apiErr = client.GetInstances(ctx, nil, paginationFilter)
	if apiErr != nil {
		fmt.Printf("Error getting instances: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Page %d: Found %d instances (Total: %d, PageSize: %d)\n",
		pagination.PageNumber, len(instances), pagination.Total, pagination.PageSize)
	for _, instance := range instances {
		name := ""
		if instance.Name != nil {
			name = *instance.Name
		}
		status := ""
		if instance.Status != nil {
			status = string(*instance.Status)
		}
		fmt.Printf("  - Name: %s, ID: %s, Status: %s\n", name, instance.GetId(), status)
	}

	// Example 3: Filter instances by name only
	fmt.Println("\nExample 3: Filtering instances by name...")
	instanceName := "test-instance"
	if envName := os.Getenv("NICO_INSTANCE_NAME"); envName != "" {
		instanceName = envName
	}
	instanceFilter := &simple.InstanceFilter{Name: &instanceName}
	instances, _, apiErr = client.GetInstances(ctx, instanceFilter, nil)
	if apiErr != nil {
		fmt.Printf("Error getting instances by name: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Found %d instance(s) with name '%s'\n", len(instances), instanceName)
	for _, instance := range instances {
		name := ""
		if instance.Name != nil {
			name = *instance.Name
		}
		status := ""
		if instance.Status != nil {
			status = string(*instance.Status)
		}
		machineID := instance.GetMachineId()
		fmt.Printf("  - Name: %s, ID: %s, Status: %s, MachineID: %s\n",
			name, instance.GetId(), status, machineID)
		if instance.Description.IsSet() {
			fmt.Printf("    Description: %s\n", *instance.Description.Get())
		}
		if instance.Labels != nil {
			fmt.Printf("    Labels: %v\n", instance.Labels)
		}
		if instance.Interfaces != nil {
			fmt.Printf("    Interfaces: %d\n", len(instance.Interfaces))
		}
	}

	// Example 4: Filter instances by name WITH pagination
	fmt.Println("\nExample 4: Filtering instances by name with pagination...")
	pageNumber = 1
	pageSize = 5
	paginationFilter = &simple.PaginationFilter{
		PageNumber: &pageNumber,
		PageSize:   &pageSize,
	}
	instances, pagination, apiErr = client.GetInstances(ctx, instanceFilter, paginationFilter)
	if apiErr != nil {
		fmt.Printf("Error getting instances: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Found %d instance(s) with name '%s' on page %d (Total: %d)\n",
		len(instances), instanceName, pagination.PageNumber, pagination.Total)
	for _, instance := range instances {
		name := ""
		if instance.Name != nil {
			name = *instance.Name
		}
		status := ""
		if instance.Status != nil {
			status = string(*instance.Status)
		}
		fmt.Printf("  - Name: %s, ID: %s, Status: %s\n", name, instance.GetId(), status)
	}

	fmt.Println("\nAll examples completed successfully!")
}
