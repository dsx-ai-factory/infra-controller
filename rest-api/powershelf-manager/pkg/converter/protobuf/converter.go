// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	"fmt"

	gofish "github.com/stmcginnis/gofish/redfish"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	pb "github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/internal/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powersupply"
)

var pmcTypeToMap map[vendor.VendorCode]pb.PMCVendor
var pmcTypeFromMap map[pb.PMCVendor]vendor.VendorCode
var componentTypeFromMap map[pb.PowershelfComponent]powershelf.Component

func init() {
	// Initialize the mappings between internal PMC types and protobuf PMC
	// types
	pmcTypeToMap = map[vendor.VendorCode]pb.PMCVendor{
		vendor.VendorCodeUnsupported: pb.PMCVendor_PMC_TYPE_UNKNOWN,
		vendor.VendorCodeLiteon:      pb.PMCVendor_PMC_TYPE_LITEON,
		vendor.VendorCodeDelta:       pb.PMCVendor_PMC_TYPE_DELTA,
	}

	componentTypeFromMap = map[pb.PowershelfComponent]powershelf.Component{
		pb.PowershelfComponent_PMC: powershelf.PMC,
		pb.PowershelfComponent_PSU: powershelf.PSU,
	}

	// Reverse mappings for PMC types
	pmcTypeFromMap = make(map[pb.PMCVendor]vendor.VendorCode)
	for t, pt := range pmcTypeToMap {
		pmcTypeFromMap[pt] = t
	}
}

// ComponentTypeFromMap maps a protobuf Component to a powershelf.Component.
func ComponentTypeFromMap(pbComponent pb.PowershelfComponent) (powershelf.Component, error) {
	if t, ok := componentTypeFromMap[pbComponent]; ok {
		return t, nil
	}

	return "", fmt.Errorf("unsupported protobuf Component type: %v", pbComponent)
}

// PMCVendorFrom maps a protobuf PMCVendor to a vendor.VendorCode.
func PMCVendorFrom(pt pb.PMCVendor) vendor.VendorCode {
	if t, ok := pmcTypeFromMap[pt]; ok {
		return t
	}

	return vendor.VendorCodeUnsupported
}

// VendorCodeTo maps a vendor.VendorCode to its protobuf PMCVendor.
func VendorCodeTo(t vendor.VendorCode) pb.PMCVendor {
	if pt, ok := pmcTypeToMap[t]; ok {
		return pt
	}

	return pb.PMCVendor_PMC_TYPE_UNKNOWN
}

// PMCTo converts an internal PMC to a protobuf Pmc
func PMCTo(pmc *pmc.PMC) *pb.PowerManagementController {
	if pmc == nil {
		return nil
	}

	return &pb.PowerManagementController{
		MacAddress: pmc.MAC.String(),
		IpAddress:  pmc.IP.String(),
		Vendor:     VendorCodeTo(pmc.Vendor.Code),
	}
}

// ChassisTo converts an internal Chassis to a protobuf Pmc
func ChassisTo(chassis *gofish.Chassis) *pb.Chassis {
	if chassis == nil {
		return nil
	}

	return &pb.Chassis{
		SerialNumber: chassis.SerialNumber,
		Model:        chassis.Model,
		Manufacturer: chassis.Model,
	}
}

func ThresholdTo(threshold *gofish.Threshold) *pb.SensorThreshold {
	if threshold == nil {
		return nil
	}

	return &pb.SensorThreshold{
		Reading: threshold.Reading,
	}
}

func ThresholdsTo(thresholds *gofish.Thresholds) *pb.SensorThresholds {
	if thresholds == nil {
		return nil
	}

	return &pb.SensorThresholds{
		LowerCaution:  ThresholdTo(&thresholds.LowerCaution),
		LowerCritical: ThresholdTo(&thresholds.LowerCritical),
		UpperCaution:  ThresholdTo(&thresholds.UpperCaution),
		UpperCritical: ThresholdTo(&thresholds.UpperCritical),
	}
}

func SensorTo(sensor *gofish.Sensor) *pb.Sensor {
	if sensor == nil {
		return nil
	}

	return &pb.Sensor{
		Id:              sensor.ID,
		Name:            sensor.Name,
		Reading:         sensor.Reading,
		ReadingRangeMax: sensor.ReadingRangeMax,
		ReadingRangeMin: sensor.ReadingRangeMin,
		ReadingType:     string(sensor.ReadingType),
		ReadingUnits:    sensor.ReadingUnits,
		Thresholds:      ThresholdsTo(&sensor.Thresholds),
	}
}

// PSUTo converts an internal PSUTo to a protobuf Pmc
func PSUTo(psu *powersupply.PowerSupply) *pb.PowerSupplyUnit {
	if psu == nil {
		return nil
	}

	pbSensors := make([]*pb.Sensor, 0, len(psu.Sensors))
	for _, sensor := range psu.Sensors {
		pbSensors = append(pbSensors, SensorTo(sensor))
	}

	return &pb.PowerSupplyUnit{
		CapacityWatts:   psu.CapacityWatts,
		FirmwareVersion: psu.FirmwareVersion,
		HardwareVersion: psu.HardwareVersion,
		Id:              psu.ID,
		Manufacturer:    psu.Manufacturer,
		Model:           psu.Model,
		Name:            psu.Name,
		PowerState:      psu.PowerState,
		Sensors:         pbSensors,
		SerialNumber:    psu.SerialNumber,
	}
}

// PMCTo converts an internal PMC to a protobuf Pmc
func enrichPmc(pmc *pb.PowerManagementController, manager *gofish.Manager) {
	if pmc == nil || manager == nil {
		return
	}

	pmc.SerialNumber = manager.SerialNumber
	pmc.Model = manager.Model
	pmc.Manufacturer = manager.Manufacturer
	pmc.PartNumber = manager.PartNumber
	pmc.FirmwareVersion = manager.FirmwareVersion
}

// PowershelfTo converts an internal Powershelf to a protobuf Pmc
func PowershelfTo(powershelf *powershelf.PowerShelf) *pb.PowerShelf {
	if powershelf == nil {
		return nil
	}

	pmc := PMCTo(powershelf.PMC)
	if pmc != nil {
		enrichPmc(pmc, powershelf.Manager)
	}

	psus := make([]*pb.PowerSupplyUnit, 0, len(powershelf.PowerSupplies))
	for _, psu := range powershelf.PowerSupplies {
		if pbPsu := PSUTo(psu); pbPsu != nil {
			psus = append(psus, PSUTo(psu))
		}
	}

	return &pb.PowerShelf{
		Pmc:     pmc,
		Chassis: ChassisTo(powershelf.Chassis),
		Psus:    psus,
	}
}

// PMCFrom converts a protobuf PMC to an internal PMC
func PMCFrom(protobufPmc *pb.PowerManagementController, protobufCreds *pb.Credentials) (*pmc.PMC, error) {
	if protobufPmc == nil {
		return nil, fmt.Errorf("nil protobufPmc")
	}

	vendor := PMCVendorFrom(protobufPmc.Vendor)

	pmc, err := pmc.New(protobufPmc.MacAddress, protobufPmc.IpAddress, vendor, nil)
	if err != nil {
		return nil, err
	}

	if protobufCreds != nil {
		creds := credential.New(protobufCreds.Username, protobufCreds.Password)
		pmc.SetCredential(&creds)
	}

	return pmc, nil
}

// FirmwareStateToProto converts a domain FirmwareState to the protobuf enum.
func FirmwareStateToProto(state powershelf.FirmwareState) pb.FirmwareUpdateState {
	switch state {
	case powershelf.FirmwareStateQueued:
		return pb.FirmwareUpdateState_FIRMWARE_UPDATE_STATE_QUEUED
	case powershelf.FirmwareStateVerifying:
		return pb.FirmwareUpdateState_FIRMWARE_UPDATE_STATE_VERIFYING
	case powershelf.FirmwareStateCompleted:
		return pb.FirmwareUpdateState_FIRMWARE_UPDATE_STATE_COMPLETED
	case powershelf.FirmwareStateFailed:
		return pb.FirmwareUpdateState_FIRMWARE_UPDATE_STATE_FAILED
	default:
		return pb.FirmwareUpdateState_FIRMWARE_UPDATE_STATE_UNKNOWN
	}
}

// FirmwareUpdateStatusTo converts a domain FirmwareUpdate to a protobuf FirmwareUpdateStatus.
func FirmwareUpdateStatusTo(update *powershelf.FirmwareUpdate, pbComponent pb.PowershelfComponent) *pb.FirmwareUpdateStatus {
	if update == nil {
		return nil
	}

	return &pb.FirmwareUpdateStatus{
		PmcMacAddress: update.PmcMacAddress,
		Component:     pbComponent,
		State:         FirmwareStateToProto(update.State),
		Status:        pb.StatusCode_SUCCESS,
	}
}
