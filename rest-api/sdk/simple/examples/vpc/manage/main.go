// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"fmt"
	"net/http"
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

	// Example 1: Create a new VPC
	fmt.Println("\n=== Creating a new VPC ===")
	desc := "Example VPC created via SDK"
	createRequest := simple.VpcCreateRequest{
		Name:                      "example-vpc",
		Description:               &desc,
		NetworkVirtualizationType: "FNN",
	}
	vpc, apiErr := client.CreateVpc(ctx, createRequest)
	if apiErr != nil {
		fmt.Printf("Error creating VPC: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Created VPC: ID=%s, Name=%s, NetworkVirtualizationType=%s\n",
		vpc.ID, vpc.Name, vpc.NetworkVirtualizationType)

	// Example 2: Get the VPC by ID
	fmt.Println("\n=== Getting VPC by ID ===")
	retrievedVpc, apiErr := client.GetVpc(ctx, vpc.ID)
	if apiErr != nil {
		fmt.Printf("Error getting VPC: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Retrieved VPC: ID=%s, Name=%s\n", retrievedVpc.ID, retrievedVpc.Name)

	// Example 3: List all VPCs
	fmt.Println("\n=== Listing all VPCs ===")
	siteID := client.GetSiteID()
	vpcFilter := &simple.VpcFilter{SiteID: &siteID}
	paginationFilter := &simple.PaginationFilter{PageSize: simple.IntPtr(10)}
	vpcs, pagination, apiErr := client.GetVpcs(ctx, vpcFilter, paginationFilter)
	if apiErr != nil {
		fmt.Printf("Error listing VPCs: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Found %d VPCs (Total: %d)\n", len(vpcs), pagination.Total)
	for i, v := range vpcs {
		fmt.Printf("  [%d] ID=%s, Name=%s, Type=%s\n", i+1, v.ID, v.Name, v.NetworkVirtualizationType)
	}

	// Example 4: Update the VPC
	fmt.Println("\n=== Updating VPC ===")
	updatedName := "example-vpc-updated"
	updatedDesc := "Updated description for VPC"
	updateRequest := simple.VpcUpdateRequest{
		Name:        &updatedName,
		Description: &updatedDesc,
	}
	updatedVpc, apiErr := client.UpdateVpc(ctx, vpc.ID, updateRequest)
	if apiErr != nil {
		fmt.Printf("Error updating VPC: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Updated VPC: ID=%s, Name=%s\n", updatedVpc.ID, updatedVpc.Name)

	// Example 5: Delete the VPC
	fmt.Println("\n=== Deleting VPC ===")
	apiErr = client.DeleteVpc(ctx, vpc.ID)
	if apiErr != nil {
		fmt.Printf("Error deleting VPC: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Successfully deleted VPC: %s\n", vpc.ID)

	// Verify deletion
	fmt.Println("\n=== Verifying VPC deletion ===")
	_, apiErr = client.GetVpc(ctx, vpc.ID)
	if apiErr != nil {
		if apiErr.Code == http.StatusNotFound {
			fmt.Println("VPC successfully deleted (404 returned)")
		} else {
			fmt.Printf("Unexpected error when verifying deletion: %s\n", apiErr.Message)
		}
	} else {
		fmt.Println("Warning: VPC still exists after deletion")
	}

	fmt.Println("\n=== VPC Management Example Complete ===")
}
