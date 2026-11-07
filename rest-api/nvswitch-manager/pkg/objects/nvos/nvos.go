// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvos

import (
	"fmt"
	"net"
	"regexp"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
)

// DefaultNVOSPort is the default SSH port.
const DefaultNVOSPort = 22

// NVOS represents the NV-Switch Operating System subsystem of an NV-Switch tray.
// It has its own MAC address, IP address, port, and credentials for SSH access.
type NVOS struct {
	MAC        net.HardwareAddr       `json:"mac"`
	IP         net.IP                 `json:"ip"`
	Port       int                    `json:"port"` // Custom port (0 = default 22)
	Credential *credential.Credential `json:"credential"`
}

// New creates a new NVOS instance by parsing MAC and IP from strings.
func New(mac string, ip string, cred *credential.Credential) (*NVOS, error) {
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

// NewFromAddr creates a new NVOS instance from native net types.
func NewFromAddr(mac net.HardwareAddr, ip net.IP, cred *credential.Credential) (*NVOS, error) {
	if mac == nil {
		return nil, fmt.Errorf("MAC address is required")
	}
	if ip == nil {
		return nil, fmt.Errorf("IP address is required")
	}

	n := &NVOS{
		MAC: mac,
		IP:  ip,
	}

	if cred != nil {
		nc := *cred
		n.Credential = &nc
	}

	return n, nil
}

// GetMAC returns the NVOS MAC address.
func (n *NVOS) GetMAC() net.HardwareAddr {
	return n.MAC
}

// GetIP returns the NVOS IP address.
func (n *NVOS) GetIP() net.IP {
	return n.IP
}

// GetCredential returns the NVOS credential or nil.
func (n *NVOS) GetCredential() *credential.Credential {
	return n.Credential
}

// SetCredential sets the NVOS credential.
func (n *NVOS) SetCredential(cred *credential.Credential) {
	if cred != nil {
		nc := *cred
		n.Credential = &nc
	}
}

// SetIP sets the NVOS IP address.
func (n *NVOS) SetIP(ip string) error {
	parsed := net.ParseIP(ip)
	if parsed == nil {
		return fmt.Errorf("invalid IP address: %s", ip)
	}
	n.IP = parsed
	return nil
}

// GetPort returns the effective port (custom or default 22).
func (n *NVOS) GetPort() int {
	if n.Port > 0 {
		return n.Port
	}
	return DefaultNVOSPort
}

// SetPort sets a custom port for NVOS access.
func (n *NVOS) SetPort(port int) {
	n.Port = port
}
