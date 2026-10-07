// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dps

import (
	"encoding/binary"
	"fmt"
	"strings"

	"github.com/google/uuid"
)

// ExternalIDFromVPCID derives a stable positive int64 DPS external_id from a
// VPC UUID.
func ExternalIDFromVPCID(vpcID string) (int64, error) {
	id, err := uuid.Parse(strings.TrimSpace(vpcID))
	if err != nil {
		return 0, fmt.Errorf("parse VPC ID %q: %w", vpcID, err)
	}

	externalID := int64(binary.BigEndian.Uint64(id[:8]) & (1<<63 - 1))
	if externalID == 0 {
		externalID = 1
	}
	return externalID, nil
}
