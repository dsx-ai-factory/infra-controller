// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dao

import (
	"fmt"
	"net"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/sqltypes"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"
)

// PmcTo converts a domain PMC to a database model.
func PmcTo(pmc *pmc.PMC) *model.PMC {
	if pmc == nil {
		return nil
	}

	return &model.PMC{
		MacAddress: model.MacAddr(pmc.GetMac()),
		IPAddress:  sqltypes.IPAddr(pmc.GetIp()),
		Vendor:     pmc.GetVendor().Code,
	}
}

// PmcFrom converts a database PMC model to a domain PMC.
func PmcFrom(dao *model.PMC) (*pmc.PMC, error) {
	if dao == nil {
		return nil, nil
	}

	// Convert model types back to net types for the domain object
	return pmc.NewFromAddr(dao.MacAddress.HardwareAddr(), dao.IPAddress.IP(), dao.Vendor, nil)
}

// FirmwareUpdateTo converts a domain FirmwareUpdate to a database model.
// Returns an error if update is nil or if the MAC address cannot be parsed.
func FirmwareUpdateTo(update *powershelf.FirmwareUpdate) (*model.FirmwareUpdate, error) {
	if update == nil {
		return nil, fmt.Errorf("cannot convert nil FirmwareUpdate")
	}

	mac, err := net.ParseMAC(update.PmcMacAddress)
	if err != nil {
		return nil, fmt.Errorf("invalid MAC address %q: %w", update.PmcMacAddress, err)
	}

	return &model.FirmwareUpdate{
		PmcMacAddress: model.MacAddr(mac),
		Component:     update.Component,
		VersionFrom:   update.VersionFrom,
		VersionTo:     update.VersionTo,
		State:         update.State,
		ErrorMessage:  update.ErrorMessage,
		JobID:         update.JobID,
	}, nil
}

// FirmwareUpdateFrom converts a database FirmwareUpdate model to a domain FirmwareUpdate.
func FirmwareUpdateFrom(dao *model.FirmwareUpdate) *powershelf.FirmwareUpdate {
	if dao == nil {
		return nil
	}

	return &powershelf.FirmwareUpdate{
		PmcMacAddress: dao.PmcMacAddress.String(),
		Component:     dao.Component,
		VersionFrom:   dao.VersionFrom,
		VersionTo:     dao.VersionTo,
		State:         dao.State,
		ErrorMessage:  dao.ErrorMessage,
		JobID:         dao.JobID,
	}
}
