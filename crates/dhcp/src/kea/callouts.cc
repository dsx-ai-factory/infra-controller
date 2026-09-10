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

#include "callouts.h"
#include "carbide_rust.h"
#include <dhcp/option6_ia.h>
#include <dhcp/option6_iaaddr.h>
#include <dhcp/option6_status_code.h>

isc::log::Logger logger("carbide-callouts");

const int IPV4_ADDR_SIZEB = 4;
const int IPV6_ADDR_SIZEB = 16;

const uint8_t *nullable_data(const OptionBuffer &buffer) {
  return buffer.empty() ? nullptr : buffer.data();
}

// Own a Rust DHCP byte buffer for the duration of C++ option construction.
class DhcpByteBufferGuard {
public:
  explicit DhcpByteBufferGuard(DhcpByteBuffer buffer) : buffer_(buffer) {}
  ~DhcpByteBufferGuard() { machine_free_dhcp_byte_buffer(buffer_); }
  DhcpByteBufferGuard(const DhcpByteBufferGuard &) = delete;
  DhcpByteBufferGuard &operator=(const DhcpByteBufferGuard &) = delete;

  bool empty() const { return buffer_.len == 0 || buffer_.ptr == nullptr; }

  OptionBuffer optionBuffer() const {
    return OptionBuffer(buffer_.ptr, buffer_.ptr + buffer_.len);
  }

private:
  DhcpByteBuffer buffer_;
};

// Extract a DHCPv6 relay-option payload for the Rust decoder, returning an
// empty buffer when Kea did not retain that relay metadata.
OptionBuffer option_data(const Pkt6::RelayInfo &relay, uint16_t code) {
  auto option = relay.options_.find(code);
  if (option == relay.options_.end() || !option->second) {
    return OptionBuffer();
  }
  return option->second->getData();
}

void add_or_replace_option6(Pkt6Ptr response6_ptr, uint16_t code,
                            DhcpByteBuffer buffer) {
  DhcpByteBufferGuard guard(buffer);

  // Delete first so an intentionally empty trusted value removes any
  // client-derived option already present on Kea's response.
  response6_ptr->delOption(code);
  if (guard.empty()) {
    return;
  }

  OptionBuffer payload = guard.optionBuffer();
  response6_ptr->addOption(OptionPtr(new Option(Option::V6, code, payload)));
}

void add_or_replace_option6(Pkt6Ptr response6_ptr, uint16_t code,
                            const OptionBuffer &buffer) {
  if (buffer.empty()) {
    return;
  }

  response6_ptr->delOption(code);
  response6_ptr->addOption(OptionPtr(new Option(Option::V6, code, buffer)));
}

void add_client_fqdn_option6(Pkt6Ptr query6_ptr, Pkt6Ptr response6_ptr,
                             Machine *machine) {
  response6_ptr->delOption(D6O_CLIENT_FQDN);

  OptionPtr requested =
      query6_ptr ? query6_ptr->getOption(D6O_CLIENT_FQDN) : OptionPtr();
  if (!requested || requested->getData().empty()) {
    return;
  }

  DhcpByteBuffer buffer = machine_get_client_fqdn_ipv6(machine);
  DhcpByteBufferGuard guard(buffer);
  if (guard.empty()) {
    return;
  }

  OptionBuffer payload = guard.optionBuffer();
  // Preserve client negotiation flags, but keep the API-owned hostname.
  payload[0] = requested->getData()[0];
  response6_ptr->addOption(
      OptionPtr(new Option(Option::V6, D6O_CLIENT_FQDN, payload)));
}

void add_status6(Pkt6Ptr response6_ptr, uint16_t status,
                 const std::string &message) {
  response6_ptr->delOption(D6O_STATUS_CODE);
  response6_ptr->addOption(
      OptionPtr(new Option6StatusCode(status, message)));
}

void add_ia_na_status6(Pkt6Ptr query6_ptr, Pkt6Ptr response6_ptr,
                       uint16_t status, const std::string &message) {
  uint32_t iaid = 0;
  if (query6_ptr) {
    OptionPtr option = query6_ptr->getOption(D6O_IA_NA);
    Option6IAPtr query_ia_na =
        boost::dynamic_pointer_cast<Option6IA>(option);
    if (query_ia_na) {
      iaid = query_ia_na->getIAID();
    }
  }

  // Replace Kea's old-address IA_NA success with an IA-scoped failure.
  response6_ptr->delOption(D6O_STATUS_CODE);
  response6_ptr->delOption(D6O_IA_NA);
  Option6IAPtr ia_na(new Option6IA(D6O_IA_NA, iaid));
  ia_na->addOption(OptionPtr(new Option6StatusCode(status, message)));
  response6_ptr->addOption(ia_na);
}

void record_dropped_v6_request(const char *reason) {
  // DHCPv6 has its own metric family; the unsuffixed counter remains DHCPv4.
  carbide_increment_dropped_v6_requests(reason);
}

/// Records a DHCPv6 response that Kea is still allowed to send.
void record_v6_reply_sent(CalloutHandle &handle, Pkt6Ptr response6_ptr) {
  // Count only responses Kea will be allowed to put on the wire.
  if (response6_ptr && handle.getStatus() != CalloutHandle::NEXT_STEP_DROP) {
    carbide_increment_v6_reply_sent(response6_ptr->getType());
  }
}

void CDHCPOptionsHandler<Option>::resetOption(boost::any param) {
  switch (option) {
  case DHO_SUBNET_MASK:
    option_val.reset(new OptionInt<uint32_t>(
        Option::V4, option,
        machine_get_interface_subnet_mask(boost::any_cast<Machine *>(param))));
    break;
  case DHO_BROADCAST_ADDRESS:
    option_val.reset(new OptionInt<uint32_t>(
        Option::V4, option,
        machine_get_broadcast_address(boost::any_cast<Machine *>(param))));
    break;
  case DHO_HOST_NAME: {
    char *hostname =
        machine_get_interface_hostname(boost::any_cast<Machine *>(param));
    option_val.reset(new OptionString(Option::V4, option, hostname));
    machine_free_fqdn(hostname);
  } break;
  case DHO_BOOT_FILE_NAME: {
    // if client does not support netboot we get a null pointer
    const char *filename =
        machine_get_filename(boost::any_cast<Machine *>(param));
    if (filename) {
      option_val.reset(new OptionString(Option::V4, option, filename));
      machine_free_filename(filename);
    }
  } break;
  case DHO_VENDOR_CLASS_IDENTIFIER:
    option_val.reset(new OptionString(Option::V4, DHO_VENDOR_CLASS_IDENTIFIER,
                                      boost::any_cast<char *>(param)));
    break;
  default:
    LOG_ERROR(logger, "LOG_CARBIDE_PKT4_SEND: packet send error: Option [%1] "
                      "is not implemented for reset.")
        .arg(option);
  }
}

Option4AddrLst::AddressContainer getAddresses(std::string ips) {
  std::stringstream ss(ips);
  std::vector<isc::asiolink::IOAddress> out;
  char delim = ',';

  std::string s;
  while (std::getline(ss, s, delim)) {
    out.push_back(isc::asiolink::IOAddress(s));
  }

  return out;
}

void CDHCPOptionsHandler<Option>::resetAndAddOption(boost::any param) {
  switch (option) {
  case DHO_ROUTERS:
    response4_ptr->addOption(OptionPtr(new Option4AddrLst(
        option, isc::asiolink::IOAddress(machine_get_interface_router(
                    boost::any_cast<Machine *>(param))))));
    break;
  case DHO_NAME_SERVERS:
    response4_ptr->addOption(OptionPtr(new Option4AddrLst(
        option, getAddresses(boost::any_cast<std::string>(param)))));
    break;
  case DHO_DOMAIN_NAME_SERVERS:
    response4_ptr->addOption(OptionPtr(new Option4AddrLst(
        option, getAddresses(boost::any_cast<std::string>(param)))));
    break;
  case DHO_NTP_SERVERS:
    response4_ptr->addOption(OptionPtr(new Option4AddrLst(
        option, getAddresses(boost::any_cast<std::string>(param)))));
    break;
  case DHO_MQTT_SERVER:
    response4_ptr->addOption(OptionPtr(new OptionString(
        Option::V4, option, boost::any_cast<std::string>(param))));
    break;
  case DHO_SUBNET_MASK:
  case DHO_BROADCAST_ADDRESS:
  case DHO_HOST_NAME:
  case DHO_BOOT_FILE_NAME:
  case DHO_VENDOR_CLASS_IDENTIFIER:
    resetOption(param);
    if (option_val) {
      response4_ptr->addOption(option_val);
    }
    break;
  case DHO_INTERFACE_MTU:
	response4_ptr->delOption(DHO_INTERFACE_MTU);
	response4_ptr->addOption(OptionPtr(new OptionInt<uint16_t>(Option::V4, DHO_INTERFACE_MTU, boost::any_cast<uint16_t>(param))));
	break;
  default:
    LOG_ERROR(logger, "LOG_CARBIDE_PKT4_SEND: packet send error: Option [%1] "
                      "is not implemented for addandreset.")
        .arg(option);
  }
}

/*
 * The main function which updates the option in response4_ptr.
 * Currently as per implementation only Option and OptionUint16 templates are
 * implemented.
 */
template <typename T>
void update_option(CalloutHandle &handle, Pkt4Ptr response4_ptr,
                   const int option, boost::any param) {
  try {
    CDHCPOptionsHandler<T> option_handler(handle, response4_ptr, option);
    option_handler.resetAndAddOption(param);
  } catch (exception &e) {
    LOG_ERROR(logger, "LOG_CARBIDE_PKT4_SEND: packet send Exception for option "
                      "[%1]. Exception: %2")
        .arg(option)
        .arg(e.what());
    // Several options are updated per reply and each failing update throws;
    // count the packet's drop once, on the throw that marks it dropped.
    if (handle.getStatus() != CalloutHandle::NEXT_STEP_DROP) {
      carbide_increment_dropped_requests("OptionEncodingFailed");
    }
    handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
  }
}

DiscoveryBuilderResult update_discovery_parameters_option82(
    DiscoveryBuilderFFI *discovery, int option,
    boost::shared_ptr<OptionCustom> option_val) {
  switch (option) {
  case RAI_OPTION_LINK_SELECTION: {
    OptionPtr link_select = option_val->getOption(RAI_OPTION_LINK_SELECTION);
    if (link_select) {
      OptionBuffer link_select_buf = link_select->getData();
      if (link_select_buf.size() == sizeof(uint32_t)) {
        uint32_t option_select =
            isc::asiolink::IOAddress::fromBytes(AF_INET, &link_select_buf[0])
                .toUint32();
        // Update link select address.
        return discovery_set_link_select(discovery, option_select);
      }
    }
    break;
  }
  case RAI_OPTION_AGENT_CIRCUIT_ID: {
    OptionPtr circuit_id_opt =
        option_val->getOption(RAI_OPTION_AGENT_CIRCUIT_ID);
    if (circuit_id_opt) {
      OptionBuffer circuit_id = circuit_id_opt->getData();
      std::string circuit_value(circuit_id.begin(), circuit_id.end());
      LOG_INFO(logger, "LOG_CARBIDE_PKT4_RECEIVE: CIRCUIT ID [%1] in packet")
          .arg(circuit_value);
      return discovery_set_circuit_id(discovery, circuit_value.c_str());
    }
    break;
  }
  case RAI_OPTION_REMOTE_ID: {
    OptionPtr remote_id_opt = option_val->getOption(RAI_OPTION_REMOTE_ID);
    if (remote_id_opt) {
      OptionBuffer remote_id = remote_id_opt->getData();
      std::string remote_value(remote_id.begin(), remote_id.end());
      LOG_INFO(logger, "LOG_CARBIDE_PKT4_RECEIVE: REMOTE ID [%1] in packet")
          .arg(remote_value);
      return discovery_set_remote_id(discovery, remote_value.c_str());
    }
    break;
  }
  }

  return DiscoveryBuilderResult::Success;
}

DiscoveryBuilderResult
update_discovery_parameters(DiscoveryBuilderFFI *discovery, int option,
                            boost::shared_ptr<OptionCustom> option_val) {

  DiscoveryBuilderResult ret_val;
  switch (option) {
  case DHO_DHCP_AGENT_OPTIONS:
    ret_val = update_discovery_parameters_option82(
        discovery, RAI_OPTION_LINK_SELECTION, option_val);
    if (ret_val != DiscoveryBuilderResult::Success) {
      LOG_ERROR(
          logger,
          "LOG_CARBIDE_PKT4_RECEIVE: Failed in handling link select address.");
      return ret_val;
    }

    ret_val = update_discovery_parameters_option82(
        discovery, RAI_OPTION_AGENT_CIRCUIT_ID, option_val);
    if (ret_val != DiscoveryBuilderResult::Success) {
      LOG_ERROR(logger,
                "LOG_CARBIDE_PKT4_RECEIVE: Failed in handling circuit_id.");
      return ret_val;
    }

    ret_val = update_discovery_parameters_option82(
        discovery, RAI_OPTION_REMOTE_ID, option_val);
    if (ret_val != DiscoveryBuilderResult::Success) {
      LOG_ERROR(logger,
                "LOG_CARBIDE_PKT4_RECEIVE: Failed in handling remote_id.");
      return ret_val;
    }
    break;
  }

  return DiscoveryBuilderResult::Success;
}

DiscoveryBuilderResult
update_discovery_parameters(DiscoveryBuilderFFI *discovery, int option,
                            boost::shared_ptr<OptionString> option_val) {
  switch (option) {
  case DHO_VENDOR_CLASS_IDENTIFIER:
    return discovery_set_vendor_class(discovery,
                                      option_val->getValue().c_str());
  }

  return DiscoveryBuilderResult::Success;
}

DiscoveryBuilderResult
update_discovery_parameters(DiscoveryBuilderFFI *discovery, int option,
                            boost::shared_ptr<OptionUint16Array> option_val) {
  switch (option) {
  case DHO_SYSTEM: {
    const auto &architectures = option_val->getValues();
    if (!architectures.empty()) {
      return discovery_set_client_system(discovery, architectures.front());
    }
    break;
  }
  }

  return DiscoveryBuilderResult::Success;
}

template <typename T>
DiscoveryBuilderResult
update_discovery_parameters(Pkt4Ptr query4_ptr, DiscoveryBuilderFFI *discovery,
                            int option) {
  boost::shared_ptr<T> option_val =
      boost::dynamic_pointer_cast<T>(query4_ptr->getOption(option));
  if (option_val) {
    LOG_INFO(logger, isc::log::LOG_CARBIDE_GENERIC).arg(option_val->toText());
    return update_discovery_parameters(discovery, option, option_val);
  } else {
    if (option != DHO_DHCP_AGENT_OPTIONS) {
      // TODO: Does this mean we rather should return an error here?
      LOG_ERROR(logger,
                "LOG_CARBIDE_PKT4_RECEIVE: Missing option [%1] in packet")
          .arg(option);
    }
  }

  return DiscoveryBuilderResult::Success;
}

void set_options(CalloutHandle &handle, Pkt4Ptr response4_ptr,
                 Machine *machine) {
  // Router Address
  update_option<Option>(handle, response4_ptr, DHO_ROUTERS, machine);

  // DNS servers
  char *machine_nameservers = machine_get_nameservers(machine);
  std::string nameservers(machine_nameservers);
  update_option<Option>(handle, response4_ptr, DHO_NAME_SERVERS, nameservers);
  update_option<Option>(handle, response4_ptr, DHO_DOMAIN_NAME_SERVERS,
                        nameservers);
  machine_free_nameservers(machine_nameservers);

  // NTP server
  char *machine_ntpservers = machine_get_ntpservers(machine);
  std::string ntpservers(machine_ntpservers);
  update_option<Option>(handle, response4_ptr, DHO_NTP_SERVERS, ntpservers);
  machine_free_nameservers(machine_ntpservers);

  // MQTT server
  char *machine_mqtt_server = machine_get_mqtt_server(machine);
  if (machine_mqtt_server != nullptr) {
    std::string mqtt_server(machine_mqtt_server);
    update_option<Option>(handle, response4_ptr, DHO_MQTT_SERVER, mqtt_server);
    machine_free_nameservers(machine_mqtt_server);
  }

  // Set Interface MTU
  uint16_t mtu = machine_get_interface_mtu(machine);
  update_option<Option>(handle, response4_ptr, DHO_INTERFACE_MTU, mtu);

  // Set subnet-mask
  update_option<Option>(handle, response4_ptr, DHO_SUBNET_MASK, machine);

  // Set broadcast address
  update_option<Option>(handle, response4_ptr, DHO_BROADCAST_ADDRESS, machine);

  // Set hostname, the RFC says this is the short name, but whatever.
  update_option<Option>(handle, response4_ptr, DHO_HOST_NAME, machine);

  // Set filename
  update_option<Option>(handle, response4_ptr, DHO_BOOT_FILE_NAME, machine);

  char *machine_client_type = machine_get_client_type(machine);
  if (strlen(machine_client_type) > 0) {
    update_option<Option>(handle, response4_ptr, DHO_VENDOR_CLASS_IDENTIFIER,
                          machine_client_type);
  }
  machine_free_client_type(machine_client_type);
}

void set_options_v6(CalloutHandle &handle, Pkt6Ptr query6_ptr,
                    Pkt6Ptr response6_ptr, Machine *machine) {
  try {
    // DNS servers: DHCPv6 option 23 is a flat IPv6 address list.
    add_or_replace_option6(response6_ptr, D6O_NAME_SERVERS,
                           machine_get_dns_servers_ipv6(machine));

    // Domain search: Rust returns RFC 1035 wire-format domain names.
    add_or_replace_option6(response6_ptr, D6O_DOMAIN_SEARCH,
                           machine_get_domain_search_ipv6(machine));

    // NTP servers: Rust returns RFC 5908 suboption TLVs for option 56.
    add_or_replace_option6(response6_ptr, 56,
                           machine_get_ntp_servers_ipv6(machine));

    // Client FQDN is client-controlled; echo negotiation flags only and
    // render the API-owned hostname. This cannot use add_or_replace_option6
    // because the response payload depends on the client's requested flags.
    add_client_fqdn_option6(query6_ptr, response6_ptr, machine);

    if (machine_get_rapid_commit_v6(machine)) {
      response6_ptr->delOption(D6O_RAPID_COMMIT);
      response6_ptr->addOption(OptionPtr(new Option(Option::V6, D6O_RAPID_COMMIT)));
    }
  } catch (exception &e) {
    LOG_ERROR(logger, "LOG_CARBIDE_PKT6_SEND: packet send Exception: %1")
        .arg(e.what());
    handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
  }
}

void set_vendor_options(Pkt4Ptr response4_ptr) {
  OptionPtr option_vendor(
      new Option(Option::V4, DHO_VENDOR_ENCAPSULATED_OPTIONS));
  LOG_INFO(logger, isc::log::LOG_CARBIDE_GENERIC).arg(option_vendor->toText());

  // Option 6 set to 0x8 tells iPXE not to wait for Proxy PXE since we don't
  // care about that.
  OptionPtr vendor_option_6 = option_vendor->getOption(6);
  if (vendor_option_6) {
    option_vendor->delOption(6);
  }
  vendor_option_6.reset(new OptionInt<uint32_t>(Option::V4, 6, 0x8));
  option_vendor->addOption(vendor_option_6);

  response4_ptr->addOption(option_vendor);
}

// Resolve the authoritative Carbide IPv6 address cached on the DHCPv6 exchange.
int get_carbide_lease6_address(CalloutHandle &handle, Lease6Ptr lease6,
                               const std::string &hook_name,
                               isc::asiolink::IOAddress &carbide_addr) {
  if (!lease6) {
    LOG_ERROR(logger, "LOG_CARBIDE_LEASE6_OVERRIDE: %1")
        .arg(hook_name + ": Missing lease6 argument");
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    return 1;
  }

  boost::shared_ptr<Machine> machine;
  try {
    handle.getContext("machine", machine);
  } catch (...) {
    machine.reset();
  }
  if (!machine) {
    LOG_ERROR(logger, "LOG_CARBIDE_LEASE6_OVERRIDE: %1")
        .arg(hook_name + ": Missing machine object from handle context");
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    return 1;
  }

  uint8_t carbide_bytes[IPV6_ADDR_SIZEB] = {0};
  if (!machine_get_interface_address_ipv6(machine.get(), carbide_bytes,
                                          IPV6_ADDR_SIZEB)) {
    LOG_ERROR(logger, "LOG_CARBIDE_LEASE6_OVERRIDE: %1")
        .arg(hook_name + ": Carbide returned no usable IPv6 address");
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    return 1;
  }

  carbide_addr =
      isc::asiolink::IOAddress::fromBytes(AF_INET6, carbide_bytes);
  return 0;
}

// Ensure lease expiry uses the hook-selected MAC identity.
int set_lease6_selected_hwaddr(CalloutHandle &handle, Lease6Ptr lease6,
                               const std::string &hook_name) {
  if (!lease6) {
    LOG_ERROR(logger, "LOG_CARBIDE_LEASE6_OVERRIDE: %1")
        .arg(hook_name + ": Missing lease6 argument");
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    return 1;
  }

  boost::shared_ptr<Machine> machine;
  try {
    handle.getContext("machine", machine);
  } catch (...) {
    machine.reset();
  }
  if (!machine) {
    LOG_ERROR(logger, "LOG_CARBIDE_LEASE6_OVERRIDE: %1")
        .arg(hook_name + ": Missing machine object from handle context");
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    return 1;
  }

  if (machine_is_invalidated_v6_lease(machine.get())) {
    LOG_WARN(logger, "LOG_CARBIDE_LEASE6_OVERRIDE: %1")
        .arg(hook_name + ": refusing recently expired DHCPv6 lease");
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    return 1;
  }

  char *selected_mac = machine_get_discovery_mac(machine.get());
  if (selected_mac == nullptr) {
    LOG_ERROR(logger, "LOG_CARBIDE_LEASE6_OVERRIDE: %1")
        .arg(hook_name + ": Missing selected DHCPv6 MAC");
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    return 1;
  }

  // Kea already has the hook-selected identity; leave the lease unchanged.
  if (lease6->hwaddr_ && !lease6->hwaddr_->hwaddr_.empty() &&
      lease6->hwaddr_->toText(false) == selected_mac) {
    machine_free_discovery_mac(selected_mac);
    return 0;
  }

  // If DUID fallback recovers the same selected MAC, expiry can stay scoped
  // without forcing a synthetic hwaddr into leases that Kea left empty.
  if ((!lease6->hwaddr_ || lease6->hwaddr_->hwaddr_.empty()) &&
      lease6->duid_) {
    const auto &duid = lease6->duid_->getDuid();
    char *duid_mac = carbide_mac_from_duid(nullable_data(duid), duid.size());
    if (duid_mac != nullptr && std::string(duid_mac) == selected_mac) {
      carbide_free_mac_string(duid_mac);
      machine_free_discovery_mac(selected_mac);
      return 0;
    }
    if (duid_mac != nullptr) {
      carbide_free_mac_string(duid_mac);
    }
  }

  try {
    HWAddr hwaddr = HWAddr::fromText(selected_mac, HTYPE_ETHER);
    lease6->hwaddr_.reset(new HWAddr(hwaddr));
    handle.setArgument("lease6", lease6);
  } catch (...) {
    LOG_ERROR(logger, "LOG_CARBIDE_LEASE6_OVERRIDE: %1")
        .arg(hook_name + ": Failed to persist selected DHCPv6 MAC");
    machine_free_discovery_mac(selected_mac);
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    return 1;
  }

  machine_free_discovery_mac(selected_mac);
  return 0;
}

// Decode the API-selected IPv6 address from a Machine context.
bool machine_ipv6_address(Machine *machine,
                          isc::asiolink::IOAddress &carbide_addr) {
  uint8_t carbide_bytes[IPV6_ADDR_SIZEB] = {0};
  if (!machine_get_interface_address_ipv6(machine, carbide_bytes,
                                          IPV6_ADDR_SIZEB)) {
    return false;
  }

  carbide_addr =
      isc::asiolink::IOAddress::fromBytes(AF_INET6, carbide_bytes);
  return true;
}

// Return true when Kea is about to send an address that is not API-owned.
// This can happen when Kea reuses an existing IA_NA after renew/rebind refused
// an unsupported address migration.
bool response6_has_stale_ia_na_address(Pkt6Ptr response6_ptr,
                                       const isc::asiolink::IOAddress &carbide_addr,
                                       std::string &detail) {
  OptionPtr ia_option = response6_ptr->getOption(D6O_IA_NA);
  if (!ia_option) {
    return false;
  }

  Option6IAPtr ia_na = boost::dynamic_pointer_cast<Option6IA>(ia_option);
  if (!ia_na) {
    detail = "IA_NA option has unexpected type";
    return true;
  }

  OptionPtr iaaddr_option = ia_na->getOption(D6O_IAADDR);
  if (!iaaddr_option) {
    return false;
  }

  Option6IAAddrPtr iaaddr =
      boost::dynamic_pointer_cast<Option6IAAddr>(iaaddr_option);
  if (!iaaddr) {
    detail = "IAADDR option has unexpected type";
    return true;
  }

  if (iaaddr->getAddress() != carbide_addr) {
    detail = "Kea response address " + iaaddr->getAddress().toText() +
             " does not match Carbide address " + carbide_addr.toText();
    return true;
  }

  return false;
}

// Override a proposed DHCPv6 allocation before Kea persists a new lease.
int override_lease6_address(CalloutHandle &handle, Lease6Ptr lease6,
                            const std::string &hook_name,
                            const std::string &detail) {
  isc::asiolink::IOAddress carbide_addr("::");
  if (get_carbide_lease6_address(handle, lease6, hook_name, carbide_addr) !=
      0) {
    return 1;
  }

  if (set_lease6_selected_hwaddr(handle, lease6, hook_name) != 0) {
    return 1;
  }

  if (lease6->addr_ != carbide_addr) {
    LOG_INFO(logger, "LOG_CARBIDE_LEASE6_OVERRIDE: %1")
        .arg(hook_name + ": overriding " + lease6->addr_.toText() + " -> " +
             carbide_addr.toText() + detail);
    lease6->addr_ = carbide_addr;
    handle.setArgument("lease6", lease6);
  } else {
    LOG_INFO(logger, "LOG_CARBIDE_LEASE6_OVERRIDE: %1")
        .arg(hook_name + ": lease addr already matches Carbide (" +
             carbide_addr.toText() + ")");
  }

  return 0;
}

// Refuse unsupported in-place migration of an existing DHCPv6 lease address.
int refuse_lease6_address_migration(CalloutHandle &handle, Lease6Ptr lease6,
                                    const std::string &hook_name) {
  isc::asiolink::IOAddress carbide_addr("::");
  if (get_carbide_lease6_address(handle, lease6, hook_name, carbide_addr) !=
      0) {
    return 1;
  }

  // RENEW/REBIND extends an existing address-indexed lease. Changing addr_
  // here asks Kea to update a lease under a different key, which memfile
  // rejects.
  if (lease6->addr_ != carbide_addr) {
    LOG_WARN(logger, "LOG_CARBIDE_LEASE6_OVERRIDE: %1")
        .arg(hook_name + ": refusing to migrate existing lease " +
             lease6->addr_.toText() + " -> " + carbide_addr.toText());
    // Kea's public hook surface does not expose a safe lease re-key here.
    // Drop instead of sending a successful renewal for the API-stale address.
    handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
    return 1;
  }

  // Even steady-state renew/rebind can have Kea's configured mac-sources copy
  // a DUID-derived hwaddr onto the lease. Restore the hook-selected identity.
  if (set_lease6_selected_hwaddr(handle, lease6, hook_name) != 0) {
    return 1;
  }

  LOG_INFO(logger, "LOG_CARBIDE_LEASE6_OVERRIDE: %1")
      .arg(hook_name + ": lease addr already matches Carbide (" +
           carbide_addr.toText() + ")");
  return 0;
}

// Notify Carbide when Kea reclaims a DHCPv6 lease that still has a scoped
// client identity.
int handle_carbide_lease6_end(CalloutHandle &handle,
                              const std::string &hook_name) {
  Lease6Ptr lease6;
  handle.getArgument("lease6", lease6);

  if (!lease6) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_LEASE_EXPIRE_ERROR)
        .arg(hook_name + ": missing lease6 argument");
    return 0;
  }

  std::string ip_str = lease6->addr_.toText();
  // DHCPv6 identifies clients by DUID, but Kea still records the
  // client's hardware address on the lease when available.
  std::string mac_str;
  if (lease6->hwaddr_ && !lease6->hwaddr_->hwaddr_.empty()) {
    mac_str = lease6->hwaddr_->toText(false);
  } else if (lease6->duid_) {
    // DHCPv6 leases commonly have no hwaddr_; DUID-LL/LLT still lets NICo
    // scope expiry to the same (ip, mac) pair as DHCPv4.
    const auto &duid = lease6->duid_->getDuid();
    char *duid_mac = carbide_mac_from_duid(nullable_data(duid), duid.size());
    if (duid_mac != nullptr) {
      mac_str = duid_mac;
      carbide_free_mac_string(duid_mac);
    }
  }
  LOG_INFO(logger, isc::log::LOG_CARBIDE_LEASE_EXPIRE)
      .arg(hook_name + ": " + ip_str);

  // Address-only expiry is unsafe for hook-originated DHCPv6 reclamation:
  // Kea can remove client identity from declined leases before expiry.
  if (mac_str.empty()) {
    LOG_WARN(logger, "LOG_CARBIDE_LEASE6_EXPIRE: %1")
        .arg(hook_name + ": skipping API lease expiry for " + ip_str +
             " because Kea did not provide a client MAC");
    return 0;
  }

  size_t invalidated =
      carbide_invalidate_v6_lease_cache(ip_str.c_str(), mac_str.c_str());
  auto result = carbide_expire_lease(ip_str.c_str(), mac_str.c_str());
  if (result == LeaseExpirationResult::FeatureDisabled) {
    // The API is still authoritative when expiry handling is disabled, so the
    // pre-call tombstone must not block a subsequent API-selected lease.
    carbide_clear_v6_lease_cache_invalidation(ip_str.c_str(),
                                              mac_str.c_str());
  } else if (result != LeaseExpirationResult::InvalidAddress) {
    // Keep the tombstone even on API errors: the API may have committed the
    // deletion before a response was lost.
    invalidated +=
        carbide_invalidate_v6_lease_cache(ip_str.c_str(), mac_str.c_str());
  }
  if (result != LeaseExpirationResult::Success &&
      result != LeaseExpirationResult::FeatureDisabled) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_LEASE_EXPIRE_ERROR)
        .arg(hook_name + ": " + ip_str);
  }
  LOG_INFO(logger, "LOG_CARBIDE_LEASE6_EXPIRE: %1")
      .arg(hook_name + ": invalidated " + std::to_string(invalidated) +
           " DHCPv6 lease cache entries for " + ip_str);

  return 0;
}

extern "C" {
int pkt4_receive(CalloutHandle &handle) {
  Pkt4Ptr query4_ptr;

  handle.getArgument("query4", query4_ptr);

  LOG_INFO(logger, isc::log::LOG_CARBIDE_PKT4_RECEIVE)
      .arg(query4_ptr->toText());

  /*
   * Call to increment total requests counter
   */
  carbide_increment_total_requests();

  /*
   * We only work on relayed packets (i.e. we never provide DHCP
   * for the network in which this daemon is running.
   */
  if (!query4_ptr || !query4_ptr->isRelayed()) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_PKT4_RECEIVE)
        .arg("Received a non-relayed packet, dropping it");
    handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
    /*
     * Call to increment drooped requests counter
     */
    carbide_increment_dropped_requests("NonRelayedPacket");
    return 0;
  }

  LOG_INFO(logger, "LOG_CARBIDE_PKT4_RECEIVE: Packet type name: %1")
	  .arg(query4_ptr->getName());

  // Initialize a discovery builder object
  // Since the object needs to be freed using a Rust function, we wrap it in
  // a unique_ptr with a custom deleter
  std::unique_ptr<DiscoveryBuilderFFI, void (*)(DiscoveryBuilderFFI *)>
      discovery(discovery_builder_allocate(), discovery_builder_free);

  /*
   * Extract the DHO_DHCP_AGENT_OPTIONS (82) from request and check if Suboption
   * 5: RAI_OPTION_LINK_SELECTION (RFC3527) and 1: RAI_OPTION_AGENT_CIRCUIT_ID
   * (RFC3527) are present or not.
   */
  DiscoveryBuilderResult builder_result =
      update_discovery_parameters<OptionCustom>(query4_ptr, discovery.get(),
                                                DHO_DHCP_AGENT_OPTIONS);
  /*
   * Extract the vendor class, which has some interesting bits
   * like HTTPClient / PXEClient
   *
   * TODO(ajf): find out where this option format is documented
   * at all so maybe we can build a type around it.
   */
  if (builder_result == DiscoveryBuilderResult::Success) {
    builder_result = update_discovery_parameters<OptionString>(
        query4_ptr, discovery.get(), DHO_VENDOR_CLASS_IDENTIFIER);
  }

  if (builder_result == DiscoveryBuilderResult::Success) {
    OptionPtr opt = query4_ptr->getOption(DHO_DHCP_REQUESTED_ADDRESS);
    if (opt) {
      OptionBuffer buf = opt->getData();
      auto bufSize = buf.size();

      if (bufSize == IPV4_ADDR_SIZEB) {
        uint32_t temp = 0;
        memcpy(&temp, buf.data(), IPV4_ADDR_SIZEB);
        uint32_t v4 = htonl(temp);

        isc::asiolink::IOAddress addr(v4);

        auto desired = addr.toText();

        builder_result =
            discovery_set_desired_address(discovery.get(), desired.c_str());

        if (builder_result == DiscoveryBuilderResult::Success) {
          LOG_INFO(logger,
                  "LOG_CARBIDE_PKT4_RECEIVE: Desired Address [%1] set")
            .arg(desired);
        }
      } else {
        LOG_ERROR(logger, "LOG_CARBIDE_PKT4_RECEIVE: Desired addr buf len wrong: [%1]")
          .arg(bufSize);
      }
    }
  }

  /*
   * Extract the "client architecture" - DHCP option 93 from the
   * packet, which will tell us what the booting architecture is
   * in order to figure out which filname to give back
   */
  if (builder_result == DiscoveryBuilderResult::Success) {
    builder_result = update_discovery_parameters<OptionUint16Array>(
        query4_ptr, discovery.get(), DHO_SYSTEM);
  }

  /*
   * There's helper functions for the basic stuff like mac
   * address and relay address
   */
  if (builder_result == DiscoveryBuilderResult::Success) {
    builder_result = discovery_set_relay(discovery.get(),
                                         query4_ptr->getGiaddr().toUint32());
  }

  if (builder_result == DiscoveryBuilderResult::Success) {
    auto mac = query4_ptr->getHWAddr()->hwaddr_;
    builder_result =
        discovery_set_mac_address(discovery.get(), mac.data(), mac.size());
  }

  Machine *machine = nullptr;
  if (builder_result == DiscoveryBuilderResult::Success) {
    /*
     * We've been building up a object for the dhcp client options
     * we care about, so now we call the function to turn that
     * object into a dhcp machine object from the carbide API.
     */
    builder_result = discovery_fetch_machine(discovery.get(), &machine);
  }

  if (builder_result != DiscoveryBuilderResult::Success || machine == nullptr) {
    LOG_ERROR(logger,
              "LOG_CARBIDE_PKT4_RECV: Error while executing machine discovery "
              "in discovery_fetch_machine: %1, machine_ptr=%2")
        .arg(discovery_builder_result_as_str(builder_result))
        .arg(machine);
    handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
    /*
     * Call to increment drooped requests counter
     */
    carbide_increment_dropped_requests(discovery_builder_result_as_str(builder_result));
    return 1;
  }

  /*
   * machine_get_interface_address returns the IPv4 address as a u32 in
   * network byte order, or 0 if Carbide didn't return a parseable IPv4
   * address. 0.0.0.0 is not a valid allocation, and pkt4_receive is the
   * packet-level hook where NEXT_STEP_DROP reliably stops processing before
   * Kea can select, renew, or persist a lease.
   */
  if (machine_get_interface_address(machine) == 0) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_PKT4_RECEIVE)
        .arg("Carbide returned no usable IPv4 address; dropping packet");
    machine_free(machine);
    handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
    carbide_increment_dropped_requests("NoUsableIPv4Address");
    return 1;
  }

  // On success, we set the pointer to the machine in the request context to
  // be retrieved later
  boost::shared_ptr<Machine> machinePtr(machine, [](Machine *ptr) {
    // Tell rust code to free the memory, since memory allocated in Rust can't
    // be freed with a native `delete` or `free`.
    // By wrapping this in the `shared_ptr`, we make sure KEA always releases
    // the handle when it's done with the request
    machine_free(ptr);
  });
  handle.setContext("machine", machinePtr);
  return 0;
}

int pkt4_send(CalloutHandle &handle) {
  Pkt4Ptr query4_ptr, response4_ptr;

  handle.getArgument("query4", query4_ptr);
  handle.getArgument("response4", response4_ptr);

  /*
   * Load the machine from the context.  It should have been set in
   * pkt4_receive.
   */
  boost::shared_ptr<Machine> machine;
  handle.getContext("machine", machine);
  if (!machine) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_PKT4_SEND)
        .arg("Missing machine object from handle context");
    handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
    carbide_increment_dropped_requests("MissingMachineContext");
    return 1;
  }

  /*
   * Fetch the interface address for this machine (i.e. this is the address
   * assigned to the DHCP-ing host.
   */
  response4_ptr->setYiaddr(
      isc::asiolink::IOAddress(machine_get_interface_address(machine.get())));

  set_options(handle, response4_ptr, machine.get());

  // Set next-server (Siaddr) - server address
  response4_ptr->setSiaddr(
      isc::asiolink::IOAddress(machine_get_next_server(machine.get())));

  /*
   * Encapsulate some PXE options in the vendor encapsulated
   */
  set_vendor_options(response4_ptr);

  LOG_INFO(logger, isc::log::LOG_CARBIDE_PKT4_SEND)
      .arg(response4_ptr->toText());

  /*
   * The reply is fully assembled; count it by DHCP message type unless an
   * option failure above already marked the exchange dropped (and counted).
   */
  if (handle.getStatus() != CalloutHandle::NEXT_STEP_DROP) {
    carbide_increment_reply_sent(response4_ptr->getType());
  }

  return 0;
}

int lease4_expire(CalloutHandle &handle) {
  Lease4Ptr lease4;
  handle.getArgument("lease4", lease4);

  if (!lease4) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_LEASE_EXPIRE_ERROR)
        .arg("missing lease4 argument");
    return 0;
  }

  std::string ip_str = lease4->addr_.toText();
  // Pass the MAC alongside the IP so NICo can scope the delete to
  // exactly this (ip, mac) lease.
  std::string mac_str;
  if (lease4->hwaddr_ && !lease4->hwaddr_->hwaddr_.empty()) {
    mac_str = lease4->hwaddr_->toText(false);
  }
  LOG_INFO(logger, isc::log::LOG_CARBIDE_LEASE_EXPIRE).arg(ip_str);

  auto result = carbide_expire_lease(
      ip_str.c_str(), mac_str.empty() ? nullptr : mac_str.c_str());
  if (result != LeaseExpirationResult::Success &&
      result != LeaseExpirationResult::FeatureDisabled) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_LEASE_EXPIRE_ERROR).arg(ip_str);
  }

  return 0;
}

int lease4_select(CalloutHandle &handle) {
  /*
   * lease4_select fires for both DHCPDISCOVER (fake_allocation=true) and
   * DHCPREQUEST (fake_allocation=false). For DISCOVER the lease is built
   * for the OFFER response but is not persisted to memfile. For REQUEST it
   * is persisted.
   *
   * Either way, we want to take Kea's proposed lease and replace its address
   * with the one Carbide allocated. The Machine was stashed on the callout
   * handle context in pkt4_receive, so it's already cached.
   */
  Lease4Ptr lease4;
  handle.getArgument("lease4", lease4);

  // Get the rest of the hook arguments for context. We only really need
  // them for logging here, but `fake_allocation` is also relevant if we
  // ever want different behavior between DISCOVER and REQUEST.
  bool fake_allocation = false;
  try {
    handle.getArgument("fake_allocation", fake_allocation);
  } catch (...) {
    // Some Kea versions may not always pass this, that's fine.
  }

  if (!lease4) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_LEASE4_SELECT)
        .arg("Missing lease4 argument");
    // At lease4_select, SKIP means Kea will not assign its selected lease.
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    carbide_increment_dropped_requests("AllocationRefused");
    return 1;
  }

  // Load the Machine cached in pkt4_receive. If it's missing, pkt4_receive
  // either failed or wasn't called for this exchange; in either case we
  // can't authoritatively assign an address, so fail closed.
  //
  // (pkt4_receive would normally have already set NEXT_STEP_DROP in this
  // failure path, so the fact that we got here at all suggests something
  // unusual; we still want to defend Kea's memfile against accepting an
  // un-authorized allocation.)
  boost::shared_ptr<Machine> machine;
  handle.getContext("machine", machine);
  if (!machine) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_LEASE4_SELECT)
        .arg("Missing machine object from handle context");
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    carbide_increment_dropped_requests("AllocationRefused");
    return 1;
  }

  // machine_get_interface_address returns the IPv4 address as a u32 in
  // network byte order, or 0 if Carbide didn't return a parseable IPv4
  // address. 0.0.0.0 is not a valid allocation, so treat it as a failure.
  uint32_t carbide_u32 = machine_get_interface_address(machine.get());
  if (carbide_u32 == 0) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_LEASE4_SELECT)
        .arg("Carbide returned no usable IPv4 address; refusing to allocate");
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    carbide_increment_dropped_requests("AllocationRefused");
    return 1;
  }

  isc::asiolink::IOAddress carbide_addr(carbide_u32);

  // If Kea's allocator already picked the same address Carbide returned,
  // no override needed -- but the common case is that they differ (Kea's
  // allocator is bidding from the 0.0.0.0/0 pool independently of what
  // Carbide chose).
  if (lease4->addr_ != carbide_addr) {
    LOG_INFO(logger, isc::log::LOG_CARBIDE_LEASE4_SELECT)
        .arg(std::string("overriding ") + lease4->addr_.toText() + " -> " +
             carbide_addr.toText() +
             (fake_allocation ? " (DISCOVER, not persisted)"
                              : " (REQUEST, will persist)"));
    lease4->addr_ = carbide_addr;
    // Push the modified lease back. Lease4Ptr is a shared_ptr so mutating
    // through it already affects Kea's copy, but calling setArgument is
    // explicit about our intent and survives any future Kea changes to
    // how it tracks lease-object mutation.
    handle.setArgument("lease4", lease4);
  } else {
    LOG_INFO(logger, isc::log::LOG_CARBIDE_LEASE4_SELECT)
        .arg(std::string("lease addr already matches Carbide (") +
             carbide_addr.toText() + ")");
  }

  return 0;
}

int lease4_renew(CalloutHandle &handle) {
  /*
   * lease4_renew fires when Kea is extending an existing lease, i.e. a
   * DHCPREQUEST in RENEWING (T1 expired, unicast) or REBINDING (T2
   * expired, broadcast) state. Unlike lease4_select there's no
   * `fake_allocation` distinction -- renewals are always persisted.
   *
   * Our goal here is the same as lease4_select: keep Kea's memfile lease
   * address aligned with whatever Carbide currently considers the
   * binding for this MAC. In the common case (Carbide's allocation is
   * stable) this is a no-op; the interesting case is when an operator
   * has changed `machine_interfaces.address` while a lease is live, and
   * we want the memfile to track the change rather than drift.
   */
  Lease4Ptr lease4;
  handle.getArgument("lease4", lease4);

  if (!lease4) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_LEASE4_RENEW)
        .arg("Missing lease4 argument");
    // At lease4_renew, SKIP means Kea will not update the lease database.
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    carbide_increment_dropped_requests("RenewalRefused");
    return 1;
  }

  boost::shared_ptr<Machine> machine;
  handle.getContext("machine", machine);
  if (!machine) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_LEASE4_RENEW)
        .arg("Missing machine object from handle context");
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    carbide_increment_dropped_requests("RenewalRefused");
    return 1;
  }

  uint32_t carbide_u32 = machine_get_interface_address(machine.get());
  if (carbide_u32 == 0) {
    LOG_ERROR(logger, isc::log::LOG_CARBIDE_LEASE4_RENEW)
        .arg("Carbide returned no usable IPv4 address; refusing to renew");
    handle.setStatus(CalloutHandle::NEXT_STEP_SKIP);
    carbide_increment_dropped_requests("RenewalRefused");
    return 1;
  }

  isc::asiolink::IOAddress carbide_addr(carbide_u32);

  if (lease4->addr_ != carbide_addr) {
    LOG_INFO(logger, isc::log::LOG_CARBIDE_LEASE4_RENEW)
        .arg(std::string("overriding ") + lease4->addr_.toText() + " -> " +
             carbide_addr.toText() + " on renewal");
    lease4->addr_ = carbide_addr;
    handle.setArgument("lease4", lease4);
  } else {
    LOG_INFO(logger, isc::log::LOG_CARBIDE_LEASE4_RENEW)
        .arg(std::string("renewing, lease addr already matches Carbide (") +
             carbide_addr.toText() + ")");
  }

  return 0;
}

int pkt6_receive(CalloutHandle &handle) {
  Pkt6Ptr query6_ptr;
  handle.getArgument("query6", query6_ptr);

  carbide_increment_v6_requests(query6_ptr ? query6_ptr->getType() : 0);

  if (!query6_ptr) {
    LOG_ERROR(logger, "LOG_CARBIDE_PKT6_RECEIVE: missing query6 argument");
    handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
    record_dropped_v6_request("invalid_packet");
    return 1;
  }

  LOG_INFO(logger, "LOG_CARBIDE_PKT6_RECEIVE: %1")
      .arg(query6_ptr->toText());

  size_t relay_count = query6_ptr->relay_info_.size();
  if (relay_count == 0) {
    LOG_ERROR(logger, "LOG_CARBIDE_PKT6_RECEIVE: Received a non-relayed packet, dropping it");
    handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
    record_dropped_v6_request("NonRelayedPacket");
    return 0;
  }

  uint8_t hop_count = 0;
  OptionBuffer relay_link;
  OptionBuffer interface_id;
  OptionBuffer remote_id;
  OptionBuffer client_link_layer;

  const auto &relay = query6_ptr->relay_info_.front();
  hop_count = relay.hop_count_;
  relay_link = relay.linkaddr_.toBytes();
  interface_id = option_data(relay, D6O_INTERFACE_ID);
  remote_id = option_data(relay, D6O_REMOTE_ID);
  client_link_layer = option_data(relay, D6O_CLIENT_LINKLAYER_ADDR);

  Machine *machine = nullptr;
  V6HookResult result = carbide_pkt6_receive(
      nullable_data(query6_ptr->data_), query6_ptr->data_.size(), relay_count,
      hop_count, nullable_data(relay_link), relay_link.size(),
      nullable_data(interface_id), interface_id.size(), nullable_data(remote_id),
      remote_id.size(), nullable_data(client_link_layer),
      client_link_layer.size(), &machine);

  if (result == V6HookResult::Success && machine != nullptr) {
    boost::shared_ptr<Machine> machinePtr(machine, [](Machine *ptr) {
      machine_free(ptr);
    });
    handle.setContext("machine", machinePtr);
    // CONFIRM cache hits must be visible as top-level Success in pkt6_send.
    if (query6_ptr->getType() == DHCPV6_CONFIRM) {
      handle.setContext("confirm_on_link", true);
    }
    return 0;
  }

  if (result == V6HookResult::Ignore) {
    handle.setContext("lease_end_message_validated", true);
    return 0;
  }

  if (result == V6HookResult::ConfirmNotOnLink) {
    handle.setContext("confirm_not_on_link", true);
    return 0;
  }

  if (result == V6HookResult::Success && machine == nullptr) {
    result = V6HookResult::InvalidMachinePointer;
  }

  LOG_ERROR(logger,
            "LOG_CARBIDE_PKT6_RECEIVE: Error while executing DHCPv6 discovery: %1, machine_ptr=%2")
      .arg(carbide_v6_hook_result_as_str(result))
      .arg(machine);
  if (machine != nullptr) {
    machine_free(machine);
  }
  handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
  record_dropped_v6_request(carbide_v6_hook_result_as_str(result));
  return 1;
}

int lease6_select(CalloutHandle &handle) {
  Lease6Ptr lease6;
  handle.getArgument("lease6", lease6);

  bool fake_allocation = false;
  try {
    handle.getArgument("fake_allocation", fake_allocation);
  } catch (...) {
    // Some Kea versions may not always pass this, that's fine.
  }

  return override_lease6_address(
      handle, lease6, "lease6_select",
      fake_allocation ? " (SOLICIT, not persisted)" : " (REQUEST, will persist)");
}

int lease6_renew(CalloutHandle &handle) {
  Lease6Ptr lease6;
  handle.getArgument("lease6", lease6);

  return refuse_lease6_address_migration(handle, lease6, "lease6_renew");
}

int lease6_rebind(CalloutHandle &handle) {
  Lease6Ptr lease6;
  handle.getArgument("lease6", lease6);

  return refuse_lease6_address_migration(handle, lease6, "lease6_rebind");
}

// Final DHCPv6 send-path guard: drop unsafe responses, apply hook-managed
// status/options, and count replies that Kea is allowed to send.
int pkt6_send(CalloutHandle &handle) {
  // Recover Kea's request/response pair. query6 can be absent on some error
  // paths, but response6 is required because this hook mutates the outbound
  // packet.
  Pkt6Ptr query6_ptr, response6_ptr;
  try {
    handle.getArgument("query6", query6_ptr);
  } catch (...) {
    query6_ptr.reset();
  }
  handle.getArgument("response6", response6_ptr);

  if (!response6_ptr) {
    LOG_ERROR(logger, "LOG_CARBIDE_PKT6_SEND: missing response6 argument");
    handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
    return 1;
  }

  // CONFIRM cache validation can produce a status-only response without a
  // Machine context.
  bool confirm_not_on_link = false;
  try {
    handle.getContext("confirm_not_on_link", confirm_not_on_link);
  } catch (...) {
    confirm_not_on_link = false;
  }

  if (confirm_not_on_link) {
    add_status6(response6_ptr, STATUS_NotOnLink, "not on link");
    record_v6_reply_sent(handle, response6_ptr);
    return 0;
  }

  bool confirm_on_link = false;
  try {
    handle.getContext("confirm_on_link", confirm_on_link);
  } catch (...) {
    confirm_on_link = false;
  }

  // RELEASE/DECLINE already passed receive-side validation. Let Kea send its
  // reply unchanged and only record the send-side metric.
  bool lease_end_message_validated = false;
  try {
    handle.getContext("lease_end_message_validated",
                      lease_end_message_validated);
  } catch (...) {
    lease_end_message_validated = false;
  }
  if (lease_end_message_validated) {
    record_v6_reply_sent(handle, response6_ptr);
    return 0;
  }

  // All remaining response paths need the API-selected Machine record for
  // stale-address checks and DHCPv6 service option rendering.
  boost::shared_ptr<Machine> machine;
  try {
    handle.getContext("machine", machine);
  } catch (...) {
    machine.reset();
  }
  if (!machine) {
    LOG_ERROR(logger, "LOG_CARBIDE_PKT6_SEND: Missing machine object from handle context");
    handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
    return 1;
  }

  // If Kea reuses a lease address after we refused renewal migration, fail
  // closed instead of sending an address the API no longer owns.
  isc::asiolink::IOAddress carbide_addr("::");
  if (machine_ipv6_address(machine.get(), carbide_addr)) {
    std::string stale_address_detail;
    if (response6_has_stale_ia_na_address(response6_ptr, carbide_addr,
                                          stale_address_detail)) {
      LOG_ERROR(logger, "LOG_CARBIDE_PKT6_SEND: %1")
          .arg("dropping DHCPv6 response with non-authoritative IA_NA: " +
               stale_address_detail);
      handle.setStatus(CalloutHandle::NEXT_STEP_DROP);
      return 1;
    }
  }

  // Kea does not synthesize this for the hook-managed CONFIRM cache path.
  if (confirm_on_link) {
    add_status6(response6_ptr, STATUS_Success, "success");
  }

  set_options_v6(handle, query6_ptr, response6_ptr, machine.get());

  // Record the final outbound packet after hook-managed options/status have
  // been applied.
  LOG_INFO(logger, "LOG_CARBIDE_PKT6_SEND: %1")
      .arg(response6_ptr->toText());

  record_v6_reply_sent(handle, response6_ptr);
  return 0;
}

int lease6_expire(CalloutHandle &handle) {
  return handle_carbide_lease6_end(handle, "lease6_expire");
}
}
