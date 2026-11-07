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

	// Example 1: List all IP Blocks
	fmt.Println("\nExample 1: Listing IP Blocks...")
	paginationFilter := &simple.PaginationFilter{
		PageSize: simple.IntPtr(20),
	}
	ipBlocks, pagination, apiErr := client.GetIpBlocks(ctx, paginationFilter)
	if apiErr != nil {
		fmt.Printf("Error listing IP blocks: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Found %d IP blocks on this page (total: %d)\n", len(ipBlocks), pagination.Total)
	for i, ib := range ipBlocks {
		fmt.Printf("  %d. ID=%s CIDR=%s\n", i+1, ib.ID, ib.Cidr)
	}

	// Example 2: Get a specific IP Block by ID (if any exist)
	if len(ipBlocks) > 0 {
		ipBlockID := ipBlocks[0].ID
		fmt.Printf("\nExample 2: Getting IP Block %s...\n", ipBlockID)
		ipBlock, apiErr := client.GetIpBlock(ctx, ipBlockID)
		if apiErr != nil {
			fmt.Printf("Error getting IP block: %s\n", apiErr.Message)
			os.Exit(1)
		}
		fmt.Printf("Retrieved IP Block: ID=%s CIDR=%s\n", ipBlock.ID, ipBlock.Cidr)
	}

	fmt.Println("\nIP Block example completed successfully.")
}
