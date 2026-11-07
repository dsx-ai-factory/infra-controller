// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	pb "github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/internal/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/common/vendor"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
)

var vendorToMap map[vendor.VendorCode]pb.Vendor
var vendorFromMap map[pb.Vendor]vendor.VendorCode

func init() {
	// Initialize the mappings between internal vendor types and protobuf types
	vendorToMap = map[vendor.VendorCode]pb.Vendor{
		vendor.VendorCodeUnsupported: pb.Vendor_VENDOR_UNKNOWN,
		vendor.VendorCodeNVIDIA:      pb.Vendor_VENDOR_NVIDIA,
	}

	// Reverse mappings for vendor types
	vendorFromMap = make(map[pb.Vendor]vendor.VendorCode)
	for t, pt := range vendorToMap {
		vendorFromMap[pt] = t
	}
}

// VendorFrom maps a protobuf Vendor to a vendor.VendorCode.
func VendorFrom(pt pb.Vendor) vendor.VendorCode {
	if t, ok := vendorFromMap[pt]; ok {
		return t
	}

	return vendor.VendorCodeUnsupported
}

// VendorTo maps a vendor.VendorCode to its protobuf Vendor.
func VendorTo(t vendor.VendorCode) pb.Vendor {
	if pt, ok := vendorToMap[t]; ok {
		return pt
	}

	return pb.Vendor_VENDOR_UNKNOWN
}

// NVSwitchTrayTo converts an internal NVSwitchTray to a protobuf NVSwitchTray
func NVSwitchTrayTo(tray *nvswitch.NVSwitchTray) *pb.NVSwitchTray {
	if tray == nil {
		return nil
	}

	pbTray := &pb.NVSwitchTray{
		Uuid:        tray.UUID.String(),
		Vendor:      VendorTo(tray.Vendor.Code),
		CpldVersion: tray.CPLDVersion,
		RackId:      tray.RackID,
	}

	// BMC info
	if tray.BMC != nil {
		pbTray.Bmc = &pb.BMCInfo{
			MacAddress:      tray.BMC.MAC.String(),
			IpAddress:       tray.BMC.IP.String(),
			Port:            int32(tray.BMC.GetPort()),
			FirmwareVersion: tray.FirmwareVersion,
		}
		// Enrich with manager info if available
		if tray.Manager != nil {
			pbTray.Bmc.SerialNumber = tray.Manager.SerialNumber
			pbTray.Bmc.Model = tray.Manager.Model
			pbTray.Bmc.Manufacturer = tray.Manager.Manufacturer
			pbTray.Bmc.PartNumber = tray.Manager.PartNumber
			pbTray.Bmc.FirmwareVersion = tray.Manager.FirmwareVersion
		}
	}

	// NVOS info
	if tray.NVOS != nil {
		pbTray.Nvos = &pb.NVOSInfo{
			MacAddress: tray.NVOS.MAC.String(),
			IpAddress:  tray.NVOS.IP.String(),
			Port:       int32(tray.NVOS.GetPort()),
			Version:    tray.NVOSVersion,
		}
	}

	// Chassis info
	if tray.Chassis != nil {
		pbTray.Chassis = &pb.Chassis{
			SerialNumber: tray.Chassis.SerialNumber,
			Model:        tray.Chassis.Model,
			Manufacturer: tray.Chassis.Manufacturer,
		}
	}

	return pbTray
}

// ParseUUID parses a UUID string.
func ParseUUID(s string) (uuid.UUID, error) {
	return uuid.Parse(s)
}
