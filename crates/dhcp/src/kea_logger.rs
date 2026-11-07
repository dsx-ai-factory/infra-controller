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
use std::ffi::CString;

use libc::{c_char, c_int};
use log::{Level, Metadata, Record};

pub(super) struct KeaLogger;

// Kea stops at level DEBUG, but splits that into 100 'debuglevel' values, so arbitrarity assign
// one to Log::Debug and one to Log::Trace.
const KEA_DEBUGLEVEL_DEBUG: c_int = 10;
const KEA_DEBUGLEVEL_TRACE: c_int = 99;

unsafe extern "C" {
    fn kea_log_is_debug_enabled(debuglevel: c_int) -> bool;
    fn kea_log_is_info_enabled() -> bool;
    fn kea_log_is_warn_enabled() -> bool;
    fn kea_log_is_error_enabled() -> bool;

    // 'level' is kea config loggers/debuglevel
    fn kea_log_generic_debug(level: c_int, _: *const c_char);
    fn kea_log_generic_info(_: *const c_char);
    fn kea_log_generic_warn(_: *const c_char);
    fn kea_log_generic_error(_: *const c_char);
}

impl log::Log for KeaLogger {
    // Delegates the question to Kea. This allows us to configure logging entirely through Kea's
    // config.
    //
    // Manually map Rust 'log' levels (log/src/lib.rs) to Kea Severity (logger_level.h) because
    // the enum int values don't match and run in different directions.
    fn enabled(&self, metadata: &Metadata) -> bool {
        // SAFETY: Initial lint enablement: this C++ boundary needs owner review.
        // The linked matching-ABI probes access only Kea's process-lifetime
        // logger and must not unwind into Rust.
        unsafe {
            use Level::*;
            match metadata.level() {
                Trace => kea_log_is_debug_enabled(KEA_DEBUGLEVEL_TRACE),
                Debug => kea_log_is_debug_enabled(KEA_DEBUGLEVEL_DEBUG),
                Info => kea_log_is_info_enabled(),
                Warn => kea_log_is_warn_enabled(),
                Error => kea_log_is_error_enabled(),
            }
        }
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let text = CString::new(format!(
                "{}:{}:{} - {}",
                record.file().unwrap_or("<no file>"),
                record.line().unwrap_or(0),
                record.target(),
                record.args()
            ))
            .unwrap();
            let text_ptr = text.as_ptr();

            // SAFETY: Initial lint enablement: this C++ boundary needs owner
            // review. `text_ptr` remains NUL-terminated and live until the
            // synchronous matching-ABI logger call returns; C++ only reads it
            // and must not unwind into Rust.
            unsafe {
                use Level::*;
                match record.metadata().level() {
                    Trace => kea_log_generic_debug(KEA_DEBUGLEVEL_TRACE, text_ptr),
                    Debug => kea_log_generic_debug(KEA_DEBUGLEVEL_DEBUG, text_ptr),
                    Info => kea_log_generic_info(text_ptr),
                    Warn => kea_log_generic_warn(text_ptr),
                    Error => kea_log_generic_error(text_ptr),
                }
            }
        }
    }

    fn flush(&self) {}
}
