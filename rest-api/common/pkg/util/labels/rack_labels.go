// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package labels

// Well-known label keys for Expected/Managed Rack metadata.
// These mirror the constants defined in Core's api-model crate so REST callers
// and Core stay aligned on rack chassis and location labels.

const (
	// Chassis identity labels — physically identifies the rack hardware.
	RackLabelChassisManufacturer = "chassis.manufacturer"
	RackLabelChassisSerialNumber = "chassis.serial-number"
	RackLabelChassisModel        = "chassis.model"

	// Physical location labels — identifies where the rack lives.
	RackLabelLocationRegion     = "location.region"
	RackLabelLocationDatacenter = "location.datacenter"
	RackLabelLocationRoom       = "location.room"
	RackLabelLocationPosition   = "location.position"
)
