// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

// APIHealthCheck is a data structure to capture NICo API health information
type APIHealthCheck struct {
	// IsHealthy provides a flag to accompany an error status code
	IsHealthy bool `json:"is_healthy"`
	// Error contains an error message in case of health issues
	Error *string `json:"error"`
}

// NewAPIHealthCheck creates and returns a new APIHealthCheck object
func NewAPIHealthCheck(isHealthy bool, errorMessage *string) *APIHealthCheck {
	ahc := &APIHealthCheck{
		IsHealthy: isHealthy,
		Error:     errorMessage,
	}

	return ahc
}
