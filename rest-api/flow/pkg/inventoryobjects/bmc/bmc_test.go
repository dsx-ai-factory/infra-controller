// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bmc

import (
	"net"
	"testing"

	"github.com/stretchr/testify/assert"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
)

func TestNew(t *testing.T) {
	mac := "001122334455"
	ip := "192.168.1.100"
	cred := credential.New("admin", "password")

	bmc, err := New(mac, &cred, ip)
	assert.NoError(t, err)
	assert.Equal(t, "00:11:22:33:44:55", bmc.MAC.String())
	assert.Equal(t, ip, bmc.IP.String())
	assert.Equal(t, &cred, bmc.Credential)
}

func TestSetCredential(t *testing.T) {
	bmc := &BMC{}
	cred := credential.New("admin", "password")

	bmc.SetCredential(&cred)
	assert.Equal(t, &cred, bmc.Credential)
}

func TestSetIP(t *testing.T) {
	bmc := &BMC{}
	ip := "192.168.1.100"

	bmc.SetIP(ip)
	assert.Equal(t, ip, bmc.IP.String())
}

func TestPatch(t *testing.T) {
	macs := []string{"00:11:22:33:44:55", "66:77:88:99:00:11"}
	ips := []string{"192.168.1.100", "192.168.1.101"}
	creds := []credential.Credential{
		credential.New("admin", "a-password"),
		credential.New("root", "r-password"),
	}

	credPtr := func(cred credential.Credential) *credential.Credential {
		return &cred
	}

	hwAddrs := make([]net.HardwareAddr, 0, len(macs))
	for _, m := range macs {
		hwAddr, err := net.ParseMAC(m)
		assert.NoError(t, err)

		hwAddrs = append(hwAddrs, hwAddr)
	}

	parsedIPs := make([]net.IP, 0, len(ips))
	for _, ip := range ips {
		parsedIPs = append(parsedIPs, net.ParseIP(ip))
	}

	bmc := BMC{MAC: MACAddress{HardwareAddr: hwAddrs[0]}, IP: parsedIPs[0], Credential: credPtr(creds[0])}

	// Test case 1: no patch if the mac is different
	nb0 := BMC{MAC: MACAddress{HardwareAddr: hwAddrs[1]}, IP: parsedIPs[1], Credential: credPtr(creds[1])}
	patched := bmc.Patch(nb0)
	assert.False(t, patched)
	assert.Equal(t, parsedIPs[0], bmc.IP)
	assert.Equal(t, &creds[0], bmc.Credential)

	// Test case 2: patched on the changed information
	nb1 := BMC{MAC: MACAddress{HardwareAddr: hwAddrs[0]}, IP: parsedIPs[1], Credential: credPtr(creds[1])}
	patched = bmc.Patch(nb1)
	assert.True(t, patched)
	assert.Equal(t, parsedIPs[1], bmc.IP)
	assert.Equal(t, &creds[1], bmc.Credential)
}
