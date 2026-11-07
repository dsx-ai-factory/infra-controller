// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"text/tabwriter"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/firmwaremanager/packages"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
)

var (
	fwPackagesDir   string
	fwFirmwareDir   string
	fwBundleVersion string
)

// firmwareCmd represents the firmware command group
var firmwareCmd = &cobra.Command{
	Use:   "firmware",
	Short: "Manage firmware bundles",
	Long: `Manage firmware bundles for NV-Switch updates.

Examples:
  # List all available firmware bundles
  nvswitch-manager firmware list --packages-dir ./test-firmware/packages --firmware-dir ./test-firmware/packages

  # Show details of a specific bundle
  nvswitch-manager firmware show --version 1.3.1 --packages-dir ./test-firmware/packages --firmware-dir ./test-firmware/packages`,
}

// firmwareListCmd lists available firmware bundles
var firmwareListCmd = &cobra.Command{
	Use:   "list",
	Short: "List available firmware bundles",
	Run: func(cmd *cobra.Command, args []string) {
		registry := packages.NewRegistry(fwFirmwareDir)
		if err := registry.LoadFromDirectory(fwPackagesDir); err != nil {
			log.Fatalf("Failed to load packages: %v", err)
		}

		pkgs := registry.ListPackages()
		if len(pkgs) == 0 {
			fmt.Println("No firmware bundles found.")
			fmt.Printf("\nSearched in: %s\n", fwPackagesDir)
			return
		}

		fmt.Printf("Available Firmware Bundles (%d):\n\n", len(pkgs))

		w := tabwriter.NewWriter(os.Stdout, 0, 0, 2, ' ', 0)
		fmt.Fprintln(w, "VERSION\tCOMPONENTS\tDESCRIPTION")
		fmt.Fprintln(w, "-------\t----------\t-----------")

		for _, pkg := range pkgs {
			components := make([]string, 0, len(pkg.Components))
			for name := range pkg.Components {
				components = append(components, strings.ToUpper(name))
			}
			desc := pkg.Description
			if len(desc) > 50 {
				desc = desc[:47] + "..."
			}
			fmt.Fprintf(w, "%s\t%s\t%s\n", pkg.Version, strings.Join(components, ", "), desc)
		}
		w.Flush()
	},
}

// firmwareShowCmd shows details of a specific bundle
var firmwareShowCmd = &cobra.Command{
	Use:   "show",
	Short: "Show details of a firmware bundle",
	Run: func(cmd *cobra.Command, args []string) {
		if fwBundleVersion == "" {
			log.Fatal("Bundle version is required (--version)")
		}

		registry := packages.NewRegistry(fwFirmwareDir)
		if err := registry.LoadFromDirectory(fwPackagesDir); err != nil {
			log.Fatalf("Failed to load packages: %v", err)
		}

		pkg, err := registry.Get(fwBundleVersion)
		if err != nil {
			log.Fatalf("Bundle not found: %v", err)
		}

		fmt.Printf("Firmware Bundle: %s\n", pkg.Version)
		fmt.Printf("Description: %s\n", pkg.Description)
		fmt.Printf("\nUpdate Order: %s\n", strings.Join(pkg.GetOrderedComponents(), " -> "))
		fmt.Printf("\nComponents:\n")

		w := tabwriter.NewWriter(os.Stdout, 0, 0, 2, ' ', 0)
		fmt.Fprintln(w, "  COMPONENT\tVERSION\tSTRATEGY\tFILE")
		fmt.Fprintln(w, "  ---------\t-------\t--------\t----")

		for _, compName := range pkg.GetOrderedComponents() {
			comp := pkg.GetComponent(compName)
			// Get file size
			fwPath := filepath.Join(fwFirmwareDir, comp.File)
			var sizeStr string
			if info, err := os.Stat(fwPath); err == nil {
				size := info.Size()
				switch {
				case size >= 1024*1024*1024:
					sizeStr = fmt.Sprintf("%.1fG", float64(size)/(1024*1024*1024))
				case size >= 1024*1024:
					sizeStr = fmt.Sprintf("%.1fM", float64(size)/(1024*1024))
				case size >= 1024:
					sizeStr = fmt.Sprintf("%.1fK", float64(size)/1024)
				default:
					sizeStr = fmt.Sprintf("%dB", size)
				}
			} else {
				sizeStr = "N/A"
			}

			fileName := filepath.Base(comp.File)
			if len(fileName) > 40 {
				fileName = fileName[:37] + "..."
			}
			fmt.Fprintf(w, "  %s\t%s\t%s\t%s (%s)\n",
				strings.ToUpper(compName), comp.Version, comp.Strategy, fileName, sizeStr)
		}
		w.Flush()

		// Show strategy config
		fmt.Printf("\nStrategy Configuration:\n")
		if pkg.StrategyConfig.Redfish != nil {
			fmt.Printf("  Redfish:\n")
			fmt.Printf("    Poll Interval: %ds\n", pkg.StrategyConfig.Redfish.PollIntervalSeconds)
			fmt.Printf("    Poll Timeout:  %ds\n", pkg.StrategyConfig.Redfish.PollTimeoutSeconds)
		}
		if pkg.StrategyConfig.SSH != nil {
			fmt.Printf("  SSH:\n")
			fmt.Printf("    Remote Dir:     %s\n", pkg.StrategyConfig.SSH.RemoteDir)
			fmt.Printf("    Reboot Timeout: %ds\n", pkg.StrategyConfig.SSH.RebootTimeoutSeconds)
		}
		if pkg.StrategyConfig.Script != nil {
			fmt.Printf("  Script:\n")
			fmt.Printf("    Script Dir: %s\n", pkg.StrategyConfig.Script.ScriptDir)
			fmt.Printf("    Timeout:    %ds\n", pkg.StrategyConfig.Script.TimeoutSeconds)
		}
	},
}

// firmwareValidateCmd validates a bundle's files exist
var firmwareValidateCmd = &cobra.Command{
	Use:   "validate",
	Short: "Validate firmware bundle files exist",
	Run: func(cmd *cobra.Command, args []string) {
		if fwBundleVersion == "" {
			log.Fatal("Bundle version is required (--version)")
		}

		registry := packages.NewRegistry(fwFirmwareDir)
		if err := registry.LoadFromDirectory(fwPackagesDir); err != nil {
			log.Fatalf("Failed to load packages: %v", err)
		}

		pkg, err := registry.Get(fwBundleVersion)
		if err != nil {
			log.Fatalf("Bundle not found: %v", err)
		}

		fmt.Printf("Validating bundle %s...\n\n", pkg.Version)

		allValid := true
		for _, compName := range pkg.GetOrderedComponents() {
			comp := pkg.GetComponent(compName)
			fwPath := filepath.Join(fwFirmwareDir, comp.File)

			info, err := os.Stat(fwPath)
			if err != nil {
				fmt.Printf("  [FAIL] %s: %v\n", strings.ToUpper(compName), err)
				allValid = false
				continue
			}

			fmt.Printf("  [OK]   %s: %s (%d bytes)\n", strings.ToUpper(compName), filepath.Base(comp.File), info.Size())
		}

		fmt.Println()
		if allValid {
			fmt.Println("All firmware files validated successfully.")
		} else {
			fmt.Println("Some firmware files are missing or invalid.")
			os.Exit(1)
		}
	},
}

func init() {
	rootCmd.AddCommand(firmwareCmd)

	// Default paths relative to current directory
	defaultBundlesDir := "./firmware/bundles"
	defaultFirmwareDir := "./firmware/files"

	firmwareCmd.PersistentFlags().StringVar(&fwPackagesDir, "bundles-dir", defaultBundlesDir, "Directory containing bundle YAML files")
	firmwareCmd.PersistentFlags().StringVar(&fwFirmwareDir, "firmware-dir", defaultFirmwareDir, "Base directory for firmware files")

	firmwareCmd.AddCommand(firmwareListCmd)
	firmwareCmd.AddCommand(firmwareShowCmd)
	firmwareCmd.AddCommand(firmwareValidateCmd)

	firmwareShowCmd.Flags().StringVar(&fwBundleVersion, "version", "", "Bundle version to show")
	firmwareValidateCmd.Flags().StringVar(&fwBundleVersion, "version", "", "Bundle version to validate")
}
