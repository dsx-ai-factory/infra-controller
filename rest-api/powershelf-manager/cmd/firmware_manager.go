// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	svc "github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/internal/service"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/credentials"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/powershelfmanager"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
)

var vendorStr string
var fwAction string
var dryRun bool
var versionTo string
var pmcMAC string

// serveCmd represents the serve command
var fwCmd = &cobra.Command{
	Use:   "fw",
	Short: "print embedded fws",
	Long:  `print embedded fws`,
	Run: func(cmd *cobra.Command, args []string) {
		doFw()
	},
}

type FwManagerAction string

const (
	Summary    FwManagerAction = "summary"
	CanUpgrade FwManagerAction = "can_upgrade"
	Upgrade    FwManagerAction = "upgrade"
)

var fwManagerActions = []FwManagerAction{
	Summary,
	CanUpgrade,
	Upgrade,
}

func getAvailableFwActions() string {
	actionStrings := make([]string, len(fwManagerActions))
	for i, action := range fwManagerActions {
		actionStrings[i] = string(action)
	}
	return strings.Join(actionStrings, ", ")
}

func init() {
	rootCmd.AddCommand(fwCmd)

	fwCmd.Flags().StringVarP(&vendorStr, "vendor", "v", "", "Vendor")
	fwCmd.Flags().StringVarP(&fwAction, "action", "a", "", "Action to perform: "+getAvailableFwActions())
	fwCmd.Flags().BoolVarP(&dryRun, "dry", "d", true, "dry run (default true)")
	fwCmd.Flags().StringVarP(&pmcIP, "ip", "i", "", "PMC IP address")
	fwCmd.Flags().StringVarP(&pmcUsername, "user", "u", "root", "Username")
	fwCmd.Flags().StringVarP(&pmcPassword, "pass", "p", "0penBmc", "Password")
	fwCmd.Flags().StringVar(&versionTo, "version", "", "Target Version to upgrade to")
	fwCmd.Flags().StringVarP(&pmcMAC, "mac", "m", "", "PMC MAC address")
	fwCmd.Flags().StringVar(&firmwareDir, "fw_dir", getEnvOrDefault("FW_DIR", "/var/lib/psm/firmware"), "Firmware files directory (env: FW_DIR)")
}

func doFw() {
	vendor := vendor.StringToVendor(vendorStr)

	if err := vendor.IsSupported(); err != nil {
		log.Fatalf("unsupported vendor: %v\n", err)
	}

	cred := credential.New(pmcUsername, pmcPassword)
	pmcInstance, err := pmc.New(pmcMAC, pmcIP, vendor.Code, &cred)
	if err != nil {
		log.Fatalf("failed to create PMC: %v\n", err)
	}

	svcConfig := svc.Config{
		Port:          port,
		DataStoreType: powershelfmanager.DatastoreTypeInMemory,
		VaultConf: credentials.VaultConfig{
			Address: vaultAddress,
			Token:   vaultToken,
		},
		DBConf: cdb.Config{
			Host:              dbHostName,
			Port:              dbPort,
			DBName:            dbName,
			Credential:        credential.New(dbUser, dbPassword),
			CACertificatePath: "",
		},
		FirmwareDir: firmwareDir,
	}

	psmConfig, err := svcConfig.ToPsmConf()
	if err != nil {
		log.Fatalf("failed to convert to psm conf: %v\n", err)
	}

	psm, err := powershelfmanager.New(context.Background(), *psmConfig)
	if err != nil {
		log.Fatalf("failed to init powershelf manager: %v\n", err)
	}

	if err := psm.RegisterPmc(context.Background(), pmcInstance); err != nil {
		log.Fatalf("failed to register PMC: %v\n", err)
	}

	fw_manager := psm.FirmwareManager

	switch FwManagerAction(fwAction) {
	case Summary:
		summary, err := fw_manager.Summary()
		if err != nil {
			log.Fatalf("failed to get fw repo summary for %v: %v\n", vendor, err)
		}

		fmt.Println(summary)
	case CanUpgrade:
		supported, err := fw_manager.CanUpdate(context.Background(), pmcInstance, powershelf.PMC, versionTo)
		if err != nil {
			log.Fatalf("failed to upgrade fw for %v: %v\n", vendor, err)
		}

		fmt.Printf("%v\n", supported)
	case Upgrade:
		fmt.Printf("Upgrading fw for %v (ip %s)\n", vendor, pmcIP)
		err := fw_manager.Upgrade(context.Background(), pmcInstance, powershelf.PMC, versionTo)
		if err != nil {
			log.Fatalf("failed to upgrade fw for %v: %v\n", vendor, err)
		}
		fmt.Println("Firmware upgrade queued.")
		time.Sleep(10 * time.Minute)
	default:
		log.Fatalf("unsupported action: %s\n", fwAction)
	}
}
