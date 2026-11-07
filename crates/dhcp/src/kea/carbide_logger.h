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

// File created from carbide_logger.mes

#ifndef CARBIDE_LOGGER_H
#define CARBIDE_LOGGER_H

#include <log/message_types.h>

namespace isc {
namespace log {

extern const isc::log::MessageID LOG_CARBIDE_GENERIC;
extern const isc::log::MessageID LOG_CARBIDE_INITIALIZATION;
extern const isc::log::MessageID LOG_CARBIDE_INVALID_HANDLE;
extern const isc::log::MessageID LOG_CARBIDE_INVALID_NEXTSERVER_IPV4;
extern const isc::log::MessageID LOG_CARBIDE_LEASE4_SELECT;
extern const isc::log::MessageID LOG_CARBIDE_LEASE4_RENEW;
extern const isc::log::MessageID LOG_CARBIDE_LEASE_EXPIRE;
extern const isc::log::MessageID LOG_CARBIDE_LEASE_EXPIRE_ERROR;
extern const isc::log::MessageID LOG_CARBIDE_PKT4_RECEIVE;
extern const isc::log::MessageID LOG_CARBIDE_PKT4_SEND;

} // namespace log
} // namespace isc

#endif // CARBIDE_LOGGER_H
