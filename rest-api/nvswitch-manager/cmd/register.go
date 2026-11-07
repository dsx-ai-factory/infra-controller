// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"
	"time"

	pb "github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/internal/proto/v1"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/protobuf/types/known/emptypb"
)

var (
	// Register command flags
	regServerAddr string
	regBmcMAC     string
	regBmcIP      string
	regBmcPort    int
	regBmcUser    string
	regBmcPass    string
	regNvosMAC    string
	regNvosIP     string
	regNvosPort   int
	regNvosUser   string
	regNvosPass   string
)

// registerCmd represents the register command group
var registerCmd = &cobra.Command{
	Use:   "register",
	Short: "Register NV-Switch trays with the NSM service",
	Long: `Register NV-Switch trays with the NSM service via gRPC.

The service must be running for registration to work.

Examples:
  # Register a switch
  nvswitch-manager register switch \
    --server localhost:50051 \
    --bmc-mac 00:00:00:00:00:01 --bmc-ip 192.168.1.100 --bmc-user root --bmc-pass password \
    --nvos-mac 00:00:00:00:00:02 --nvos-ip 192.168.1.101 --nvos-user admin --nvos-pass password

  # List registered switches
  nvswitch-manager register list --server localhost:50051`,
}

// registerSwitchCmd registers a single switch
var registerSwitchCmd = &cobra.Command{
	Use:   "switch",
	Short: "Register a single NV-Switch tray",
	Run: func(cmd *cobra.Command, args []string) {
		if regBmcMAC == "" || regBmcIP == "" {
			log.Fatal("BMC MAC and IP are required")
		}
		if regNvosMAC == "" || regNvosIP == "" {
			log.Fatal("NVOS MAC and IP are required")
		}

		// Connect to gRPC server
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()

		conn, err := grpc.DialContext(ctx, regServerAddr,
			grpc.WithTransportCredentials(insecure.NewCredentials()),
			grpc.WithBlock(),
		)
		if err != nil {
			log.Fatalf("Failed to connect to server %s: %v", regServerAddr, err)
		}
		defer conn.Close()

		client := pb.NewNVSwitchManagerClient(conn)

		// Create registration request
		req := &pb.RegisterNVSwitchesRequest{
			RegistrationRequests: []*pb.RegisterNVSwitchRequest{
				{
					Vendor: pb.Vendor_VENDOR_NVIDIA,
					Bmc: &pb.Subsystem{
						MacAddress: regBmcMAC,
						IpAddress:  regBmcIP,
						Port:       int32(regBmcPort),
						Credentials: &pb.Credentials{
							Username: regBmcUser,
							Password: regBmcPass,
						},
					},
					Nvos: &pb.Subsystem{
						MacAddress: regNvosMAC,
						IpAddress:  regNvosIP,
						Port:       int32(regNvosPort),
						Credentials: &pb.Credentials{
							Username: regNvosUser,
							Password: regNvosPass,
						},
					},
				},
			},
		}

		resp, err := client.RegisterNVSwitches(ctx, req)
		if err != nil {
			log.Fatalf("Failed to register switch: %v", err)
		}

		for _, r := range resp.Responses {
			if r.Status == pb.StatusCode_SUCCESS {
				fmt.Printf("Switch registered successfully:\n")
				fmt.Printf("  UUID:   %s\n", r.Uuid)
				fmt.Printf("  Is New: %v\n", r.IsNew)
			} else {
				fmt.Printf("Registration failed:\n")
				fmt.Printf("  Status: %s\n", r.Status.String())
				fmt.Printf("  Error:  %s\n", r.Error)
			}
		}
	},
}

// registerListCmd lists registered switches
var registerListCmd = &cobra.Command{
	Use:   "list",
	Short: "List registered NV-Switch trays",
	Run: func(cmd *cobra.Command, args []string) {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()

		conn, err := grpc.DialContext(ctx, regServerAddr,
			grpc.WithTransportCredentials(insecure.NewCredentials()),
			grpc.WithBlock(),
		)
		if err != nil {
			log.Fatalf("Failed to connect to server %s: %v", regServerAddr, err)
		}
		defer conn.Close()

		client := pb.NewNVSwitchManagerClient(conn)

		resp, err := client.GetNVSwitches(ctx, &pb.NVSwitchRequest{})
		if err != nil {
			log.Fatalf("Failed to list switches: %v", err)
		}

		if len(resp.Nvswitches) == 0 {
			fmt.Println("No switches registered")
			return
		}

		fmt.Printf("Registered NV-Switches (%d):\n\n", len(resp.Nvswitches))
		for _, sw := range resp.Nvswitches {
			fmt.Printf("UUID: %s\n", sw.Uuid)
			fmt.Printf("  Vendor: %s\n", sw.Vendor.String())
			if sw.Bmc != nil {
				fmt.Printf("  BMC:\n")
				fmt.Printf("    IP:       %s\n", sw.Bmc.IpAddress)
				fmt.Printf("    Port:     %d\n", sw.Bmc.Port)
				fmt.Printf("    MAC:      %s\n", sw.Bmc.MacAddress)
				fmt.Printf("    Firmware: %s\n", sw.Bmc.FirmwareVersion)
				fmt.Printf("    Serial:   %s\n", sw.Bmc.SerialNumber)
			}
			if sw.Nvos != nil {
				fmt.Printf("  NVOS:\n")
				fmt.Printf("    IP:      %s\n", sw.Nvos.IpAddress)
				fmt.Printf("    Port:    %d\n", sw.Nvos.Port)
				fmt.Printf("    MAC:     %s\n", sw.Nvos.MacAddress)
				fmt.Printf("    Version: %s\n", sw.Nvos.Version)
			}
			if sw.Chassis != nil {
				fmt.Printf("  Chassis:\n")
				fmt.Printf("    Model:  %s\n", sw.Chassis.Model)
				fmt.Printf("    Serial: %s\n", sw.Chassis.SerialNumber)
			}
			fmt.Println()
		}
	},
}

// registerBundlesCmd lists available firmware bundles
var registerBundlesCmd = &cobra.Command{
	Use:   "bundles",
	Short: "List available firmware bundles",
	Run: func(cmd *cobra.Command, args []string) {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()

		conn, err := grpc.DialContext(ctx, regServerAddr,
			grpc.WithTransportCredentials(insecure.NewCredentials()),
			grpc.WithBlock(),
		)
		if err != nil {
			log.Fatalf("Failed to connect to server %s: %v", regServerAddr, err)
		}
		defer conn.Close()

		client := pb.NewNVSwitchManagerClient(conn)

		resp, err := client.ListBundles(ctx, &emptypb.Empty{})
		if err != nil {
			log.Fatalf("Failed to list bundles: %v", err)
		}

		if len(resp.Bundles) == 0 {
			fmt.Println("No firmware bundles available")
			return
		}

		fmt.Printf("Available Firmware Bundles (%d):\n\n", len(resp.Bundles))
		for _, b := range resp.Bundles {
			fmt.Printf("Version: %s\n", b.Version)
			fmt.Printf("  Description: %s\n", b.Description)
			fmt.Printf("  Components:\n")
			for _, c := range b.Components {
				fmt.Printf("    - %s: %s (%s)\n", c.Name, c.Version, c.Strategy)
			}
			fmt.Println()
		}
	},
}

func init() {
	rootCmd.AddCommand(registerCmd)

	// Common flags
	registerCmd.PersistentFlags().StringVar(&regServerAddr, "server", "localhost:50051", "NSM gRPC server address")

	// Switch registration flags
	registerSwitchCmd.Flags().StringVar(&regBmcMAC, "bmc-mac", "", "BMC MAC address (required)")
	registerSwitchCmd.Flags().StringVar(&regBmcIP, "bmc-ip", "", "BMC IP address (required)")
	registerSwitchCmd.Flags().IntVar(&regBmcPort, "bmc-port", 0, "BMC port (0 = default 443)")
	registerSwitchCmd.Flags().StringVar(&regBmcUser, "bmc-user", "root", "BMC username")
	registerSwitchCmd.Flags().StringVar(&regBmcPass, "bmc-pass", "", "BMC password (required)")
	registerSwitchCmd.Flags().StringVar(&regNvosMAC, "nvos-mac", "", "NVOS MAC address (required)")
	registerSwitchCmd.Flags().StringVar(&regNvosIP, "nvos-ip", "", "NVOS IP address (required)")
	registerSwitchCmd.Flags().IntVar(&regNvosPort, "nvos-port", 0, "NVOS port (0 = default 22)")
	registerSwitchCmd.Flags().StringVar(&regNvosUser, "nvos-user", "admin", "NVOS username")
	registerSwitchCmd.Flags().StringVar(&regNvosPass, "nvos-pass", "", "NVOS password (required)")

	registerCmd.AddCommand(registerSwitchCmd)
	registerCmd.AddCommand(registerListCmd)
	registerCmd.AddCommand(registerBundlesCmd)
}
