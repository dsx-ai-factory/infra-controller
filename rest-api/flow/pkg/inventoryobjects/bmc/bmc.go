// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bmc

import (
	"net"
	"regexp"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
)

// BMC specifies the information for a BMC which includes MAC address, IP
// address, and access credential.
type BMC struct {
	MAC        MACAddress             `json:"mac"`
	IP         net.IP                 `json:"ip"`
	Credential *credential.Credential `json:"credential"`
}

// New creates a new BMC instance with the given MAC address, credential and
// IP address.
func New(s string, cred *credential.Credential, ip string) (*BMC, error) {
	if len(s) == 12 {
		// MAC address with no seperators, add ":"
		re := regexp.MustCompile(`(..)(..)(..)(..)(..)(..)`)
		s = re.ReplaceAllString(strings.ToLower(s), "$1:$2:$3:$4:$5:$6")
	}

	addr, err := net.ParseMAC(s)
	if err != nil {
		return nil, err
	}

	bmcInfo := &BMC{MAC: MACAddress{HardwareAddr: addr}}

	if cred != nil {
		nc := *cred
		bmcInfo.Credential = &nc
	}

	bmcInfo.IP = net.ParseIP(ip)

	return bmcInfo, nil
}

// SetCredential sets the credential for the BMC.
func (b *BMC) SetCredential(cred *credential.Credential) {
	if cred != nil {
		nc := *cred
		b.Credential = &nc
	}
}

// SetIP sets the IP address for the BMC.
func (b *BMC) SetIP(ip string) {
	b.IP = net.ParseIP(ip)
}

// Patch updates the BMC instance with the values from another BMC instance.
func (b *BMC) Patch(nb BMC) bool {
	patched := false

	if strings.Compare(b.MAC.String(), nb.MAC.String()) == 0 {
		if nb.IP != nil && !b.IP.Equal(nb.IP) {
			b.IP = nb.IP
			patched = true
		}

		if b.Credential.Patch(nb.Credential) {
			patched = true
		}
	}

	return patched
}
