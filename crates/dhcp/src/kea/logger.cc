/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#include <log/logger.h>
#include <log/macros.h>

#include "carbide_logger.h"

isc::log::Logger ffi_logger("carbide-rust");

extern "C" {
	bool kea_log_is_debug_enabled(int debuglevel) {
		return ffi_logger.isDebugEnabled(debuglevel);
	}
	bool kea_log_is_info_enabled() {
		return ffi_logger.isInfoEnabled();
	}
	bool kea_log_is_warn_enabled() {
		return ffi_logger.isWarnEnabled();
	}
	bool kea_log_is_error_enabled() {
		return ffi_logger.isErrorEnabled();
	}

	void kea_log_generic_debug(int level, const char* message) {
		LOG_DEBUG(ffi_logger, level, isc::log::LOG_CARBIDE_GENERIC).arg(message);
	}
	void kea_log_generic_info(const char* message) {
		LOG_INFO(ffi_logger, isc::log::LOG_CARBIDE_GENERIC).arg(message);
	}
	void kea_log_generic_warn(const char* message) {
		LOG_WARN(ffi_logger, isc::log::LOG_CARBIDE_GENERIC).arg(message);
	}
	void kea_log_generic_error(const char* message) {
		LOG_ERROR(ffi_logger, isc::log::LOG_CARBIDE_GENERIC).arg(message);
	}
}
