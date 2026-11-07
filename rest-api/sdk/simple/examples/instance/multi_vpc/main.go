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

	// Get the default VPC ID from client metadata
	defaultVpcID := client.GetVpcID()
	fmt.Printf("Default VPC ID from client: %s\n", defaultVpcID)

	// Example 1: Create instance using default VPC (no VPC ID specified)
	fmt.Println("\n=== Example 1: Create instance with default VPC ===")
	fmt.Println("Note: VPC ID is automatically taken from authenticated client metadata")

	// Example 2: List all VPCs to get VPC IDs
	fmt.Println("\n=== Example 2: List available VPCs ===")
	siteID := client.GetSiteID()
	vpcFilter := &simple.VpcFilter{SiteID: &siteID}
	vpcs, _, apiErr := client.GetVpcs(ctx, vpcFilter, nil)
	if apiErr != nil {
		fmt.Printf("Error listing VPCs: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Found %d VPCs:\n", len(vpcs))
	for i, vpc := range vpcs {
		fmt.Printf("  [%d] ID=%s, Name=%s\n", i+1, vpc.ID, vpc.Name)
	}

	// Example 3: Create instance with specific VPC ID (if multiple VPCs exist)
	if len(vpcs) > 1 {
		fmt.Println("\n=== Example 3: Create instance with specific VPC ===")
		specificVpcID := vpcs[1].ID
		fmt.Printf("Creating instance in VPC: %s (%s)\n", vpcs[1].Name, specificVpcID)
		fmt.Println("(Uncomment CreateInstance call in source to actually create)")
	}

	// Example 4: Filter instances by default VPC
	fmt.Println("\n=== Example 4: List instances (default VPC filter) ===")
	instances, pagination, apiErr := client.GetInstances(ctx, nil, nil)
	if apiErr != nil {
		fmt.Printf("Error listing instances: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Found %d instances in default VPC (Total: %d)\n", len(instances), pagination.Total)
	for i, inst := range instances {
		name := ""
		if inst.Name != nil {
			name = *inst.Name
		}
		fmt.Printf("  [%d] ID=%s, Name=%s\n", i+1, inst.GetId(), name)
	}

	// Example 5: Filter instances by specific VPC
	if len(vpcs) > 1 {
		fmt.Println("\n=== Example 5: List instances from specific VPC ===")
		specificVpcID := vpcs[1].ID
		instanceFilter := &simple.InstanceFilter{VpcID: &specificVpcID}
		instancesFiltered, paginationFiltered, apiErr := client.GetInstances(ctx, instanceFilter, nil)
		if apiErr != nil {
			fmt.Printf("Error listing instances from VPC %s: %s\n", specificVpcID, apiErr.Message)
		} else {
			fmt.Printf("Found %d instances in VPC %s (Total: %d)\n",
				len(instancesFiltered), vpcs[1].Name, paginationFiltered.Total)
			for i, inst := range instancesFiltered {
				name := ""
				if inst.Name != nil {
					name = *inst.Name
				}
				fmt.Printf("  [%d] ID=%s, Name=%s\n", i+1, inst.GetId(), name)
			}
		}
	}

	// Example 6: Temporarily change default VPC and list instances
	fmt.Println("\n=== Example 6: Temporarily change default VPC ===")
	originalVpcID := client.GetVpcID()
	fmt.Printf("Original default VPC: %s\n", originalVpcID)
	if len(vpcs) > 1 {
		newDefaultVpc := vpcs[1].ID
		client.SetVpcID(newDefaultVpc)
		fmt.Printf("Changed default VPC to: %s\n", newDefaultVpc)
		instancesNewDefault, paginationNewDefault, apiErr := client.GetInstances(ctx, nil, nil)
		if apiErr != nil {
			fmt.Printf("Error listing instances: %s\n", apiErr.Message)
		} else {
			fmt.Printf("Found %d instances with new default VPC (Total: %d)\n",
				len(instancesNewDefault), paginationNewDefault.Total)
		}
		client.SetVpcID(originalVpcID)
		fmt.Printf("Restored default VPC to: %s\n", originalVpcID)
	}

	// Example 7: List instances across all VPCs (no VPC filter)
	fmt.Println("\n=== Example 7: List instances across all VPCs ===")
	client.SetVpcID("") // Clear the default VPC filter
	instancesAll, paginationAll, apiErr := client.GetInstances(ctx, nil, nil)
	if apiErr != nil {
		fmt.Printf("Error listing all instances: %s\n", apiErr.Message)
	} else {
		fmt.Printf("Found %d instances across all VPCs (Total: %d)\n", len(instancesAll), paginationAll.Total)
	}
	client.SetVpcID(originalVpcID)
	fmt.Printf("Restored default VPC to: %s\n", originalVpcID)

	fmt.Println("\n=== Multi-VPC Instance Management Example Complete ===")
	fmt.Println("\nKey Takeaways:")
	fmt.Println("1. By default, instances use the VPC from authenticated client metadata")
	fmt.Println("2. You can override the VPC by specifying VpcID in InstanceCreateRequest")
	fmt.Println("3. When filtering, you can specify VpcID in InstanceFilter to query specific VPCs")
	fmt.Println("4. You can temporarily change the default VPC using client.SetVpcID()")
	fmt.Println("5. Clear the default VPC filter with client.SetVpcID(\"\") to query all VPCs")
}
