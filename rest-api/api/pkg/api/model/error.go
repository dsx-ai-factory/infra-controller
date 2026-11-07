// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

// common validation errors
const (
	validationErrorValueRequired                  = "a value is required"
	validationErrorInvalidUUID                    = "must be a valid UUID"
	validationErrorStringLength                   = "must be at least 2 characters and maximum 256 characters"
	validationErrorDescriptionStringLength        = "maximum 1024 characters are allowed in description"
	validationErrorMachineMaintenanceStringLength = "must be at least 5 characters and maximum 256 characters"
	validationErrorInvalidIPAddress               = "invalid IP address"
	validationErrorInvalidIPv4Address             = "invalid IPv4 address"
	validationErrorInvalidHostname                = "invalid hostname"
	validationErrorInvalidIPv6Address             = "invalid IPv6 address"
	validationErrorStringLength64                 = "must be at least 2 characters and maximum 64 characters"

	validationCommonErrorField = "__all__"
)
