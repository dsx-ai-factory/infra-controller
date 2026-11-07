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

	// Example 1: List all Machines
	fmt.Println("\nExample 1: Listing Machines...")
	paginationFilter := &simple.PaginationFilter{
		PageSize: simple.IntPtr(20),
	}
	machines, pagination, apiErr := client.GetMachines(ctx, paginationFilter)
	if apiErr != nil {
		fmt.Printf("Error listing machines: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Found %d machines on this page (total: %d)\n", len(machines), pagination.Total)
	for i, m := range machines {
		vendor := ""
		if m.Vendor != nil {
			vendor = *m.Vendor
		}
		product := ""
		if m.ProductName != nil {
			product = *m.ProductName
		}
		fmt.Printf("  %d. ID=%s Vendor=%s Product=%s Status=%s\n", i+1, m.ID, vendor, product, m.Status)
	}

	// Example 2: Get a specific Machine by ID (if any exist)
	if len(machines) > 0 {
		machineID := machines[0].ID
		fmt.Printf("\nExample 2: Getting Machine %s...\n", machineID)
		machine, apiErr := client.GetMachine(ctx, machineID)
		if apiErr != nil {
			fmt.Printf("Error getting machine: %s\n", apiErr.Message)
			os.Exit(1)
		}
		vendor := ""
		if machine.Vendor != nil {
			vendor = *machine.Vendor
		}
		product := ""
		if machine.ProductName != nil {
			product = *machine.ProductName
		}
		fmt.Printf("Retrieved Machine: ID=%s Vendor=%s Product=%s Status=%s\n",
			machine.ID, vendor, product, machine.Status)
	}

	fmt.Println("\nMachine example completed successfully.")
}
