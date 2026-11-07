// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"crypto/rand"
	"encoding/binary"
	"fmt"
	"net"
	"time"

	mrand "math/rand"
)

func generateMacAddress() string {
	buf := make([]byte, 6)
	rand.Read(buf)

	// Set the local bit
	buf[0] |= 2
	maca := fmt.Sprintf("Random MAC address: %02x:%02x:%02x:%02x:%02x:%02x\n", buf[0], buf[1], buf[2], buf[3], buf[4], buf[5])

	return maca
}

func generateInteger(max int) int {
	s := mrand.NewSource(time.Now().UnixNano())
	r := mrand.New(s)

	return r.Intn(max)
}

func generateIPAddress() string {
	buf := make([]byte, 4)

	ip := mrand.Uint32()
	binary.LittleEndian.PutUint32(buf, ip)

	return fmt.Sprintf("%s\n", net.IP(buf))
}

func getStrPtr(s string) *string {
	sp := s
	return &sp
}
