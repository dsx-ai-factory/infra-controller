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

	fmt.Println("=== Batch Create Expected Machines ===")

	// Prepare batch create requests
	createRequests := []simple.ExpectedMachineCreateRequest{
		{
			BmcMacAddress:            "00:11:22:33:44:55",
			ChassisSerialNumber:      "CHASSIS-001",
			FallbackDPUSerialNumbers: []string{"DPU-001", "DPU-002"},
			Labels: map[string]string{
				"environment": "production",
				"rack":        "A1",
			},
		},
		{
			BmcMacAddress:            "00:11:22:33:44:56",
			ChassisSerialNumber:      "CHASSIS-002",
			FallbackDPUSerialNumbers: []string{"DPU-003", "DPU-004"},
			Labels: map[string]string{
				"environment": "production",
				"rack":        "A2",
			},
		},
		{
			BmcMacAddress:            "00:11:22:33:44:57",
			ChassisSerialNumber:      "CHASSIS-003",
			FallbackDPUSerialNumbers: []string{"DPU-005", "DPU-006"},
			Labels: map[string]string{
				"environment": "staging",
				"rack":        "B1",
			},
		},
	}

	// Batch create expected machines
	createdMachines, apiErr := client.BatchCreateExpectedMachines(ctx, createRequests)
	if apiErr != nil {
		fmt.Printf("Error batch creating expected machines: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Successfully created %d expected machines\n", len(createdMachines))
	for i, machine := range createdMachines {
		fmt.Printf("  [%d] ID: %s, BMC MAC: %s, Chassis SN: %s\n",
			i+1, machine.ID, machine.BmcMacAddress, machine.ChassisSerialNumber)
	}

	// Batch update expected machines
	fmt.Println("\n=== Batch Update Expected Machines ===")
	if len(createdMachines) < 2 {
		fmt.Println("Not enough machines to demonstrate batch update")
	} else {
		newLabel := "updated"
		updateRequests := []simple.ExpectedMachineUpdateRequest{
			{
				ID: createdMachines[0].ID,
				Labels: map[string]string{
					"environment": "production",
					"rack":        "A1",
					"status":      newLabel,
				},
			},
			{
				ID: createdMachines[1].ID,
				Labels: map[string]string{
					"environment": "production",
					"rack":        "A2",
					"status":      newLabel,
				},
			},
		}
		updatedMachines, apiErr := client.BatchUpdateExpectedMachines(ctx, updateRequests)
		if apiErr != nil {
			fmt.Printf("Error batch updating expected machines: %s\n", apiErr.Message)
		} else {
			fmt.Printf("Successfully updated %d expected machines\n", len(updatedMachines))
			for i, machine := range updatedMachines {
				fmt.Printf("  [%d] ID: %s, Labels: %v\n", i+1, machine.ID, machine.Labels)
			}
		}
	}

	// Clean up: delete created machines
	fmt.Println("\n=== Cleaning up ===")
	for _, machine := range createdMachines {
		apiErr := client.DeleteExpectedMachine(ctx, machine.ID)
		if apiErr != nil {
			fmt.Printf("Warning: Failed to delete expected machine %s: %s\n", machine.ID, apiErr.Message)
		} else {
			fmt.Printf("Deleted expected machine: %s\n", machine.ID)
		}
	}

	fmt.Println("\nBatch operations example completed successfully!")
}
