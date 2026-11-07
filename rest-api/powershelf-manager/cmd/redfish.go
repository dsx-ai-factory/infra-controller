// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"
	"net"
	"strings"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/secretstring"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/util"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/redfish"
)

var pmcIP string
var pmcUsername string
var pmcPassword string
var redfish_action string
var firmwarePath string

type Action string

const (
	QueryChassis                     Action = "query_chassis"
	QueryManager                     Action = "query_manager"
	QueryPowerStatus                 Action = "query_power_status"
	PowerOff                         Action = "power_off"
	PowerOn                          Action = "power_on"
	QueryUpdateService               Action = "query_update_service"
	QueryFirmwareInventories         Action = "query_firmware_inventories"
	QueryFirmwareVersion             Action = "query_firmware_version"
	ResetChassis                     Action = "reset_chassis"
	GracefulResetPmc                 Action = "graceful_reset_pmc"
	ForceResetPmc                    Action = "force_reset_pmc"
	FactoryResetPmc                  Action = "factory_reset_pmc"
	SetHttpPushUriApplyTimeImmediate Action = "set_http_push_uri_apply_time_immediate"
	UploadFirmwareFile               Action = "upload_firmware_file"
	QueryPowerSubsystem              Action = "query_power_subsystem"
	QueryPowerSupply                 Action = "query_power_supply"
)

var actions = []Action{
	QueryChassis,
	QueryManager,
	QueryPowerStatus,
	PowerOff,
	PowerOn,
	QueryUpdateService,
	QueryFirmwareInventories,
	QueryFirmwareVersion,
	ResetChassis,
	GracefulResetPmc,
	ForceResetPmc,
	FactoryResetPmc,
	SetHttpPushUriApplyTimeImmediate,
	UploadFirmwareFile,
	QueryPowerSupply,
}

// redfishCmd represents the redfish command
var redfishCmd = &cobra.Command{
	Use:   "redfish",
	Short: "Interact with a PMC",
	Long:  `Interact with a PMC`,
	Run: func(cmd *cobra.Command, args []string) {
		doRedfish()
	},
}

func getAvailableActions() string {
	actionStrings := make([]string, len(actions))
	for i, action := range actions {
		actionStrings[i] = string(action)
	}
	return strings.Join(actionStrings, ", ")
}

func init() {
	rootCmd.AddCommand(redfishCmd)

	// Here you will define your flags and configuration settings.

	// Cobra supports Persistent Flags which will work for this command
	// and all subcommands, e.g.:
	// serveCmd.PersistentFlags().String("foo", "", "A help for foo")

	// Cobra supports local flags which will only run when this command
	// is called directly, e.g.:
	// serveCmd.Flags().BoolP("toggle", "t", false, "Help message for toggle")
	redfishCmd.Flags().StringVarP(&pmcIP, "ip", "i", "", "PMC IP address")
	redfishCmd.Flags().StringVarP(&pmcUsername, "user", "u", "root", "Username")
	redfishCmd.Flags().StringVarP(&pmcPassword, "pass", "p", "0penBmc", "Password")
	redfishCmd.Flags().StringVarP(&redfish_action, "action", "a", "", "Action to perform: "+getAvailableActions())
	redfishCmd.Flags().StringVarP(&firmwarePath, "firmware", "f", "", "Path to firmware file (for upload_firmware_file action)")
}

func doRedfish() {
	ip := net.ParseIP(pmcIP)
	if ip == nil {
		log.Fatalf("invalid IP address: %s", pmcIP)
	}

	pmc := pmc.PMC{
		IP: ip,
		Credential: &credential.Credential{
			User:     pmcUsername,
			Password: secretstring.New(pmcPassword),
		},
	}

	client, err := redfish.New(context.Background(), &pmc, false)
	if err != nil {
		log.Fatalf("failed to create redfish client: %v\n", err)
	}
	defer client.Logout()

	switch Action(redfish_action) {
	case QueryChassis:
		chassis, err := client.QueryChassis()
		if err != nil {
			log.Fatalf("failed to query chassis: %v\n", err)
		}
		fmt.Printf("Health Status: %s\n", chassis.Status.Health)
		fmt.Printf("Model: %s\n", chassis.Model)
		fmt.Printf("SKU: %s\n", chassis.SKU)
		fmt.Printf("Serial Number: %s\n", chassis.SerialNumber)
		fmt.Printf("Power State: %s\n", chassis.PowerState)
	case QueryManager:
		manager, err := client.QueryManager()
		if err != nil {
			log.Fatalf("failed to query manager: %v\n", err)
		}

		fmt.Printf("Name: %s\n", manager.Name)
		fmt.Printf("Manager Type: %s\n", manager.ManagerType)
		fmt.Printf("Model: %s\n", manager.Model)
		fmt.Printf("Manufacturer: %s\n", manager.Manufacturer)
		fmt.Printf("Rack Offset Location: %v\n", manager.Location.Placement.RackOffset)
		fmt.Printf("Firmware Version: %s\n", manager.FirmwareVersion)
		fmt.Printf("Part Number: %s\n", manager.PartNumber)
		fmt.Printf("Power Status: %s\n", manager.PowerState)
		fmt.Printf("UUID: %s\n", manager.UUID)
		fmt.Printf("Last Reset Time: %s\n", manager.LastResetTime)

	case QueryPowerStatus:
		power_state, err := client.QueryPowerState()
		if err != nil {
			log.Fatalf("failed to create redfish client: %v\n", err)
		}
		fmt.Printf("Power State: %s\n", power_state)
	case PowerOff:
		resp, err := client.PowerOff()
		if err != nil {
			log.Fatalf("failed to query power subsystem: %v\n", err)
		}
		util.PrintPrettyResponse(resp)
	case PowerOn:
		resp, err := client.PowerOn()
		if err != nil {
			log.Fatalf("failed to query power subsystem: %v\n", err)
		}
		util.PrintPrettyResponse(resp)
	case QueryUpdateService:
		updateService, err := client.UpdateService()
		if err != nil {
			log.Fatalf("failed to query Update Service: %v\n", err)
		}
		fmt.Printf("update service: %v\n", updateService)
	case QueryFirmwareInventories:
		fwInventories, err := client.FirmwareInventories()
		if err != nil {
			log.Fatalf("failed to query fw inventories: %v\n", err)
		}
		for _, fw := range fwInventories {
			fmt.Printf("%v: %v\n", fw.ID, fw.Version)
		}
	case QueryFirmwareVersion:
		manager, err := client.QueryManager()
		if err != nil {
			log.Fatalf("failed to query manager: %v\n", err)
		}
		fmt.Printf("%s\n", manager.FirmwareVersion)
	case ResetChassis:
		resp, err := client.ResetChassis()
		if err != nil {
			log.Fatalf("failed to reset chassis: %v\n", err)
		}
		util.PrintPrettyResponse(resp)
	case GracefulResetPmc:
		resp, err := client.ResetPmc(redfish.GracefulRestart)
		if err != nil {
			log.Fatalf("failed to reset pmc: %v\n", err)
		}
		util.PrintPrettyResponse(resp)
	case ForceResetPmc:
		resp, err := client.ResetPmc(redfish.ForceRestart)
		if err != nil {
			log.Fatalf("failed to reset pmc: %v\n", err)
		}
		util.PrintPrettyResponse(resp)
	case FactoryResetPmc:
		resp, err := client.FactoryResetPmc()
		if err != nil {
			log.Fatalf("failed to factory reset pmc: %v\n", err)
		}
		util.PrintPrettyResponse(resp)
	case SetHttpPushUriApplyTimeImmediate:
		resp, err := client.SetHttpPushUriApplyTimeImmediate()
		if err != nil {
			log.Fatalf("failed to configure http push uri apply time: %v\n", err)
		}
		util.PrintPrettyResponse(resp)
	case UploadFirmwareFile:
		if firmwarePath == "" {
			log.Fatalf("firmware path is required for upload_firmware_file action (use --firmware/-f)")
		}
		resp, err := client.UploadFirmwareByPath(firmwarePath)
		if err != nil {
			log.Fatalf("failed to upload firmware file: %v\n", err)
		}
		util.PrintPrettyResponse(resp)
	case QueryPowerSubsystem:
		power_subsystem, err := client.QueryPowerSubsystem()
		if err != nil {
			log.Fatalf("failed to query power supplies: %v\n", err)
		}
		fmt.Printf("update service: %v\n", *power_subsystem)

	case QueryPowerSupply:
		power_supplies, err := client.QueryPowerSupplies()
		if err != nil {
			log.Fatalf("failed to query power supplies: %v\n", err)
		}

		for _, psu := range power_supplies {
			if psu != nil {
				fmt.Printf("%s", psu.Summary())
			}
		}
	default:
		log.Fatalf("unknown action: %v\n", err)
	}

}
