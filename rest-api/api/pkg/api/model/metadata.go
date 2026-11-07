// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/metadata"
)

// APIMetadata is a data structure to capture NICo API system information
type APIMetadata struct {
	// Version contains the API version
	Version string `json:"version"`
	// BuildTime contains the time the binary was built
	BuildTime string `json:"buildTime"`
}

// NewAPIMetadata creates and returns a new APISystemInfo object
func NewAPIMetadata() *APIMetadata {
	amd := &APIMetadata{
		Version:   metadata.Version,
		BuildTime: metadata.BuildTime,
	}

	return amd
}
