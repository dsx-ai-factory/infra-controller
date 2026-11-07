// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bmc

import (
	"fmt"
	"net"
	"regexp"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
)

// DefaultBMCPort is the default Redfish HTTPS port.
const DefaultBMCPort = 443

// BMC represents the Board Management Controller subsystem of an NV-Switch tray.
// It has its own MAC address, IP address, port, and credentials for Redfish access.
type BMC struct {
	MAC        net.HardwareAddr       `json:"mac"`
	IP         net.IP                 `json:"ip"`
	Port       int                    `json:"port"` // Custom port (0 = default 443)
	Credential *credential.Credential `json:"credential"`
}

// New creates a new BMC instance by parsing MAC and IP from strings.
func New(mac string, ip string, cred *credential.Credential) (*BMC, error) {
	if len(mac) == 12 {
		// MAC address with no separators, add ":"
		re := regexp.MustCompile(`(..)(..)(..)(..)(..)(..)`)
		mac = re.ReplaceAllString(strings.ToLower(mac), "$1:$2:$3:$4:$5:$6")
	}

	addr, err := net.ParseMAC(mac)
	if err != nil {
		return nil, fmt.Errorf("invalid MAC address: %w", err)
	}

	ipAddr := net.ParseIP(ip)
	if ipAddr == nil {
		return nil, fmt.Errorf("invalid IP address: %s", ip)
	}

	return NewFromAddr(addr, ipAddr, cred)
}

// NewFromAddr creates a new BMC instance from native net types.
func NewFromAddr(mac net.HardwareAddr, ip net.IP, cred *credential.Credential) (*BMC, error) {
	if mac == nil {
		return nil, fmt.Errorf("MAC address is required")
	}
	if ip == nil {
		return nil, fmt.Errorf("IP address is required")
	}

	b := &BMC{
		MAC: mac,
		IP:  ip,
	}

	if cred != nil {
		nc := *cred
		b.Credential = &nc
	}

	return b, nil
}

// GetMAC returns the BMC MAC address.
func (b *BMC) GetMAC() net.HardwareAddr {
	return b.MAC
}

// GetIP returns the BMC IP address.
func (b *BMC) GetIP() net.IP {
	return b.IP
}

// GetCredential returns the BMC credential or nil.
func (b *BMC) GetCredential() *credential.Credential {
	return b.Credential
}

// SetCredential sets the BMC credential.
func (b *BMC) SetCredential(cred *credential.Credential) {
	if cred != nil {
		nc := *cred
		b.Credential = &nc
	}
}

// SetIP sets the BMC IP address.
func (b *BMC) SetIP(ip string) error {
	parsed := net.ParseIP(ip)
	if parsed == nil {
		return fmt.Errorf("invalid IP address: %s", ip)
	}
	b.IP = parsed
	return nil
}

// GetPort returns the effective port (custom or default 443).
func (b *BMC) GetPort() int {
	if b.Port > 0 {
		return b.Port
	}
	return DefaultBMCPort
}

// SetPort sets a custom port for BMC access.
func (b *BMC) SetPort(port int) {
	b.Port = port
}
