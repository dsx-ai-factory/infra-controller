// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"

type pauseDecision struct {
	pause   bool
	reason  operationrun.OperationRunStatusReason
	message string
}
