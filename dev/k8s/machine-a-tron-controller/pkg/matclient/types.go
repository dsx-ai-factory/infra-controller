// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package matclient provides an HTTP client for the machine-a-tron API.
package matclient

// MachinesStatusResponse is the top-level response from GET /machines/status.
type MachinesStatusResponse struct {
	Machines []MachineStatus `json:"machines"`
}

// MachineStatus represents the status of a single machine (host or DPU).
type MachineStatus struct {
	// MatID is the machine-a-tron internal identifier (UUID).
	MatID string `json:"mat_id"`
	// MachineID is the observed NICo machine ID, if known.
	MachineID *string `json:"machine_id,omitempty"`
	// HardwareType is the hardware model (e.g., "GB200", "DGX").
	HardwareType *string `json:"hardware_type,omitempty"`
	// MatState is the machine-a-tron FSM state.
	MatState *string `json:"mat_state,omitempty"`
	// APIState is the observed NICo API state.
	APIState string `json:"api_state"`
	// PowerState is the current power state ("On", "Off", etc.).
	PowerState string `json:"power_state"`
	// MachineIP is the machine's management IP, if known.
	MachineIP *string `json:"machine_ip,omitempty"`
	// BMC contains BMC endpoint information.
	BMC BMCStatus `json:"bmc"`
	// DPUs contains nested DPU statuses for host machines.
	DPUs []MachineStatus `json:"dpus,omitempty"`
}

// BMCStatus contains the BMC endpoint configuration.
type BMCStatus struct {
	// IP is the BMC IP address, if assigned.
	IP *string `json:"ip,omitempty"`
	// Redfish contains the Redfish endpoint configuration.
	Redfish EndpointStatus `json:"redfish"`
	// IPMI contains the IPMI endpoint configuration, if enabled.
	IPMI *EndpointStatus `json:"ipmi,omitempty"`
	// SSH contains the SSH serial-console endpoint configuration, if enabled.
	SSH *EndpointStatus `json:"ssh,omitempty"`
}

// EndpointStatus describes port mapping for an endpoint.
type EndpointStatus struct {
	// ReachablePort is the external port clients should connect to (e.g., 443 for Redfish).
	ReachablePort uint16 `json:"reachable_port"`
	// ListenPort is the internal port the mock BMC listens on.
	ListenPort uint16 `json:"listen_port"`
}
