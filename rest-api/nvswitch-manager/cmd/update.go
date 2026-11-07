// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"
	"strings"
	"time"

	pb "github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/internal/proto/v1"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

var (
	// Update command flags
	updateServerAddr string
	updateSwitchUUID string
	updateBundleVer  string
	updateComponents []string
	updateID         string
)

// updateCmd represents the update command group
var updateCmd = &cobra.Command{
	Use:   "update",
	Short: "Manage firmware updates via the NSM service",
	Long: `Queue, monitor, and manage firmware updates via the NSM gRPC service.

Examples:
  # Queue a full bundle update for a switch
  nvswitch-manager update queue --switch-uuid <uuid> --bundle 1.3.1

  # Queue update for specific components only
  nvswitch-manager update queue --switch-uuid <uuid> --bundle 1.3.1 --components bmc,cpld

  # Check status of an update
  nvswitch-manager update status --update-id <id>

  # List all updates for a switch
  nvswitch-manager update list --switch-uuid <uuid>

  # Cancel an update
  nvswitch-manager update cancel --update-id <id>`,
}

// Helper to create gRPC client
func createUpdateClient() (pb.NVSwitchManagerClient, *grpc.ClientConn, context.Context, context.CancelFunc) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)

	conn, err := grpc.DialContext(ctx, updateServerAddr,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithBlock(),
	)
	if err != nil {
		cancel()
		log.Fatalf("Failed to connect to server %s: %v", updateServerAddr, err)
	}

	return pb.NewNVSwitchManagerClient(conn), conn, ctx, cancel
}

// updateQueueCmd queues a firmware update
var updateQueueCmd = &cobra.Command{
	Use:   "queue",
	Short: "Queue a firmware update for a switch",
	Long: `Queue a firmware update for a switch. If no components are specified, 
all components in the bundle will be updated in sequence.

Components: bmc, cpld, bios, nvos`,
	Run: func(cmd *cobra.Command, args []string) {
		if updateSwitchUUID == "" {
			log.Fatal("--switch-uuid is required")
		}
		if updateBundleVer == "" {
			log.Fatal("--bundle is required")
		}

		client, conn, ctx, cancel := createUpdateClient()
		defer conn.Close()
		defer cancel()

		// Parse components
		var components []pb.NVSwitchComponent
		for _, c := range updateComponents {
			switch strings.ToLower(strings.TrimSpace(c)) {
			case "bmc":
				components = append(components, pb.NVSwitchComponent_NVSWITCH_COMPONENT_BMC)
			case "cpld":
				components = append(components, pb.NVSwitchComponent_NVSWITCH_COMPONENT_CPLD)
			case "bios":
				components = append(components, pb.NVSwitchComponent_NVSWITCH_COMPONENT_BIOS)
			case "nvos":
				components = append(components, pb.NVSwitchComponent_NVSWITCH_COMPONENT_NVOS)
			default:
				log.Fatalf("Unknown component: %s (valid: bmc, cpld, bios, nvos)", c)
			}
		}

		req := &pb.QueueUpdateRequest{
			SwitchUuid:    updateSwitchUUID,
			BundleVersion: updateBundleVer,
			Components:    components,
		}

		resp, err := client.QueueUpdate(ctx, req)
		if err != nil {
			log.Fatalf("Failed to queue update: %v", err)
		}

		if len(resp.Updates) == 0 {
			fmt.Println("No updates queued")
			return
		}

		fmt.Printf("Queued %d update(s):\n\n", len(resp.Updates))
		for _, u := range resp.Updates {
			printUpdateInfo(u)
		}
	},
}

// updateStatusCmd gets status of an update
var updateStatusCmd = &cobra.Command{
	Use:   "status",
	Short: "Get status of a firmware update",
	Run: func(cmd *cobra.Command, args []string) {
		if updateID == "" {
			log.Fatal("--update-id is required")
		}

		client, conn, ctx, cancel := createUpdateClient()
		defer conn.Close()
		defer cancel()

		resp, err := client.GetUpdate(ctx, &pb.GetUpdateRequest{UpdateId: updateID})
		if err != nil {
			log.Fatalf("Failed to get update: %v", err)
		}

		printUpdateInfo(resp.Update)
	},
}

// updateListCmd lists updates for a switch
var updateListCmd = &cobra.Command{
	Use:   "list",
	Short: "List firmware updates for a switch",
	Run: func(cmd *cobra.Command, args []string) {
		if updateSwitchUUID == "" {
			log.Fatal("--switch-uuid is required")
		}

		client, conn, ctx, cancel := createUpdateClient()
		defer conn.Close()
		defer cancel()

		resp, err := client.GetUpdatesForSwitch(ctx, &pb.GetUpdatesForSwitchRequest{
			SwitchUuid: updateSwitchUUID,
		})
		if err != nil {
			log.Fatalf("Failed to list updates: %v", err)
		}

		if len(resp.Updates) == 0 {
			fmt.Println("No updates found for this switch")
			return
		}

		fmt.Printf("Updates for switch %s (%d):\n\n", updateSwitchUUID, len(resp.Updates))
		for _, u := range resp.Updates {
			printUpdateInfo(u)
		}
	},
}

// updateCancelCmd cancels an update
var updateCancelCmd = &cobra.Command{
	Use:   "cancel",
	Short: "Cancel a firmware update",
	Run: func(cmd *cobra.Command, args []string) {
		if updateID == "" {
			log.Fatal("--update-id is required")
		}

		client, conn, ctx, cancel := createUpdateClient()
		defer conn.Close()
		defer cancel()

		resp, err := client.CancelUpdate(ctx, &pb.CancelUpdateRequest{UpdateId: updateID})
		if err != nil {
			log.Fatalf("Failed to cancel update: %v", err)
		}

		if resp.Success {
			fmt.Printf("Update cancelled: %s\n", resp.Message)
		} else {
			fmt.Printf("Failed to cancel: %s\n", resp.Message)
		}
	},
}

// printUpdateInfo prints formatted update information
func printUpdateInfo(u *pb.FirmwareUpdateInfo) {
	fmt.Printf("Update ID: %s\n", u.Id)
	fmt.Printf("  Switch:    %s\n", u.SwitchUuid)
	fmt.Printf("  Component: %s\n", componentToString(u.Component))
	fmt.Printf("  Bundle:    %s\n", u.BundleVersion)
	fmt.Printf("  Strategy:  %s\n", strategyToString(u.Strategy))
	fmt.Printf("  State:     %s\n", stateToString(u.State))
	fmt.Printf("  Version:   %s -> %s", u.VersionFrom, u.VersionTo)
	if u.VersionActual != "" {
		fmt.Printf(" (actual: %s)", u.VersionActual)
	}
	fmt.Println()
	if u.ErrorMessage != "" {
		fmt.Printf("  Error:     %s\n", u.ErrorMessage)
	}
	if u.BundleUpdateId != "" {
		fmt.Printf("  Bundle Update: %s (seq: %d)\n", u.BundleUpdateId, u.SequenceOrder)
	}
	if u.PredecessorId != "" {
		fmt.Printf("  Predecessor:   %s\n", u.PredecessorId)
	}
	fmt.Printf("  Created:   %s\n", u.CreatedAt.AsTime().Local().Format(time.RFC3339))
	fmt.Printf("  Updated:   %s\n", u.UpdatedAt.AsTime().Local().Format(time.RFC3339))
	fmt.Println()
}

func componentToString(c pb.NVSwitchComponent) string {
	switch c {
	case pb.NVSwitchComponent_NVSWITCH_COMPONENT_BMC:
		return "BMC"
	case pb.NVSwitchComponent_NVSWITCH_COMPONENT_CPLD:
		return "CPLD"
	case pb.NVSwitchComponent_NVSWITCH_COMPONENT_BIOS:
		return "BIOS"
	case pb.NVSwitchComponent_NVSWITCH_COMPONENT_NVOS:
		return "NVOS"
	default:
		return "UNKNOWN"
	}
}

func strategyToString(s pb.UpdateStrategy) string {
	switch s {
	case pb.UpdateStrategy_UPDATE_STRATEGY_REDFISH:
		return "Redfish"
	case pb.UpdateStrategy_UPDATE_STRATEGY_SSH:
		return "SSH"
	case pb.UpdateStrategy_UPDATE_STRATEGY_SCRIPT:
		return "Script"
	default:
		return "Unknown"
	}
}

func stateToString(s pb.UpdateState) string {
	switch s {
	case pb.UpdateState_UPDATE_STATE_QUEUED:
		return "Queued"
	case pb.UpdateState_UPDATE_STATE_POWER_CYCLE:
		return "Power Cycling"
	case pb.UpdateState_UPDATE_STATE_WAIT_REACHABLE:
		return "Waiting for Reachability"
	case pb.UpdateState_UPDATE_STATE_COPY:
		return "Copying"
	case pb.UpdateState_UPDATE_STATE_UPLOAD:
		return "Uploading"
	case pb.UpdateState_UPDATE_STATE_INSTALL:
		return "Installing"
	case pb.UpdateState_UPDATE_STATE_POLL_COMPLETION:
		return "Polling Completion"
	case pb.UpdateState_UPDATE_STATE_VERIFY:
		return "Verifying"
	case pb.UpdateState_UPDATE_STATE_CLEANUP:
		return "Cleaning Up"
	case pb.UpdateState_UPDATE_STATE_COMPLETED:
		return "Completed"
	case pb.UpdateState_UPDATE_STATE_FAILED:
		return "Failed"
	case pb.UpdateState_UPDATE_STATE_CANCELLED:
		return "Cancelled"
	default:
		return "Unknown"
	}
}

func init() {
	rootCmd.AddCommand(updateCmd)

	// Common flags
	updateCmd.PersistentFlags().StringVar(&updateServerAddr, "server", "localhost:50051", "NSM gRPC server address")

	// Queue command flags
	updateQueueCmd.Flags().StringVar(&updateSwitchUUID, "switch-uuid", "", "Switch UUID (required)")
	updateQueueCmd.Flags().StringVar(&updateBundleVer, "bundle", "", "Firmware bundle version (required)")
	updateQueueCmd.Flags().StringSliceVar(&updateComponents, "components", nil, "Components to update (comma-separated: bmc,cpld,bios,nvos)")

	// Status command flags
	updateStatusCmd.Flags().StringVar(&updateID, "update-id", "", "Update ID (required)")

	// List command flags
	updateListCmd.Flags().StringVar(&updateSwitchUUID, "switch-uuid", "", "Switch UUID (required)")

	// Cancel command flags
	updateCancelCmd.Flags().StringVar(&updateID, "update-id", "", "Update ID (required)")

	updateCmd.AddCommand(updateQueueCmd)
	updateCmd.AddCommand(updateStatusCmd)
	updateCmd.AddCommand(updateListCmd)
	updateCmd.AddCommand(updateCancelCmd)
}
