// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"testing"

	"github.com/stretchr/testify/assert"

	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestStripTaskReports(t *testing.T) {
	t.Run("clears report on each non-nil task", func(t *testing.T) {
		tasks := []*pb.Task{
			{Id: &pb.UUID{Id: "a"}, Message: "running", Report: `{"version":1}`},
			{Id: &pb.UUID{Id: "b"}, Message: "failed", Report: `{"version":1,"stages":[]}`},
		}

		stripTaskReports(tasks)

		for _, tk := range tasks {
			assert.Empty(t, tk.GetReport())
		}
		// Other fields must survive so callers still see status / message.
		assert.Equal(t, "running", tasks[0].GetMessage())
		assert.Equal(t, "failed", tasks[1].GetMessage())
	})

	t.Run("tolerates nil entries", func(t *testing.T) {
		tasks := []*pb.Task{nil, {Id: &pb.UUID{Id: "c"}, Report: "{}"}}

		assert.NotPanics(t, func() { stripTaskReports(tasks) })
		assert.Empty(t, tasks[1].GetReport())
	})

	t.Run("empty slice is a no-op", func(t *testing.T) {
		assert.NotPanics(t, func() { stripTaskReports(nil) })
		assert.NotPanics(t, func() { stripTaskReports([]*pb.Task{}) })
	})
}
