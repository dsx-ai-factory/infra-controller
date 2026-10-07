// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"fmt"
	"os"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/simple"
)

func main() {
	// NICO_BASE_URL, NICO_ORG, and NICO_TOKEN are required.
	// NICO_MACHINE_ID is optional; if not set, a Ready machine is selected.
	// NICO_SITE_ID and NICO_VPC_ID are optional for testing.
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
	if vpcID := os.Getenv("NICO_VPC_ID"); vpcID != "" {
		client.SetVpcID(vpcID)
	}
	if err := client.Authenticate(ctx); err != nil {
		fmt.Printf("Error authenticating: %v\n", err)
		os.Exit(1)
	}

	// Get machines to select one in Ready state
	machines, _, apiErr := client.GetMachines(ctx, nil)
	if apiErr != nil {
		fmt.Printf("Error getting machines: %s\n", apiErr.Message)
		os.Exit(1)
	}

	selectedMachineID := os.Getenv("NICO_MACHINE_ID")
	if selectedMachineID == "" {
		for _, machine := range machines {
			if machine.Status == "Ready" {
				selectedMachineID = machine.ID
				break
			}
		}
	}
	if selectedMachineID == "" {
		fmt.Println("Could not find a suitable Machine to create an Instance. Set NICO_MACHINE_ID or ensure a Ready machine exists.")
		os.Exit(1)
	}

	// Create an Instance
	userData := "#cloud-config\nnetwork:\n  version: 2\n  ethernets:\n    eth0:\n      set-name: eth0\n      dhcp4: true\n      optional: true\n      match:\n        name: en*np0\n\n#user-data\nusers:\n- name: nico\nlock_passwd: false\nshell: /bin/bash\nsudo: ALL=(ALL) NOPASSWD:ALL\ngroups: users, admin\npasswd: <replace-with-hashed-password>\n"
	instanceCreateRequest := simple.InstanceCreateRequest{
		Name:       "test-instance",
		MachineID:  selectedMachineID,
		IpxeScript: "chain ${base-url}/internal/x86_64/qcow-imager.efi loglevel=7 console=ttyS0,115200 console=tty0 console=ttyS1,115200 pci=realloc=off image_distro_name=ubuntu image_distro_version=22.04 ds=nocloud-net;s=${tenant-cloudinit-url}\nboot",
		UserData:   &userData,
		SSHKeys: []string{
			"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleFakeKeyForDocumentationPurposesOnly user@example.com",
		},
		Labels: map[string]string{
			"test-key": "test-value",
		},
	}
	instance, apiErr := client.CreateInstance(ctx, instanceCreateRequest)
	if apiErr != nil {
		fmt.Printf("Error creating instance: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Created instance: %s. Will delete in 10s.\n", instance.GetId())
	time.Sleep(10 * time.Second)

	// Delete the Instance
	apiErr = client.DeleteInstance(ctx, instance.GetId())
	if apiErr != nil {
		fmt.Printf("Error deleting instance: %s\n", apiErr.Message)
		os.Exit(1)
	}
	fmt.Printf("Deleted instance: %s\n", instance.GetId())
}
