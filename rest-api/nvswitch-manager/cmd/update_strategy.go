// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/secretstring"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/firmwaremanager"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvos"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/redfish"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/sshclient"

	"github.com/google/uuid"
	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
)

var (
	// Common update-strategy flags
	usBmcIP        string
	usBmcPort      int
	usBmcUsername  string
	usBmcPassword  string
	usNvosIP       string
	usNvosPort     int
	usNvosUsername string
	usNvosPassword string
	usFirmwareFile string
	usTaskID       string
	usComponent    string
	usRemoteDir    string
	usScriptPath   string
)

// updateStrategyCmd represents the update-strategy command group
var updateStrategyCmd = &cobra.Command{
	Use:   "update-strategy",
	Short: "Test firmware update strategies (redfish, ssh, script)",
	Long: `Test firmware update strategies used by the firmware manager.

This command allows testing individual steps of each update strategy:
  - redfish: BMC/BIOS firmware updates via Redfish API
  - ssh: CPLD/NVOS firmware updates via SSH
  - script: Legacy script-based updates

Examples:
  # Test Redfish strategy - upload firmware
  nvswitch-manager update-strategy redfish upload --bmc-ip 192.168.1.100 --bmc-pass password --file firmware.fwpkg

  # Test SSH strategy - copy firmware to switch
  nvswitch-manager update-strategy ssh copy --nvos-ip 192.168.1.101 --nvos-pass password --file firmware.bin

  # Test Script strategy - run update script
  nvswitch-manager update-strategy script run --bmc-ip 192.168.1.100 --nvos-ip 192.168.1.101 --script /path/to/script.sh`,
}

// ============================================================================
// REDFISH STRATEGY SUBCOMMANDS
// ============================================================================

var redfishStrategyCmd = &cobra.Command{
	Use:   "redfish",
	Short: "Test Redfish-based firmware update strategy",
	Long:  `Test Redfish-based firmware update operations used for BMC/BIOS updates.`,
}

var usSkipApplyTime bool

var redfishUploadCmd = &cobra.Command{
	Use:   "upload",
	Short: "Upload firmware via Redfish UpdateService",
	Long: `Upload firmware file to BMC via Redfish UpdateService.

By default, attempts to set apply time to Immediate before uploading.
Use --skip-apply-time if your BMC doesn't support this or already has it set.

Example:
  nvswitch-manager update-strategy redfish upload --bmc-ip 192.168.1.100 -P password --file firmware.fwpkg
  nvswitch-manager update-strategy redfish upload --bmc-ip 192.168.1.100 -P password --file firmware.fwpkg --skip-apply-time`,
	Run: func(cmd *cobra.Command, args []string) {
		if usBmcIP == "" || usBmcPassword == "" {
			log.Fatal("BMC IP and password are required")
		}
		if usFirmwareFile == "" {
			log.Fatal("Firmware file is required (--file)")
		}

		// Verify file exists
		if _, err := os.Stat(usFirmwareFile); err != nil {
			log.Fatalf("Firmware file not found: %v", err)
		}

		tray := createTestTray()
		ctx := context.Background()
		client, err := redfish.New(ctx, tray.BMC, true)
		if err != nil {
			log.Fatalf("Failed to create Redfish client: %v", err)
		}
		defer client.Logout()

		// Ensure apply time is set to Immediate (checks first, only sets if needed)
		if !usSkipApplyTime {
			fmt.Printf("Checking HTTP Push URI Apply Time...\n")
			if err = client.EnsureHttpPushUriApplyTimeImmediate(); err != nil {
				log.Fatalf("Failed to ensure apply time is Immediate: %v", err)
			}
			fmt.Printf("Apply time is Immediate\n")
		}

		fmt.Printf("Uploading firmware: %s\n", usFirmwareFile)
		resp, err := client.UpdateFirmwareByPath(usFirmwareFile)
		if err != nil {
			log.Fatalf("Failed to upload firmware: %v", err)
		}

		fmt.Printf("Response Status: %s\n", resp.Status)

		if resp.StatusCode == http.StatusAccepted || resp.StatusCode == http.StatusOK {
			taskURI, err := client.GetTaskURI(resp)
			if err == nil && taskURI != "" {
				fmt.Printf("Task URI: %s\n", taskURI)
				fmt.Printf("\nMonitor with:\n")
				fmt.Printf("  nvswitch-manager update-strategy redfish poll-task --bmc-ip %s -u %s -p <password> --task-id %s\n",
					usBmcIP, usBmcUsername, taskURI)
			}
		}
	},
}

var redfishPollTaskCmd = &cobra.Command{
	Use:   "poll-task",
	Short: "Poll Redfish task status until completion",
	Long: `Poll a Redfish task until it completes, fails, or times out.

Example:
  nvswitch-manager update-strategy redfish poll-task --bmc-ip 192.168.1.100 -p password --task-id /redfish/v1/TaskService/Tasks/1`,
	Run: func(cmd *cobra.Command, args []string) {
		if usBmcIP == "" || usBmcPassword == "" {
			log.Fatal("BMC IP and password are required")
		}
		if usTaskID == "" {
			log.Fatal("Task ID is required (--task-id)")
		}

		tray := createTestTray()
		ctx := context.Background()

		fmt.Printf("Polling task: %s\n", usTaskID)
		fmt.Printf("Press Ctrl+C to stop\n\n")

		startTime := time.Now()
		pollInterval := 5 * time.Second
		timeout := 30 * time.Minute

		for {
			client, err := redfish.New(ctx, tray.BMC, true)
			if err != nil {
				fmt.Printf("[%s] Connection error: %v (retrying...)\n", time.Since(startTime).Round(time.Second), err)
				time.Sleep(pollInterval)
				continue
			}

			state, percentComplete, err := client.GetTaskStatus(usTaskID)
			client.Logout()

			if err != nil {
				fmt.Printf("[%s] Poll error: %v (retrying...)\n", time.Since(startTime).Round(time.Second), err)
				time.Sleep(pollInterval)
				continue
			}

			fmt.Printf("[%s] State: %-12s Progress: %d%%\n", time.Since(startTime).Round(time.Second), state, percentComplete)

			switch state {
			case "Completed":
				fmt.Printf("\n✓ Task completed successfully!\n")
				return
			case "Exception", "Killed", "Cancelled":
				fmt.Printf("\n✗ Task failed with state: %s\n", state)
				return
			}

			if time.Since(startTime) > timeout {
				fmt.Printf("\n✗ Timeout after %v\n", timeout)
				return
			}

			time.Sleep(pollInterval)
		}
	},
}

var redfishGetVersionCmd = &cobra.Command{
	Use:   "get-version",
	Short: "Get current firmware version via Redfish",
	Long: `Query the current firmware version for a component via Redfish.

Example:
  nvswitch-manager update-strategy redfish get-version --bmc-ip 192.168.1.100 -p password --component BMC`,
	Run: func(cmd *cobra.Command, args []string) {
		if usBmcIP == "" || usBmcPassword == "" {
			log.Fatal("BMC IP and password are required")
		}

		component := nvswitch.Component(strings.ToUpper(usComponent))
		if !component.IsValid() {
			log.Fatalf("Invalid component: %s (valid: BMC, BIOS, CPLD, NVOS)", usComponent)
		}

		tray := createTestTray()
		ctx := context.Background()

		strategy := firmwaremanager.NewRedfishStrategy(nil)
		version, err := strategy.GetCurrentVersion(ctx, tray, component)
		if err != nil {
			log.Fatalf("Failed to get version: %v", err)
		}

		fmt.Printf("Component: %s\n", component)
		fmt.Printf("Version:   %s\n", version)
	},
}

// ============================================================================
// SSH STRATEGY SUBCOMMANDS
// ============================================================================

var sshStrategyCmd = &cobra.Command{
	Use:   "ssh",
	Short: "Test SSH-based firmware update strategy",
	Long:  `Test SSH-based firmware update operations used for CPLD/NVOS updates.`,
}

var usSSHCopyCmd = &cobra.Command{
	Use:   "copy",
	Short: "Copy firmware file to switch via SCP",
	Long: `Copy a firmware file to the NV-Switch via SCP.

Example:
  nvswitch-manager update-strategy ssh copy --nvos-ip 192.168.1.101 -p password --file firmware.bin`,
	Run: func(cmd *cobra.Command, args []string) {
		if usNvosIP == "" || usNvosPassword == "" {
			log.Fatal("NVOS IP and password are required")
		}
		if usFirmwareFile == "" {
			log.Fatal("Firmware file is required (--file)")
		}

		// Verify file exists
		if _, err := os.Stat(usFirmwareFile); err != nil {
			log.Fatalf("Firmware file not found: %v", err)
		}

		// Check sshpass is available
		if _, err := exec.LookPath("sshpass"); err != nil {
			log.Fatal("sshpass not found in PATH: required for SCP operations")
		}

		fileName := filepath.Base(usFirmwareFile)
		remotePath := filepath.Join(usRemoteDir, fileName)
		targetAddr := sshclient.SCPDestination(usNvosUsername, usNvosIP, remotePath)

		fmt.Printf("Copying %s to %s\n", usFirmwareFile, targetAddr)

		// Build SCP command
		scpArgs := []string{
			"-p", usNvosPassword,
			"scp",
			"-o", "StrictHostKeyChecking=no",
			"-o", "UserKnownHostsFile=/dev/null",
			"-o", "ConnectTimeout=30",
		}
		if usNvosPort != 22 {
			scpArgs = append(scpArgs, "-P", fmt.Sprintf("%d", usNvosPort))
		}
		scpArgs = append(scpArgs, usFirmwareFile, targetAddr)

		scpCmd := exec.Command("sshpass", scpArgs...)
		scpCmd.Stdout = os.Stdout
		scpCmd.Stderr = os.Stderr

		startTime := time.Now()
		if err := scpCmd.Run(); err != nil {
			log.Fatalf("SCP failed: %v", err)
		}

		fmt.Printf("\n✓ File copied successfully in %v\n", time.Since(startTime).Round(time.Millisecond))
		fmt.Printf("Remote path: %s\n", remotePath)
	},
}

var sshFetchCmd = &cobra.Command{
	Use:   "fetch",
	Short: "Fetch firmware into NVOS via 'nv action fetch'",
	Long: `Fetch (import) firmware into NVOS using 'nv action fetch' command.

Example:
  nvswitch-manager update-strategy ssh fetch --nvos-ip 192.168.1.101 -p password --file /home/admin/firmware.bin --component CPLD`,
	Run: func(cmd *cobra.Command, args []string) {
		if usNvosIP == "" || usNvosPassword == "" {
			log.Fatal("NVOS IP and password are required")
		}
		if usFirmwareFile == "" {
			log.Fatal("Remote firmware file path is required (--file)")
		}

		component := nvswitch.Component(strings.ToUpper(usComponent))
		if component != nvswitch.CPLD && component != nvswitch.NVOS {
			log.Fatalf("Invalid component for SSH fetch: %s (valid: CPLD, NVOS)", usComponent)
		}

		client, err := createSSHClient()
		if err != nil {
			log.Fatalf("Failed to create SSH client: %v", err)
		}
		defer client.Close()

		var fetchCmd string
		switch component {
		case nvswitch.CPLD:
			fetchCmd = fmt.Sprintf("nv action fetch platform firmware CPLD1 file://%s", usFirmwareFile)
		case nvswitch.NVOS:
			fetchCmd = fmt.Sprintf("nv action fetch system image file://%s", usFirmwareFile)
		}

		fmt.Printf("Executing: %s\n", fetchCmd)
		output, err := client.RunCommand(fetchCmd)
		if err != nil {
			log.Fatalf("Fetch failed: %v", err)
		}

		fmt.Printf("\nOutput:\n%s\n", output)
		fmt.Printf("✓ Fetch completed\n")
	},
}

var sshInstallCmd = &cobra.Command{
	Use:   "install",
	Short: "Install firmware via 'nv action install'",
	Long: `Install fetched firmware using 'nv action install' command.

WARNING: For NVOS, this will trigger a reboot!

Example:
  nvswitch-manager update-strategy ssh install --nvos-ip 192.168.1.101 -p password --file firmware.bin --component CPLD`,
	Run: func(cmd *cobra.Command, args []string) {
		if usNvosIP == "" || usNvosPassword == "" {
			log.Fatal("NVOS IP and password are required")
		}
		if usFirmwareFile == "" {
			log.Fatal("Firmware filename is required (--file)")
		}

		component := nvswitch.Component(strings.ToUpper(usComponent))
		if component != nvswitch.CPLD && component != nvswitch.NVOS {
			log.Fatalf("Invalid component for SSH install: %s (valid: CPLD, NVOS)", usComponent)
		}

		client, err := createSSHClient()
		if err != nil {
			log.Fatalf("Failed to create SSH client: %v", err)
		}
		defer client.Close()

		fileName := filepath.Base(usFirmwareFile)

		var installCmd string
		switch component {
		case nvswitch.CPLD:
			installCmd = fmt.Sprintf("nv action install platform firmware CPLD1 files \"%s\"", fileName)
		case nvswitch.NVOS:
			installCmd = fmt.Sprintf("nv action install system image files \"%s\" force", fileName)
			fmt.Printf("WARNING: This will trigger a reboot!\n")
		}

		fmt.Printf("Executing: %s\n", installCmd)
		output, err := client.RunCommand(installCmd)
		if err != nil {
			// For NVOS, connection drop is expected due to reboot
			if component == nvswitch.NVOS && (strings.Contains(err.Error(), "EOF") || strings.Contains(err.Error(), "connection reset")) {
				fmt.Printf("\n✓ Install initiated, switch is rebooting\n")
				return
			}
			log.Fatalf("Install failed: %v", err)
		}

		fmt.Printf("\nOutput:\n%s\n", output)
		fmt.Printf("✓ Install completed\n")
	},
}

var sshGetVersionCmd = &cobra.Command{
	Use:   "get-version",
	Short: "Get current firmware version via SSH",
	Long: `Query the current firmware version for a component via SSH.

Example:
  nvswitch-manager update-strategy ssh get-version --nvos-ip 192.168.1.101 -p password --component CPLD`,
	Run: func(cmd *cobra.Command, args []string) {
		if usNvosIP == "" || usNvosPassword == "" {
			log.Fatal("NVOS IP and password are required")
		}

		component := nvswitch.Component(strings.ToUpper(usComponent))
		if component != nvswitch.CPLD && component != nvswitch.NVOS {
			log.Fatalf("Invalid component for SSH: %s (valid: CPLD, NVOS)", usComponent)
		}

		client, err := createSSHClient()
		if err != nil {
			log.Fatalf("Failed to create SSH client: %v", err)
		}
		defer client.Close()

		var version string
		switch component {
		case nvswitch.CPLD:
			output, err := client.RunCommand("nv show platform firmware")
			if err != nil {
				log.Fatalf("Failed to get CPLD version: %v", err)
			}
			// Parse CPLD version from output - look for CPLD1 line
			for _, line := range strings.Split(output, "\n") {
				if strings.Contains(line, "CPLD1") {
					fields := strings.Fields(line)
					if len(fields) >= 2 {
						version = fields[1]
						break
					}
				}
			}
			if version == "" {
				version = strings.TrimSpace(output)
			}

		case nvswitch.NVOS:
			output, err := client.RunCommand("nv show system version")
			if err != nil {
				log.Fatalf("Failed to get NVOS version: %v", err)
			}
			// Parse version - look for "image" line
			for _, line := range strings.Split(output, "\n") {
				if strings.Contains(line, "image") {
					fields := strings.Fields(line)
					if len(fields) >= 2 {
						version = fields[1]
						break
					}
				}
			}
			if version == "" {
				version = strings.TrimSpace(output)
			}
		}

		fmt.Printf("Component: %s\n", component)
		fmt.Printf("Version:   %s\n", version)
	},
}

var sshCheckReachableCmd = &cobra.Command{
	Use:   "check-reachable",
	Short: "Check if NVOS is reachable (SSH connectivity test)",
	Long: `Check if the NVOS is reachable via SSH.

Example:
  nvswitch-manager update-strategy ssh check-reachable --nvos-ip 192.168.1.101 --nvos-port 22 -p password`,
	Run: func(cmd *cobra.Command, args []string) {
		if usNvosIP == "" || usNvosPassword == "" {
			log.Fatal("NVOS IP and password are required")
		}

		fmt.Printf("Checking SSH connectivity to %s:%d...\n", usNvosIP, usNvosPort)

		client, err := createSSHClient()
		if err != nil {
			fmt.Printf("✗ Not reachable: %v\n", err)
			os.Exit(1)
		}
		defer client.Close()

		// Run a simple command to verify
		output, err := client.RunCommand("hostname")
		if err != nil {
			fmt.Printf("✗ SSH connected but command failed: %v\n", err)
			os.Exit(1)
		}

		fmt.Printf("✓ Reachable - hostname: %s", output)
	},
}

// ============================================================================
// SCRIPT STRATEGY SUBCOMMANDS
// ============================================================================

var scriptStrategyCmd = &cobra.Command{
	Use:   "script",
	Short: "Test script-based firmware update strategy",
	Long:  `Test script-based firmware update operations (legacy approach).`,
}

var scriptRunCmd = &cobra.Command{
	Use:   "run",
	Short: "Run an update script",
	Long: `Run a firmware update script with the specified parameters.

The script is called with arguments:
  <BMC_IP> <BMC_USER> <BMC_PASS> <NVOS_IP> <NVOS_USER> <NVOS_PASS> <FIRMWARE_PATH>

Example:
  nvswitch-manager update-strategy script run \
    --bmc-ip 192.168.1.100 --bmc-pass password \
    --nvos-ip 192.168.1.101 --nvos-pass password \
    --script /opt/scripts/nvswfwupd.sh --file firmware.fwpkg`,
	Run: func(cmd *cobra.Command, args []string) {
		if usBmcIP == "" || usBmcPassword == "" {
			log.Fatal("BMC IP and password are required")
		}
		if usNvosIP == "" || usNvosPassword == "" {
			log.Fatal("NVOS IP and password are required")
		}
		if usScriptPath == "" {
			log.Fatal("Script path is required (--script)")
		}
		if usFirmwareFile == "" {
			log.Fatal("Firmware file is required (--file)")
		}

		// Verify script exists
		if _, err := os.Stat(usScriptPath); err != nil {
			log.Fatalf("Script not found: %v", err)
		}

		// Verify firmware exists
		if _, err := os.Stat(usFirmwareFile); err != nil {
			log.Fatalf("Firmware file not found: %v", err)
		}

		scriptArgs := []string{
			usBmcIP,
			usBmcUsername,
			usBmcPassword,
			usNvosIP,
			usNvosUsername,
			usNvosPassword,
			usFirmwareFile,
		}

		fmt.Printf("Running script: %s\n", usScriptPath)
		fmt.Printf("Arguments: %s %s *** %s %s *** %s\n",
			usBmcIP, usBmcUsername, usNvosIP, usNvosUsername, usFirmwareFile)

		scriptCmd := exec.Command(usScriptPath, scriptArgs...)
		scriptCmd.Stdout = os.Stdout
		scriptCmd.Stderr = os.Stderr
		scriptCmd.Env = os.Environ()

		startTime := time.Now()
		if err := scriptCmd.Run(); err != nil {
			log.Fatalf("Script failed: %v", err)
		}

		fmt.Printf("\n✓ Script completed in %v\n", time.Since(startTime).Round(time.Second))
	},
}

var scriptListCmd = &cobra.Command{
	Use:   "list",
	Short: "List available update scripts",
	Long: `List available firmware update scripts in the scripts directory.

Example:
  nvswitch-manager update-strategy script list --script-dir /opt/nvswitch-manager/scripts`,
	Run: func(cmd *cobra.Command, args []string) {
		scriptDir, _ := cmd.Flags().GetString("script-dir")
		if scriptDir == "" {
			scriptDir = "/opt/nvswitch-manager/scripts"
		}

		fmt.Printf("Scripts in %s:\n\n", scriptDir)

		entries, err := os.ReadDir(scriptDir)
		if err != nil {
			log.Fatalf("Failed to read directory: %v", err)
		}

		found := false
		for _, entry := range entries {
			if entry.IsDir() {
				continue
			}
			if strings.HasSuffix(entry.Name(), ".sh") {
				info, _ := entry.Info()
				fmt.Printf("  %-30s %d bytes\n", entry.Name(), info.Size())
				found = true
			}
		}

		if !found {
			fmt.Println("  No scripts found")
		}
	},
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

// createTestTray creates an NVSwitchTray for testing
// It handles optional BMC and NVOS - only creates subsystems that have IPs configured
func createTestTray() *nvswitch.NVSwitchTray {
	tray := &nvswitch.NVSwitchTray{
		UUID: uuid.New(),
	}

	// Create BMC if IP is provided
	if usBmcIP != "" {
		bmcCred := &credential.Credential{
			User:     usBmcUsername,
			Password: secretstring.New(usBmcPassword),
		}
		b, err := bmc.New("00:00:00:00:00:00", usBmcIP, bmcCred)
		if err != nil {
			log.Fatalf("Failed to create BMC: %v", err)
		}
		tray.BMC = b
	}

	// Create NVOS if IP is provided
	if usNvosIP != "" {
		nvosCred := &credential.Credential{
			User:     usNvosUsername,
			Password: secretstring.New(usNvosPassword),
		}
		n, err := nvos.New("00:00:00:00:00:00", usNvosIP, nvosCred)
		if err != nil {
			log.Fatalf("Failed to create NVOS: %v", err)
		}
		tray.NVOS = n
	}

	return tray
}

// createSSHClient creates an SSH client for testing
func createSSHClient() (*sshclient.NVOSClient, error) {
	cred := &credential.Credential{
		User:     usNvosUsername,
		Password: secretstring.New(usNvosPassword),
	}

	n, err := nvos.New("00:00:00:00:00:00", usNvosIP, cred)
	if err != nil {
		return nil, fmt.Errorf("failed to create NVOS: %v", err)
	}

	return sshclient.NewWithPort(context.Background(), n, usNvosPort)
}

// ============================================================================
// INIT
// ============================================================================

func init() {
	rootCmd.AddCommand(updateStrategyCmd)

	// Common flags for all update-strategy commands
	updateStrategyCmd.PersistentFlags().StringVar(&usBmcIP, "bmc-ip", "", "BMC IP address")
	updateStrategyCmd.PersistentFlags().IntVar(&usBmcPort, "bmc-port", 443, "BMC HTTPS port")
	updateStrategyCmd.PersistentFlags().StringVarP(&usBmcUsername, "bmc-user", "U", "root", "BMC username")
	updateStrategyCmd.PersistentFlags().StringVarP(&usBmcPassword, "bmc-pass", "P", "", "BMC password")
	updateStrategyCmd.PersistentFlags().StringVar(&usNvosIP, "nvos-ip", "", "NVOS IP address")
	updateStrategyCmd.PersistentFlags().IntVar(&usNvosPort, "nvos-port", 22, "NVOS SSH port")
	updateStrategyCmd.PersistentFlags().StringVarP(&usNvosUsername, "nvos-user", "u", "admin", "NVOS SSH username")
	updateStrategyCmd.PersistentFlags().StringVarP(&usNvosPassword, "nvos-pass", "p", "", "NVOS SSH password")
	updateStrategyCmd.PersistentFlags().StringVarP(&usFirmwareFile, "file", "f", "", "Firmware file path")
	updateStrategyCmd.PersistentFlags().StringVar(&usComponent, "component", "BMC", "Component (BMC, BIOS, CPLD, NVOS)")
	updateStrategyCmd.PersistentFlags().StringVar(&usRemoteDir, "remote-dir", "/home/admin", "Remote directory for file operations")

	// Add strategy subcommands
	updateStrategyCmd.AddCommand(redfishStrategyCmd)
	updateStrategyCmd.AddCommand(sshStrategyCmd)
	updateStrategyCmd.AddCommand(scriptStrategyCmd)

	// Redfish strategy subcommands
	redfishStrategyCmd.AddCommand(redfishUploadCmd)
	redfishStrategyCmd.AddCommand(redfishPollTaskCmd)
	redfishStrategyCmd.AddCommand(redfishGetVersionCmd)

	redfishPollTaskCmd.Flags().StringVar(&usTaskID, "task-id", "", "Redfish task ID/URI to poll")
	redfishUploadCmd.Flags().BoolVar(&usSkipApplyTime, "skip-apply-time", false, "Skip setting apply time (use if BMC already has Immediate set)")

	// SSH strategy subcommands
	sshStrategyCmd.AddCommand(usSSHCopyCmd)
	sshStrategyCmd.AddCommand(sshFetchCmd)
	sshStrategyCmd.AddCommand(sshInstallCmd)
	sshStrategyCmd.AddCommand(sshGetVersionCmd)
	sshStrategyCmd.AddCommand(sshCheckReachableCmd)

	// Script strategy subcommands
	scriptStrategyCmd.AddCommand(scriptRunCmd)
	scriptStrategyCmd.AddCommand(scriptListCmd)

	scriptRunCmd.Flags().StringVar(&usScriptPath, "script", "", "Path to update script")
	scriptListCmd.Flags().String("script-dir", "/opt/nvswitch-manager/scripts", "Directory containing update scripts")
}
