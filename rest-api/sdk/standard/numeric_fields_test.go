// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"encoding/json"
	"math"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestNumericFieldsDecodeFullDeclaredRange(t *testing.T) {
	var rotation CredentialRotationStatus
	require.NoError(t, json.Unmarshal([]byte(`{"targetVersion":4294967295,"converged":18446744073709551615,"pending":0,"quarantined":0,"complete":true}`), &rotation))
	require.Equal(t, uint32(math.MaxUint32), rotation.TargetVersion)
	require.Equal(t, uint64(math.MaxUint64), rotation.Converged)
}
