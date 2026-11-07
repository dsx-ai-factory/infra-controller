// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/secretstring"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvos"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/sshclient"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
)

var (
	// Common SSH flags
	sshIP         string
	sshPort       int
	sshUsername   string
	sshPassword   string
	sshCommand    string
	sshLocalPath  string
	sshRemotePath string
)

// sshCmd represents the ssh command group
var sshCmd = &cobra.Command{
	Use:   "ssh",
	Short: "Interact with NV-Switch NVOS via SSH",
	Long: `Interact with an NV-Switch NVOS subsystem via SSH.

Examples:
  # Execute a command on NVOS
  nvswitch-manager ssh exec -i 192.168.1.101 -u admin -p password --cmd "nv show system"

  # Copy a file to NVOS
  nvswitch-manager ssh copy -i 192.168.1.101 -u admin -p password --local firmware.bin --remote /tmp/firmware.bin

  # Get system info
  nvswitch-manager ssh info -i 192.168.1.101 -u admin -p password`,
}

// Helper function to create an SSH client
func newSSHClient() (*sshclient.NVOSClient, error) {
	if sshIP == "" {
		return nil, fmt.Errorf("NVOS IP address is required (--ip or -i)")
	}
	if sshPassword == "" {
		return nil, fmt.Errorf("password is required (--pass or -p)")
	}

	cred := &credential.Credential{
		User:     sshUsername,
		Password: secretstring.New(sshPassword),
	}

	n, err := nvos.New("00:00:00:00:00:00", sshIP, cred)
	if err != nil {
		return nil, fmt.Errorf("failed to create NVOS: %v", err)
	}

	client, err := sshclient.NewWithPort(context.Background(), n, sshPort)
	if err != nil {
		return nil, fmt.Errorf("failed to create SSH client: %v", err)
	}

	return client, nil
}

// sshExecCmd executes a command on NVOS
var sshExecCmd = &cobra.Command{
	Use:   "exec",
	Short: "Execute a command on NVOS",
	Long: `Execute a command on the NV-Switch NVOS via SSH.

Examples:
  nvswitch-manager ssh exec -i 192.168.1.101 -u admin -p password --cmd "nv show system"
  nvswitch-manager ssh exec -i 192.168.1.101 -u admin -p password --cmd "nv show platform firmware"`,
	Run: func(cmd *cobra.Command, args []string) {
		if sshCommand == "" {
			log.Fatalf("Command is required (--cmd)")
		}

		client, err := newSSHClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Close()

		fmt.Printf("Executing command on %s: %s\n", sshIP, sshCommand)
		fmt.Println(strings.Repeat("-", 60))

		output, err := client.RunCommand(sshCommand)
		if err != nil {
			log.Fatalf("Command failed: %v", err)
		}

		fmt.Println(output)
	},
}

// sshCopyCmd copies a file to NVOS
var sshCopyCmd = &cobra.Command{
	Use:   "copy",
	Short: "Copy a file to NVOS",
	Long: `Copy a local file to the NV-Switch NVOS via SCP.

Example:
  nvswitch-manager ssh copy -i 192.168.1.101 -u admin -p password --local firmware.bin --remote /tmp/firmware.bin`,
	Run: func(cmd *cobra.Command, args []string) {
		if sshLocalPath == "" {
			log.Fatalf("Local file path is required (--local)")
		}
		if sshRemotePath == "" {
			log.Fatalf("Remote file path is required (--remote)")
		}

		client, err := newSSHClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Close()

		fmt.Printf("Copying %s to %s:%s\n", sshLocalPath, sshIP, sshRemotePath)

		err = client.CopyFile(sshLocalPath, sshRemotePath)
		if err != nil {
			log.Fatalf("Failed to copy file: %v", err)
		}

		fmt.Println("File copied successfully")
	},
}

// sshInfoCmd gets system information from NVOS
var sshInfoCmd = &cobra.Command{
	Use:   "info",
	Short: "Get system information from NVOS",
	Long: `Get system information from the NV-Switch NVOS via SSH.

Example:
  nvswitch-manager ssh info -i 192.168.1.101 -u admin -p password`,
	Run: func(cmd *cobra.Command, args []string) {
		client, err := newSSHClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Close()

		fmt.Printf("NVOS System Information (%s)\n", sshIP)
		fmt.Println(strings.Repeat("=", 60))

		// Get hostname
		output, err := client.RunCommand("hostname")
		if err == nil {
			fmt.Printf("Hostname: %s", output)
		}

		// Get NVOS version
		output, err = client.RunCommand("nv show system version 2>/dev/null || cat /etc/os-release 2>/dev/null | head -5")
		if err == nil {
			fmt.Printf("\nVersion Info:\n%s", output)
		}

		// Get uptime
		output, err = client.RunCommand("uptime")
		if err == nil {
			fmt.Printf("\nUptime: %s", output)
		}
	},
}

// sshFirmwareCmd gets firmware information from NVOS
var sshFirmwareCmd = &cobra.Command{
	Use:   "firmware",
	Short: "Get firmware information from NVOS",
	Long: `Get firmware information from the NV-Switch NVOS via SSH.

Example:
  nvswitch-manager ssh firmware -i 192.168.1.101 -u admin -p password`,
	Run: func(cmd *cobra.Command, args []string) {
		client, err := newSSHClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Close()

		fmt.Printf("NVOS Firmware Information (%s)\n", sshIP)
		fmt.Println(strings.Repeat("=", 60))

		// Try NVOS command first
		output, err := client.RunCommand("nv show platform firmware 2>/dev/null")
		if err == nil && output != "" {
			fmt.Println(output)
			return
		}

		// Fallback to checking common firmware locations
		output, err = client.RunCommand("cat /etc/mlnx-release 2>/dev/null || echo 'N/A'")
		if err == nil {
			fmt.Printf("MLNX Release: %s", output)
		}
	},
}

// sshTestCmd tests SSH connectivity
var sshTestCmd = &cobra.Command{
	Use:   "test",
	Short: "Test SSH connectivity to NVOS",
	Long: `Test SSH connectivity to the NV-Switch NVOS.

Example:
  nvswitch-manager ssh test -i 192.168.1.101 -u admin -p password
  nvswitch-manager ssh test -i 127.0.0.1 --port 2222 -u admin -p password`,
	Run: func(cmd *cobra.Command, args []string) {
		fmt.Printf("Testing SSH connection to %s@%s:%d...\n", sshUsername, sshIP, sshPort)

		client, err := newSSHClient()
		if err != nil {
			log.Fatalf("Connection failed: %v", err)
		}
		defer client.Close()

		// Run a simple command to verify
		output, err := client.RunCommand("echo 'SSH connection successful'")
		if err != nil {
			log.Fatalf("Command execution failed: %v", err)
		}

		fmt.Printf("✓ %s", output)
		fmt.Printf("✓ SSH connection to %s:%d established successfully\n", sshIP, sshPort)
	},
}

// sshCpldCmd gets CPLD information from NVOS
var sshCpldCmd = &cobra.Command{
	Use:   "cpld",
	Short: "Get CPLD information from NVOS",
	Long: `Get CPLD firmware information from the NV-Switch NVOS via SSH.

Example:
  nvswitch-manager ssh cpld -i 192.168.1.101 -u admin -p password`,
	Run: func(cmd *cobra.Command, args []string) {
		client, err := newSSHClient()
		if err != nil {
			log.Fatalf("Error: %v", err)
		}
		defer client.Close()

		fmt.Printf("CPLD Information (%s)\n", sshIP)
		fmt.Println(strings.Repeat("=", 60))

		// Try to get CPLD info
		output, err := client.RunCommand("nv show platform firmware cpld 2>/dev/null || nv show platform cpld 2>/dev/null")
		if err == nil && output != "" {
			fmt.Println(output)
			return
		}

		fmt.Println("Could not retrieve CPLD information")
	},
}

func init() {
	rootCmd.AddCommand(sshCmd)

	// Add persistent flags to ssh command (inherited by all subcommands)
	sshCmd.PersistentFlags().StringVarP(&sshIP, "ip", "i", "", "NVOS IP address (required)")
	sshCmd.PersistentFlags().IntVar(&sshPort, "port", 22, "SSH port (default 22)")
	sshCmd.PersistentFlags().StringVarP(&sshUsername, "user", "u", "admin", "SSH username")
	sshCmd.PersistentFlags().StringVarP(&sshPassword, "pass", "p", "", "SSH password (required)")

	// Add subcommands
	sshCmd.AddCommand(sshExecCmd)
	sshCmd.AddCommand(sshCopyCmd)
	sshCmd.AddCommand(sshInfoCmd)
	sshCmd.AddCommand(sshFirmwareCmd)
	sshCmd.AddCommand(sshTestCmd)
	sshCmd.AddCommand(sshCpldCmd)

	// Add command-specific flags
	sshExecCmd.Flags().StringVar(&sshCommand, "cmd", "", "Command to execute (required)")
	sshCopyCmd.Flags().StringVar(&sshLocalPath, "local", "", "Local file path (required)")
	sshCopyCmd.Flags().StringVar(&sshRemotePath, "remote", "", "Remote file path (required)")
}
