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

#include <csignal>

#include <asiolink/io_address.h>
#include <asiolink/io_error.h>
#include <hooks/hooks.h>
#include <hooks/server_hooks.h>
#include <log/logger.h>
#include <log/macros.h>

#include "carbide_logger.h"
#include "callouts.h"
#include "carbide_rust.h"

isc::log::Logger loader_logger("kea-shim-loader");

using namespace isc::hooks;
using namespace isc::data;

static_assert(KEA_HOOKS_VERSION >= 20200,
              "carbide-dhcp requires Kea 2.2 or newer");

using StringSetter = void (*)(const char *);
using ValidatedStringSetter = bool (*)(const char *);
using BoolSetter = void (*)(bool);

enum class KeaDhcpFamily { V4, V6, Unsupported };

KeaDhcpFamily configured_family() {
  const auto &hooks = ServerHooks::getServerHooks();
  const bool has_v4_hooks = hooks.findIndex("pkt4_receive") >= 0;
  const bool has_v6_hooks = hooks.findIndex("pkt6_receive") >= 0;

  // Each Kea daemon registers only its family-specific packet hook set.
  if (has_v4_hooks && !has_v6_hooks) {
    return KeaDhcpFamily::V4;
  }
  if (has_v6_hooks && !has_v4_hooks) {
    return KeaDhcpFamily::V6;
  }
  return KeaDhcpFamily::Unsupported;
}

bool set_string_parameter(LibraryHandle *handle, const char *name,
                          StringSetter setter) {
  ConstElementPtr value = handle->getParameter(name);
  if (!value) {
    return true;
  }
  if (value->getType() != Element::string) {
    LOG_ERROR(loader_logger, "Invalid type for hook parameter %1").arg(name);
    return false;
  }

  setter(value->stringValue().c_str());
  return true;
}

bool set_validated_string_parameter(LibraryHandle *handle, const char *name,
                                    ValidatedStringSetter setter) {
  ConstElementPtr value = handle->getParameter(name);
  if (!value) {
    return true;
  }
  if (value->getType() != Element::string) {
    LOG_ERROR(loader_logger, "Invalid type for hook parameter %1").arg(name);
    return false;
  }

  if (!setter(value->stringValue().c_str())) {
    LOG_ERROR(loader_logger, "Invalid value for hook parameter %1").arg(name);
    return false;
  }
  return true;
}

bool set_bool_parameter(LibraryHandle *handle, const char *name,
                        BoolSetter setter) {
  ConstElementPtr value = handle->getParameter(name);
  if (!value) {
    return true;
  }
  if (value->getType() != Element::boolean) {
    LOG_ERROR(loader_logger, "Invalid type for hook parameter %1").arg(name);
    return false;
  }

  setter(value->boolValue());
  return true;
}

bool configure_common(LibraryHandle *handle) {
  // Both Kea daemons need the Carbide API and metrics endpoint.
  return set_string_parameter(handle, "carbide-api-url", carbide_set_config_api) &&
         set_validated_string_parameter(handle, "carbide-metrics-endpoint",
                                        carbide_set_config_metrics_endpoint);
}

bool configure_v4(LibraryHandle *handle) {
  ConstElementPtr next_server =
      handle->getParameter("carbide-provisioning-server-ipv4");
  if (next_server) {
    if (next_server->getType() != Element::string) {
      LOG_ERROR(loader_logger, "Invalid type for hook parameter %1")
          .arg("carbide-provisioning-server-ipv4");
      return false;
    }
    try {
      auto nextserver_ipv4 =
          isc::asiolink::IOAddress(next_server->stringValue());

      if (!nextserver_ipv4.isV4()) {
        LOG_ERROR(loader_logger, isc::log::LOG_CARBIDE_INVALID_NEXTSERVER_IPV4)
            .arg("");
        return false;
      }
      carbide_set_config_next_server_ipv4(nextserver_ipv4.toUint32());
    } catch (const isc::asiolink::IOError &e) {
      LOG_ERROR(loader_logger, isc::log::LOG_CARBIDE_INVALID_NEXTSERVER_IPV4)
          .arg(e.getMessage());
      return false;
    }
  }

  // Existing DHCPv4 params keep their carbide-* names for compatibility.
  if (!set_string_parameter(handle, "carbide-ntpserver", carbide_set_config_ntp) ||
      !set_string_parameter(handle, "carbide-nameservers",
                            carbide_set_config_name_servers) ||
      !set_string_parameter(handle, "carbide-mqtt-server",
                            carbide_set_config_mqtt_server)) {
    return false;
  }

  handle->registerCallout("pkt4_receive", pkt4_receive);
  // lease4_select fires between pkt4_receive and pkt4_send, and is the
  // only place where we can override the IP that Kea will persist into
  // its lease memfile.
  handle->registerCallout("lease4_select", lease4_select);
  // lease4_renew is the renewal-time side of lease4_select.
  handle->registerCallout("lease4_renew", lease4_renew);
  handle->registerCallout("pkt4_send", pkt4_send);
  handle->registerCallout("lease4_expire", lease4_expire);
  return true;
}

// Return the first statically configured subnet whose effective Rapid Commit
// setting is disabled. Kea has already applied defaults and inheritance to the
// JSON tree before invoking dhcp6_srv_configured.
ConstElementPtr disabled_rapid_commit_subnet(ConstElementPtr config) {
  const auto find_disabled = [](ConstElementPtr subnets) -> ConstElementPtr {
    if (!subnets) {
      return ConstElementPtr();
    }

    for (const auto &subnet : subnets->listValue()) {
      const auto rapid_commit = subnet->get("rapid-commit");
      if (!rapid_commit || !rapid_commit->boolValue()) {
        return subnet;
      }
    }
    return ConstElementPtr();
  };

  auto disabled = find_disabled(config->get("subnet6"));
  if (disabled) {
    return disabled;
  }

  const auto shared_networks = config->get("shared-networks");
  if (shared_networks) {
    for (const auto &network : shared_networks->listValue()) {
      disabled = find_disabled(network->get("subnet6"));
      if (disabled) {
        return disabled;
      }
    }
  }

  return ConstElementPtr();
}

// Reject configurations where Core would commit a SOLICIT but Kea would only
// advertise a fake allocation. Kea 2.2+ propagates DROP as a config error.
extern "C" int dhcp6_srv_configured(CalloutHandle &handle) {
  if (!hook_get_config_rapid_commit_v6()) {
    return 0;
  }

  ConstElementPtr config;
  handle.getArgument("json_config", config);
  const auto disabled = disabled_rapid_commit_subnet(config);
  if (!disabled) {
    return 0;
  }

  const auto prefix = disabled->get("subnet");
  const std::string subnet = prefix ? prefix->stringValue() : "<unknown>";
  const std::string error =
      "hook-rapid-commit-v6 requires Kea rapid-commit to be enabled for subnet " +
      subnet;
  LOG_ERROR(loader_logger, "%1").arg(error);
  handle.setArgument("error", error);
  handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
  return 0;
}

bool configure_v6(LibraryHandle *handle) {
  // New DHCPv6 hook params intentionally use hook-* names.
  if (!set_validated_string_parameter(handle, "hook-dns-servers-ipv6",
                                      hook_set_config_dns_servers_ipv6) ||
      !set_validated_string_parameter(handle, "hook-ntp-servers-ipv6",
                                      hook_set_config_ntp_servers_ipv6) ||
      !set_validated_string_parameter(
          handle, "hook-provisioning-server-ipv6",
          hook_set_config_provisioning_server_ipv6) ||
      !set_bool_parameter(handle, "hook-rapid-commit-v6",
                          hook_set_config_rapid_commit_v6)) {
    return false;
  }

  // Validate after Kea applies subnet defaults and inheritance. This avoids
  // depending on Kea headers that Debian does not publish as a complete set.
  handle->registerCallout("dhcp6_srv_configured", dhcp6_srv_configured);

  handle->registerCallout("pkt6_receive", pkt6_receive);
  handle->registerCallout("lease6_select", lease6_select);
  handle->registerCallout("lease6_renew", lease6_renew);
  handle->registerCallout("lease6_rebind", lease6_rebind);
  handle->registerCallout("pkt6_send", pkt6_send);
  handle->registerCallout("lease6_expire", lease6_expire);
  return true;
}

extern "C" {
int shim_version() {
  return KEA_HOOKS_VERSION;
}

int shim_load(void *handle_ptr) {
  if (!handle_ptr) {
    LOG_INFO(loader_logger, isc::log::LOG_CARBIDE_INVALID_HANDLE);
    return 1;
  }

  LibraryHandle *handle = static_cast<LibraryHandle *>(handle_ptr);

  LOG_INFO(loader_logger, isc::log::LOG_CARBIDE_INITIALIZATION);

  // Rust installs SIG_IGN for SIGPIPE in lang_start, which runs only for Rust
  // binaries. This hook is a cdylib loaded into Kea, so the disposition stays
  // SIG_DFL and a write to a closed Carbide API socket terminates the server.
  signal(SIGPIPE, SIG_IGN);

  auto family = configured_family();
  // Family-specific validation must happen before common metrics startup,
  // because the metrics thread is process-lifetime once started.
  if (family == KeaDhcpFamily::V4) {
    if (!configure_v4(handle)) {
      return 1;
    }
  } else if (family == KeaDhcpFamily::V6) {
    if (!configure_v6(handle)) {
      return 1;
    }
  } else {
    LOG_ERROR(loader_logger, "Unsupported Kea DHCP hook set");
    return 1;
  }

  // TODO(ajf): add config options for mutual TLS authentication to the API
  if (!configure_common(handle)) {
    return 1;
  }

  return 0;
}

int shim_unload() {
  return 0;
}

int shim_multi_threading_compatible() {
  return (1);
}
}
