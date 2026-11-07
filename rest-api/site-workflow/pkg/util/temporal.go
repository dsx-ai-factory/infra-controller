// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"go.temporal.io/sdk/converter"
)

// NewTemporalDataConverter returns the data converter every Site Agent Temporal client uses.
// The inventory pager measures a page against Temporal's blob size limit with this same
// converter, so the two cannot disagree on how large a page is on the wire. ProtoJSON
// precedes the binary proto converter, so protos travel as JSON and measure larger.
func NewTemporalDataConverter() converter.DataConverter {
	return converter.NewCompositeDataConverter(
		converter.NewNilPayloadConverter(),
		converter.NewByteSlicePayloadConverter(),
		converter.NewProtoJSONPayloadConverterWithOptions(converter.ProtoJSONPayloadConverterOptions{
			AllowUnknownFields: true,
		}),
		converter.NewProtoPayloadConverter(),
		converter.NewJSONPayloadConverter(),
	)
}
