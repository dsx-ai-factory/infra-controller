// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package db

// ErrorChecker abstracts database error classification.
type ErrorChecker interface {
	IsErrNoRows(err error) bool
	IsUniqueConstraintError(err error) bool
}
