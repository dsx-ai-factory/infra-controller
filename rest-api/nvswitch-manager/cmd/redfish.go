// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/secretstring"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/common/util"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/redfish"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
)

var (
	// Common redfish flags
	rfBmcIP        string
	rfUsername     string
	rfPassword     string
	rfInsecure     bool
	rfTaskID       string
	rfFirmwareFile string
)

// redfishCmd represents the redfish command group
var redfishCmd = &cobra.Command{
	Use:   "redfish",
	Short: "Interact with NV-Switch BMC via Redfish API",
	Long: `Interact with an NV-Switch BMC via Redfish API.

Examples:
  # Query chassis information
  nvswitch-manager redfish query-chassis -i 192.168.1.100 -u root -p password

  # Query firmware inventory
  nvswitch-manager redfish query-firmware -i 192.168.1.100 -u root -p password

  # Query task status
  nvswitch-manager redfish query-task -i 192.168.1.100 -u root -p password --task-id /redfish/v1/TaskService/Tasks/1

  # Upload firmware
  nvswitch-manager redfish upload-firmware -i 192.168.1.100 -u root -p password --file firmware.fwpkg`,
}

// Helper function to create a redfish client
func newRedfishClient() (*redfish.RedfishClient, error) {
	if rfBmcIP == "" {
		return nil, fmt.Errorf("BMC IP address is required (--bmc-ip or -i)")
	}
	if rfPassword == "" {
		return nil, fmt.Errorf("password is required (--pass or -p)")
	}

	cred := &credential.Credential{
		User:     rfUsername,
		Password: secretstring.New(rfPassword),
	}

	b, err := bmc.New("00:00:00:00:00:00", rfBmcIP, cred)
	if err != nil {
		return nil, fmt.Errorf("failed to create BMC: %v", err)
	}

	client, err := redfish.New(context.Background(), b, false)
	if err != nil {
		return nil, fmt.Errorf("failed to create redfish client: %v", err)
	}

	return client, nil
}

// queryChassisCmd queries chassis information
var queryChassisCmd = &cobra.Command{
	Use:   "query-chassis",
	Short: "Query chassis information",
	Run: func(cmd *cobra.Command, args []string) {
		client, err := newRedfishClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Logout()

		chassis, err := client.QueryChassis()
		if err != nil {
			log.Fatalf("Failed to query chassis: %v", err)
		}

		fmt.Printf("Chassis Information:\n")
		fmt.Printf("  Model:         %s\n", chassis.Model)
		fmt.Printf("  SKU:           %s\n", chassis.SKU)
		fmt.Printf("  Serial Number: %s\n", chassis.SerialNumber)
		fmt.Printf("  Part Number:   %s\n", chassis.PartNumber)
		fmt.Printf("  Manufacturer:  %s\n", chassis.Manufacturer)
		fmt.Printf("  Power State:   %s\n", chassis.PowerState)
		fmt.Printf("  Health:        %s\n", chassis.Status.Health)
		fmt.Printf("  State:         %s\n", chassis.Status.State)
	},
}

// queryManagerCmd queries BMC manager information
var queryManagerCmd = &cobra.Command{
	Use:   "query-manager",
	Short: "Query BMC manager information",
	Run: func(cmd *cobra.Command, args []string) {
		client, err := newRedfishClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Logout()

		manager, err := client.QueryManager()
		if err != nil {
			log.Fatalf("Failed to query manager: %v", err)
		}

		fmt.Printf("BMC Manager Information:\n")
		fmt.Printf("  Name:             %s\n", manager.Name)
		fmt.Printf("  Manager Type:     %s\n", manager.ManagerType)
		fmt.Printf("  Model:            %s\n", manager.Model)
		fmt.Printf("  Manufacturer:     %s\n", manager.Manufacturer)
		fmt.Printf("  Firmware Version: %s\n", manager.FirmwareVersion)
		fmt.Printf("  Part Number:      %s\n", manager.PartNumber)
		fmt.Printf("  Power State:      %s\n", manager.PowerState)
		fmt.Printf("  UUID:             %s\n", manager.UUID)
		fmt.Printf("  Last Reset Time:  %s\n", manager.LastResetTime)
	},
}

// queryFirmwareCmd queries firmware inventory
var queryFirmwareCmd = &cobra.Command{
	Use:   "query-firmware",
	Short: "Query firmware inventory",
	Run: func(cmd *cobra.Command, args []string) {
		client, err := newRedfishClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Logout()

		inventories, err := client.FirmwareInventories()
		if err != nil {
			log.Fatalf("Failed to query firmware inventories: %v", err)
		}

		fmt.Printf("Firmware Inventory:\n")
		fmt.Printf("%-35s %s\n", "COMPONENT", "VERSION")
		fmt.Printf("%-35s %s\n", "---------", "-------")
		for _, fw := range inventories {
			fmt.Printf("%-35s %s\n", fw.ID, fw.Version)
		}
	},
}

// queryTaskCmd queries task status
var queryTaskCmd = &cobra.Command{
	Use:   "query-task",
	Short: "Query task status by task ID",
	Long: `Query the status of a Redfish task by its ID.

Example:
  nvswitch-manager redfish query-task -i 192.168.1.100 -u root -p password --task-id /redfish/v1/TaskService/Tasks/1`,
	Run: func(cmd *cobra.Command, args []string) {
		if rfTaskID == "" {
			log.Fatalf("Task ID is required (--task-id)")
		}

		client, err := newRedfishClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Logout()

		state, percentComplete, err := client.GetTaskStatus(rfTaskID)
		if err != nil {
			log.Fatalf("Failed to query task status: %v", err)
		}

		fmt.Printf("Task Status:\n")
		fmt.Printf("  Task ID:          %s\n", rfTaskID)
		fmt.Printf("  State:            %s\n", state)
		fmt.Printf("  Percent Complete: %d%%\n", percentComplete)
	},
}

// queryUpdateServiceCmd queries update service information
var queryUpdateServiceCmd = &cobra.Command{
	Use:   "query-update-service",
	Short: "Query update service information",
	Run: func(cmd *cobra.Command, args []string) {
		client, err := newRedfishClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Logout()

		updateService, err := client.UpdateService()
		if err != nil {
			log.Fatalf("Failed to query update service: %v", err)
		}

		// Get apply time using our helper function
		applyTime, err := client.GetHttpPushUriApplyTime()
		if err != nil {
			applyTime = fmt.Sprintf("(error: %v)", err)
		}
		if applyTime == "" {
			applyTime = "(not set)"
		}

		fmt.Printf("Update Service:\n")
		fmt.Printf("  ID:                     %s\n", updateService.ID)
		fmt.Printf("  Name:                   %s\n", updateService.Name)
		fmt.Printf("  Service Enabled:        %v\n", updateService.ServiceEnabled)
		fmt.Printf("  HTTP Push URI:          %s\n", updateService.HTTPPushURI)
		fmt.Printf("  HTTP Push URI ApplyTime: %s\n", applyTime)
		fmt.Printf("  MultipartHTTP Push URI: %s\n", updateService.MultipartHTTPPushURI)
	},
}

// uploadFirmwareCmd uploads firmware to the BMC
var uploadFirmwareCmd = &cobra.Command{
	Use:   "upload-firmware",
	Short: "Upload firmware file to BMC",
	Long: `Upload a firmware file to the BMC via Redfish UpdateService.

Example:
  nvswitch-manager redfish upload-firmware -i 192.168.1.100 -u root -p password --file firmware.fwpkg`,
	Run: func(cmd *cobra.Command, args []string) {
		if rfFirmwareFile == "" {
			log.Fatalf("Firmware file path is required (--file)")
		}

		client, err := newRedfishClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Logout()

		fmt.Printf("Uploading firmware: %s\n", rfFirmwareFile)

		resp, err := client.UploadFirmwareByPath(rfFirmwareFile)
		if err != nil {
			log.Fatalf("Failed to upload firmware: %v", err)
		}

		fmt.Printf("Upload Response Status: %s\n", resp.Status)

		// Try to extract task URI
		taskURI, err := client.GetTaskURI(resp)
		if err == nil && taskURI != "" {
			fmt.Printf("Task URI: %s\n", taskURI)
			fmt.Printf("\nMonitor task with:\n")
			fmt.Printf("  nvswitch-manager redfish query-task -i %s -u %s -p <password> --task-id %s\n", rfBmcIP, rfUsername, taskURI)
		}
	},
}

// updateFirmwareCmd uploads firmware with apply time set to immediate
var updateFirmwareCmd = &cobra.Command{
	Use:   "update-firmware",
	Short: "Upload and apply firmware immediately",
	Long: `Upload firmware and set apply time to immediate.

Example:
  nvswitch-manager redfish update-firmware -i 192.168.1.100 -u root -p password --file firmware.fwpkg`,
	Run: func(cmd *cobra.Command, args []string) {
		if rfFirmwareFile == "" {
			log.Fatalf("Firmware file path is required (--file)")
		}

		client, err := newRedfishClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Logout()

		fmt.Printf("Setting HTTP Push URI Apply Time to Immediate...\n")
		_, err = client.SetHttpPushUriApplyTimeImmediate()
		if err != nil {
			log.Fatalf("Failed to set apply time: %v", err)
		}

		fmt.Printf("Uploading firmware: %s\n", rfFirmwareFile)
		resp, err := client.UploadFirmwareByPath(rfFirmwareFile)
		if err != nil {
			log.Fatalf("Failed to upload firmware: %v", err)
		}

		fmt.Printf("Upload Response Status: %s\n", resp.Status)

		taskURI, err := client.GetTaskURI(resp)
		if err == nil && taskURI != "" {
			fmt.Printf("Task URI: %s\n", taskURI)
			fmt.Printf("\nMonitor task with:\n")
			fmt.Printf("  nvswitch-manager redfish query-task -i %s -u %s -p <password> --task-id %s\n", rfBmcIP, rfUsername, taskURI)
		}
	},
}

var rfResetType string

// resetSystemCmd performs a ComputerSystem.Reset action on the NV-Switch
var resetSystemCmd = &cobra.Command{
	Use:   "reset-system",
	Short: "Perform a power/reset action on the NV-Switch",
	Long: `Perform a Redfish ComputerSystem.Reset action on the NV-Switch.

Supported reset types:
  ForceOff, PowerCycle, GracefulShutdown, On, ForceOn, GracefulRestart, ForceRestart

Examples:
  nvswitch-manager redfish reset-system -i 192.168.1.100 -u root -p password --reset-type PowerCycle
  nvswitch-manager redfish reset-system -i 192.168.1.100 -u root -p password --reset-type GracefulShutdown
  nvswitch-manager redfish reset-system -i 192.168.1.100 -u root -p password --reset-type ForceOff`,
	Run: func(cmd *cobra.Command, args []string) {
		resetType := resolveRedfishResetType(rfResetType)

		client, err := newRedfishClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Logout()

		fmt.Printf("Performing %s on NV-Switch at %s...\n", resetType, rfBmcIP)
		resp, err := client.ResetSystem(resetType)
		if err != nil {
			log.Fatalf("Failed to perform %s: %v", resetType, err)
		}
		util.PrintPrettyResponse(resp)
	},
}

func resolveRedfishResetType(s string) redfish.ResetType {
	switch s {
	case "ForceOff":
		return redfish.ResetForceOff
	case "PowerCycle":
		return redfish.ResetPowerCycle
	case "GracefulShutdown":
		return redfish.ResetGracefulShutdown
	case "On":
		return redfish.ResetOn
	case "ForceOn":
		return redfish.ResetForceOn
	case "GracefulRestart":
		return redfish.ResetGracefulRestart
	case "ForceRestart":
		return redfish.ResetForceRestart
	default:
		log.Fatalf("Unknown reset type %q; valid types: ForceOff, PowerCycle, GracefulShutdown, On, ForceOn, GracefulRestart, ForceRestart", s)
		return ""
	}
}

// resetBmcCmd resets the BMC
var resetBmcCmd = &cobra.Command{
	Use:   "reset-bmc",
	Short: "Reset the BMC (graceful or force)",
	Long: `Reset the BMC manager.

Examples:
  # Graceful reset (default)
  nvswitch-manager redfish reset-bmc -i 192.168.1.100 -u root -p password

  # Force reset
  nvswitch-manager redfish reset-bmc -i 192.168.1.100 -u root -p password --force`,
	Run: func(cmd *cobra.Command, args []string) {
		force, _ := cmd.Flags().GetBool("force")

		client, err := newRedfishClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Logout()

		resetType := redfish.GracefulBMCRestart
		if force {
			resetType = redfish.ForceBMCRestart
		}

		fmt.Printf("Resetting BMC at %s (%s)...\n", rfBmcIP, resetType)
		resp, err := client.ResetBMC(resetType)
		if err != nil {
			log.Fatalf("Failed to reset BMC: %v", err)
		}
		util.PrintPrettyResponse(resp)
	},
}

func init() {
	rootCmd.AddCommand(redfishCmd)

	// Add persistent flags to redfish command (inherited by all subcommands)
	redfishCmd.PersistentFlags().StringVarP(&rfBmcIP, "bmc-ip", "i", "", "BMC IP address (required)")
	redfishCmd.PersistentFlags().StringVarP(&rfUsername, "user", "u", "root", "BMC username")
	redfishCmd.PersistentFlags().StringVarP(&rfPassword, "pass", "p", "", "BMC password (required)")
	redfishCmd.PersistentFlags().BoolVar(&rfInsecure, "insecure", true, "Skip TLS verification")

	// Add subcommands
	redfishCmd.AddCommand(queryChassisCmd)
	redfishCmd.AddCommand(queryManagerCmd)
	redfishCmd.AddCommand(queryFirmwareCmd)
	redfishCmd.AddCommand(queryTaskCmd)
	redfishCmd.AddCommand(queryUpdateServiceCmd)
	redfishCmd.AddCommand(uploadFirmwareCmd)
	redfishCmd.AddCommand(updateFirmwareCmd)
	redfishCmd.AddCommand(resetSystemCmd)
	redfishCmd.AddCommand(resetBmcCmd)

	// Add command-specific flags
	queryTaskCmd.Flags().StringVar(&rfTaskID, "task-id", "", "Task ID/URI to query (required)")
	uploadFirmwareCmd.Flags().StringVar(&rfFirmwareFile, "file", "", "Path to firmware file (required)")
	resetSystemCmd.Flags().StringVar(&rfResetType, "reset-type", "PowerCycle", "Reset type: ForceOff, PowerCycle, GracefulShutdown, On, ForceOn, GracefulRestart, ForceRestart")
	updateFirmwareCmd.Flags().StringVar(&rfFirmwareFile, "file", "", "Path to firmware file (required)")
	resetBmcCmd.Flags().Bool("force", false, "Force restart instead of graceful")
}
