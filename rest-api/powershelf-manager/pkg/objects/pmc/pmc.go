// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pmc

import (
	"fmt"
	"net"
	"regexp"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
)

// PMC specifies the information for a PMC which includes MAC address, IP
// address, and access credential.
type PMC struct {
	MAC        net.HardwareAddr       `json:"mac"`
	IP         net.IP                 `json:"ip"`
	Vendor     vendor.Vendor          `json:"vendor"`
	Credential *credential.Credential `json:"credential"`
}

// NewFromAddr creates a new PMC instance from native net.HardwareAddr and net.IP types.
// This is the preferred constructor for internal use where addresses are already parsed.
func NewFromAddr(mac net.HardwareAddr, ip net.IP, v vendor.VendorCode, cred *credential.Credential) (*PMC, error) {
	if mac == nil {
		return nil, fmt.Errorf("MAC address is required")
	}
	if ip == nil {
		return nil, fmt.Errorf("IP address is required")
	}

	vendor := vendor.CodeToVendor(v)
	if err := vendor.IsSupported(); err != nil {
		return nil, err
	}

	pmcInfo := &PMC{
		MAC:    mac,
		IP:     ip,
		Vendor: vendor,
	}

	if cred != nil {
		nc := *cred
		pmcInfo.Credential = &nc
	}

	return pmcInfo, nil
}

// New creates a new PMC instance by parsing MAC and IP from strings.
// Use this at API boundaries (gRPC, REST) where addresses come as strings.
// For internal use with already-parsed addresses, prefer NewFromAddr.
func New(mac string, ip string, v vendor.VendorCode, cred *credential.Credential) (*PMC, error) {
	if len(mac) == 12 {
		// MAC address with no separators, add ":"
		re := regexp.MustCompile(`(..)(..)(..)(..)(..)(..)`)
		mac = re.ReplaceAllString(strings.ToLower(mac), "$1:$2:$3:$4:$5:$6")
	}

	addr, err := net.ParseMAC(mac)
	if err != nil {
		return nil, err
	}

	ipAddr := net.ParseIP(ip)
	if ipAddr == nil {
		return nil, fmt.Errorf("could not parse valid IP from: %s", ip)
	}

	return NewFromAddr(addr, ipAddr, v, cred)
}

// GetMac returns the PMC MAC address.
func (pmc *PMC) GetMac() net.HardwareAddr {
	return pmc.MAC
}

// GetIp returns the PMC IP address.
func (pmc *PMC) GetIp() net.IP {
	return pmc.IP
}

// GetVendor returns the PMC vendor.
func (pmc *PMC) GetVendor() vendor.Vendor {
	return pmc.Vendor
}

// GetCredential returns the PMC credential or nil.
func (pmc *PMC) GetCredential() *credential.Credential {
	return pmc.Credential
}

// SetCredential sets the credential for the PMC.
func (pmc *PMC) SetCredential(cred *credential.Credential) {
	if cred != nil {
		nc := *cred
		pmc.Credential = &nc
	}
}

// SetIP sets the IP address for the PMC.
func (pmc *PMC) SetIP(ip string) {
	pmc.IP = net.ParseIP(ip)
}

// SetVendor sets the Vendor for the PMC.
func (pmc *PMC) SetVendor(v vendor.VendorCode) error {
	vendor := vendor.CodeToVendor(v)
	if err := vendor.IsSupported(); err != nil {
		return err
	}

	pmc.Vendor = vendor
	return nil
}

// Patch updates the PMC instance with the values from another PMC instance.
func (pmc *PMC) Patch(to PMC) bool {
	patched := false

	if strings.Compare(pmc.MAC.String(), to.MAC.String()) == 0 {
		if to.IP != nil && !pmc.IP.Equal(to.IP) {
			pmc.IP = to.IP
			patched = true
		}

		if pmc.Credential.Patch(to.Credential) {
			patched = true
		}
	}

	return patched
}
