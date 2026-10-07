// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	swu "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
)

// TestInventoryDefaultScheduleIsReportable pins the default against the two rules the rest of the
// system applies to a configured schedule. Config validation skips an unset schedule, so a default
// that broke either rule would leave the Site collecting on it while Cloud fell back to assuming
// DefaultInventoryReceiptInterval.
func TestInventoryDefaultScheduleIsReportable(t *testing.T) {
	interval, err := swu.InventoryIntervalFromSchedule(InventoryDefaultSchedule)
	require.NoError(t, err, "the default schedule must carry a derivable interval")
	assert.LessOrEqual(t, interval, cutil.MaxInventoryReceiptInterval,
		"the default schedule must not be slower than the supported maximum")
}
