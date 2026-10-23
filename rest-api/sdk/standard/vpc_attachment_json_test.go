// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestVpcAttachment_UnmarshalJSON(t *testing.T) {
	// A direct VPC selection on a physical function without device metadata, so every
	// nullable required field is null.
	payload := `{"id":"07cd865e-5512-4fe9-8063-8f4c8f442b1a","vpcId":"497f6eca-6276-4993-bfeb-53cbbbba6f08","vpcPrefixId":null,"instanceId":"37804b21-0a00-4668-81cb-af9e1b07ad3e","instanceTenantId":"f97df110-f4de-492e-8849-4a6af68026b0","device":null,"deviceInstance":null,"isPhysical":true,"virtualFunctionId":null,"status":"Ready","created":"2019-08-24T14:15:22Z","updated":"2019-08-24T14:15:22Z"}`
	nullableFields := []string{"vpcPrefixId", "device", "deviceInstance", "virtualFunctionId"}

	t.Run("accepts and preserves required nulls", func(t *testing.T) {
		var attachment VpcAttachment
		require.NoError(t, json.Unmarshal([]byte(payload), &attachment))
		encoded, err := json.Marshal(attachment)
		require.NoError(t, err)
		var fields map[string]any
		require.NoError(t, json.Unmarshal(encoded, &fields))
		for _, field := range nullableFields {
			require.Contains(t, fields, field)
			require.Nil(t, fields[field])
		}
	})
	for _, field := range append([]string{"vpcId"}, nullableFields...) {
		t.Run("rejects missing "+field, func(t *testing.T) {
			var fields map[string]any
			require.NoError(t, json.Unmarshal([]byte(payload), &fields))
			delete(fields, field)
			encoded, err := json.Marshal(fields)
			require.NoError(t, err)
			require.ErrorContains(t, json.Unmarshal(encoded, &VpcAttachment{}), "required property "+field)
		})
	}
}
