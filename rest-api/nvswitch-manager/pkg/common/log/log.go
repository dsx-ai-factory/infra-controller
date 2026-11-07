// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package log

import (
	"sync"

	log "github.com/sirupsen/logrus"
)

var initLogOnce sync.Once

// InitLog applies to the default log (logrus) functionality.
// Each process that wants to send its logs to a file by default should call InitLog only once.
func Init() {
	ok := false
	initLogOnce.Do(func() { ok = true })
	if !ok {
		panic("InitLog may only be run once")
	}

	log.SetFormatter(&JSONFormatter{})
	log.SetReportCaller(true)
}
