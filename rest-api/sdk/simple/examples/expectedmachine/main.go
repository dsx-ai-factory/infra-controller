// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"os"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/simple"
)

func main() {
	// NICO_BASE_URL, NICO_ORG, and NICO_TOKEN are required environment variables.
	// See sdk/simple/README.md for local dev (kind) setup.
	zerolog.TimeFieldFormat = zerolog.TimeFormatUnix
	log.Logger = log.Output(zerolog.ConsoleWriter{Out: os.Stderr})

	client, err := simple.NewClientFromEnvWithLogger(&log.Logger)
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
	var expectedMachineID string

	// Example 1: Get all ExpectedMachines
	fmt.Println("\nExample 1: Getting all ExpectedMachines...")
	startPage := 1
	paginationFilter := simple.PaginationFilter{
		PageNumber: &startPage,
	}
	done := false
	count := 0
	maxResults := 20
	for !done {
		expectedMachines, pagination, apiErr := client.GetExpectedMachines(ctx, &paginationFilter)
		if apiErr != nil {
			fmt.Printf("Error getting ExpectedMachines: %s\n", apiErr.Message)
			os.Exit(1)
		}
		count += len(expectedMachines)
		if pagination != nil {
			if pagination.PageNumber == 1 {
				fmt.Printf("Pagination: Page %d, PageSize: %d, Total: %d\n", pagination.PageNumber, pagination.PageSize, pagination.Total)
			}
			done = count >= pagination.Total || count >= maxResults
		} else {
			done = true
		}
		fmt.Printf("Found %d ExpectedMachines on page %d\n", len(expectedMachines), *paginationFilter.PageNumber)
		for _, em := range expectedMachines {
			fmt.Printf("  - ID: %s, BMC MAC: %s, Chassis SN: %s", em.ID, em.BmcMacAddress, em.ChassisSerialNumber)
			if expectedMachineID == "" {
				expectedMachineID = em.ID
			}
			if em.Sku != nil && em.Sku.Id != nil {
				fmt.Printf(", SKU ID: %s", *em.Sku.Id)
				if em.Sku.DeviceType.IsSet() && em.Sku.DeviceType.Get() != nil {
					fmt.Printf(" (Device Type: %s)", *em.Sku.DeviceType.Get())
				}
			}
			if em.MachineID != nil {
				fmt.Printf(", Machine ID: %s", *em.MachineID)
			}
			if em.Machine != nil && em.Machine.ControllerMachineType.IsSet() && em.Machine.ControllerMachineType.Get() != nil {
				fmt.Printf(" (Machine Type: %s)", *em.Machine.ControllerMachineType.Get())
			}
			fmt.Println()
		}
		(*paginationFilter.PageNumber)++
	}

	// Example 2: Get a specific ExpectedMachine by ID (skip if none exist)
	if expectedMachineID != "" {
		fmt.Println("\nExample 2: Getting a specific ExpectedMachine...")
		retrievedEM, apiErr := client.GetExpectedMachine(ctx, expectedMachineID)
		if apiErr != nil {
			fmt.Printf("Error getting ExpectedMachine: %s\n", apiErr.Message)
			os.Exit(1)
		}
		fmt.Printf("Retrieved ExpectedMachine: ID=%s, BMC MAC=%s, Chassis SN=%s\n",
			retrievedEM.ID, retrievedEM.BmcMacAddress, retrievedEM.ChassisSerialNumber)
		if retrievedEM.Sku != nil {
			fmt.Println("  SKU Details:")
			prettyJSON, _ := json.MarshalIndent(*retrievedEM.Sku, "", "  ")
			fmt.Printf("%s\n", prettyJSON)
		}
		if retrievedEM.Machine != nil {
			fmt.Println("  Machine Details:")
			prettyJSON, _ := json.MarshalIndent(*retrievedEM.Machine, "", "  ")
			fmt.Printf("%s\n", prettyJSON)
		}
	} else {
		fmt.Println("\nExample 2: Skipping (no existing ExpectedMachines)")
	}

	// Example 3: Create an ExpectedMachine
	fmt.Println("\nExample 3: Creating an ExpectedMachine...")
	bmcUsername := "admin"
	bmcPassword := "changeme"
	createRequest := simple.ExpectedMachineCreateRequest{
		BmcMacAddress:            "00:1A:2B:3C:4D:AA",
		BmcUsername:              &bmcUsername,
		BmcPassword:              &bmcPassword,
		ChassisSerialNumber:      "CHASSIS-SN-12345",
		FallbackDPUSerialNumbers: []string{"DPU-SN-001", "DPU-SN-002"},
		Labels: map[string]string{
			"environment": "test",
			"datacenter":  "dc1",
		},
	}
	expectedMachine, apiErr := client.CreateExpectedMachine(ctx, createRequest)
	if apiErr != nil {
		fmt.Printf("Error creating ExpectedMachine: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Created ExpectedMachine with ID: %s, BMC MAC: %s\n", expectedMachine.ID, expectedMachine.BmcMacAddress)
	expectedMachineID = expectedMachine.ID

	// Example 4: Update an ExpectedMachine
	fmt.Println("\nExample 4: Updating an ExpectedMachine...")
	newChassisSerialNumber := "CHASSIS-SN-67890-UPDATED"
	newBmcUsername := "updated-user"
	updateRequest := simple.ExpectedMachineUpdateRequest{
		ChassisSerialNumber:      &newChassisSerialNumber,
		BmcUsername:              &newBmcUsername,
		FallbackDPUSerialNumbers: []string{"DPU-SN-003-updated", "DPU-SN-004-updated", "DPU-SN-005-updated"},
		Labels: map[string]string{
			"environment": "production-updated",
			"datacenter":  "dc2-updated",
			"updated":     "true-updated",
		},
	}
	updatedEM, apiErr := client.UpdateExpectedMachine(ctx, expectedMachineID, updateRequest)
	if apiErr != nil {
		fmt.Printf("Error updating ExpectedMachine: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Updated ExpectedMachine: ID=%s, Chassis SN=%s, DPU count=%d\n",
		updatedEM.ID, updatedEM.ChassisSerialNumber, len(updatedEM.FallbackDPUSerialNumbers))

	// Example 5: Delete an ExpectedMachine
	fmt.Println("\nExample 5: Deleting an ExpectedMachine...")
	apiErr = client.DeleteExpectedMachine(ctx, expectedMachineID)
	if apiErr != nil {
		fmt.Printf("Error deleting ExpectedMachine: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Deleted ExpectedMachine with ID: %s\n", updatedEM.ID)

	// Verify deletion
	fmt.Println("\nVerifying deletion...")
	_, apiErr = client.GetExpectedMachine(ctx, expectedMachineID)
	if apiErr != nil {
		if apiErr.Code == http.StatusNotFound {
			fmt.Printf("ExpectedMachine with ID %s successfully deleted (no longer present)\n", updatedEM.ID)
		} else {
			fmt.Printf("Error verifying ExpectedMachine deletion: %s\n", apiErr.Message)
			os.Exit(1)
		}
	} else {
		fmt.Println("Error: ExpectedMachine still exists after deletion")
		os.Exit(1)
	}
	fmt.Println("\nAll examples completed successfully!")
}
