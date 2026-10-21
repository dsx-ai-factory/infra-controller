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

//! Host naming strategies.
//!
//! A machine interface's hostname is the DNS record name it resolves under (the
//! DNS views join `machine_interfaces.hostname` to its addresses to produce A /
//! AAAA records). How that hostname is derived is a deployment-wide policy,
//! selected once via [`crate::host_naming::HostNamingStrategyKind`] from
//! `CarbideConfig.host_naming_strategy` and applied wherever an interface is
//! [re]named.
//!
//! The policy is consulted at every naming-relevant event (a new interface, an
//! address gained or lost, an admin reconcile). Each strategy decides whether to
//! [`Naming::Assign`] a new hostname or [`Naming::Keep`] the stored one, so the
//! call sites stay uniform and strategy-agnostic:
//!
//! - [`IpHostNamingStrategy`] mirrors the address: it always reassigns, so the
//!   hostname tracks the current IP (today's behavior).
//! - [`FunHostNamingStrategy`] creates a stable adjective-noun handle once
//!   (e.g. `wholesale-walrus`) and keeps it across IP changes -- the DNS view
//!   repoints the record to the new address under the same name automatically.
//! - [`SerialNumberHostNamingStrategy`] names a host after its hardware serial.
//!   The serial isn't available on the DHCP/network plane, so the primary
//!   interface gets a temporary IP-based name and switches to the serial name
//!   once the machine is discovered and its [`NamingContext::machine_id`] is set.
//! - [`MacAddressHostNamingStrategy`] names every interface after its own MAC
//!   (`0a-1b-2c-3d-4e-5f`). The MAC is known from the first DHCP packet, so
//!   there is no temporary-name phase at all.
//!
//! Only `fun` leaves existing hostnames alone: it assigns a name only when the
//! stored one is a placeholder (a brand-new row or a `noip-...` dormant marker)
//! and otherwise keeps it, so a fleet adopts it organically as hosts cycle,
//! existing names -- and DNS -- untouched. The other styles re-derive (the name
//! tracks the current IP, the machine's serial, or the interface's MAC), so
//! switching to one of them progressively renames existing interfaces as the
//! address-sync and reconcile paths run.
//!
//! The identity-derived styles promise that the name *is* the identifier --
//! that's the whole point of choosing them. So when the fleet's data breaks
//! that promise (two machines reporting the same serial), `serial_number`
//! fails with a clear error instead of quietly assigning a substitute nobody
//! would think to look up in DNS. `mac_address` needs no such preflight: the
//! MAC name is injective (only rows for the very same MAC can share it), so
//! true duplicate MACs surface through the `(segment_id, mac_address)`
//! constraint and DHCP itself, and same-domain name conflicts are caught by
//! `fqdn_must_be_unique`.

use std::net::IpAddr;
use std::str::FromStr;
use std::sync::OnceLock;

use carbide_uuid::domain::DomainId;
use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use mac_address::MacAddress;
use model::machine::MachineInterfaceSnapshot;
use model::machine_interface::InterfaceType;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;

use crate::{DatabaseError, DatabaseResult};

/// The `machine_interfaces.hostname` column is `VARCHAR(63)` (the DNS label
/// limit), so every generated hostname must fit within this.
const MAX_HOSTNAME_LEN: usize = 63;

/// How many new adjective-noun candidates to try before falling back to a
/// numeric suffix when resolving a hostname collision.
const MAX_PLAIN_TRIES: usize = 16;

/// Upper bound on the numeric-suffix probe used as a collision backstop. Far
/// beyond any realistic contention; exceeding it is treated as an error rather
/// than looping forever.
const MAX_SUFFIX_PROBE: u32 = 10_000;

/// Inputs available wherever an interface's hostname is [re]computed.
///
/// This holds a *targeted* old/new view rather than a full before/after
/// snapshot: [`current_hostname`](Self::current_hostname) is the **old** stored
/// name (what powers a stable strategy's keep decision) and
/// [`addresses`](Self::addresses) is the **new**, post-event address set (what
/// the IP strategy derives from). No strategy needs an address *diff*, so none
/// is provided.
pub struct NamingContext<'a> {
    /// The interface's MAC, used for the dormant (`noip-...`) placeholder and
    /// for MAC-derived names.
    pub mac_address: MacAddress,
    /// Addresses the interface holds after the triggering event. Empty means the
    /// interface just went dormant (addressless).
    pub addresses: &'a [IpAddr],
    /// The hostname stored right now, if any. `None` only at first creation.
    pub current_hostname: Option<&'a str>,
    /// The machine this interface is bound to, when known. Serial-based naming
    /// resolves the serial from this; it is `None` until the machine is
    /// discovered and associated (the network plane has no machine identity).
    pub machine_id: Option<MachineId>,
    /// Whether this is the machine's primary admin interface -- the DNS-visible
    /// hostname. Under serial naming the primary gets the bare serial; secondary
    /// interfaces share that serial and so get `serial-<mac>` to stay unique.
    pub is_primary: bool,
    /// What kind of interface this is. A BMC interface belongs to the machine's
    /// management controller, not the machine itself, so serial naming leaves it
    /// IP-named rather than letting it contend for the machine's serial.
    pub interface_type: InterfaceType,
    /// The interface's own row id, when it already exists (`None` only at first
    /// creation). The collision checks use this to recognize the row itself as
    /// the holder of a name -- `current_hostname` alone can't, since it may be
    /// stale after an earlier rename in the same transaction.
    pub interface_id: Option<MachineInterfaceId>,
    /// The domain the interface is currently in. Name uniqueness is per-domain
    /// (`fqdn_must_be_unique`), so fun's duplicate self-heal scopes to it --
    /// the same hostname in two different domains is legal, settled data.
    pub domain_id: Option<DomainId>,
}

impl<'a> NamingContext<'a> {
    /// The context for an existing interface as read back from the database --
    /// the common case for the post-create paths (address sync, allocation).
    pub fn from_snapshot(snapshot: &'a MachineInterfaceSnapshot) -> Self {
        Self {
            mac_address: snapshot.mac_address,
            addresses: &snapshot.addresses,
            current_hostname: Some(&snapshot.hostname),
            machine_id: snapshot.machine_id,
            is_primary: snapshot.primary_interface,
            interface_type: snapshot.interface_type,
            interface_id: Some(snapshot.id),
            domain_id: snapshot.domain_id,
        }
    }
}

/// What the caller should do with a strategy's result.
#[derive(Debug, PartialEq, Eq)]
pub enum Naming {
    /// Write this hostname.
    Assign(String),
    /// Leave the stored hostname untouched -- what a stable style returns once an
    /// interface already has a real name.
    Keep,
}

/// Resolve the hostname for `ctx` under the configured strategy: the newly
/// assigned name, or the current one when the strategy keeps it. This is the
/// one entry point the [re]naming call sites go through.
pub async fn hostname_for(
    txn: &mut PgConnection,
    ctx: &NamingContext<'_>,
) -> DatabaseResult<String> {
    match configured().strategy().name(txn, ctx).await? {
        Naming::Assign(hostname) => Ok(hostname),
        Naming::Keep => ctx.current_hostname.map(str::to_string).ok_or_else(|| {
            DatabaseError::internal(
                "host naming strategy returned Keep with no stored hostname".to_string(),
            )
        }),
    }
}

/// A pluggable hostname naming strategy. Selected by
/// [`HostNamingStrategyKind`] and consulted at each naming-relevant event.
#[async_trait::async_trait]
pub trait HostNamingStrategy: Send + Sync {
    /// Decide the hostname for the interface described by `ctx`. `txn` is
    /// provided for strategies that need to check candidate uniqueness against
    /// the database (the others ignore it).
    async fn name(&self, txn: &mut PgConnection, ctx: &NamingContext<'_>)
    -> DatabaseResult<Naming>;
}

/// Config-selectable naming strategy. Deserializes from snake_case (e.g.
/// `"ip_address"`, `"fun"`) and defaults to [`IpAddress`](Self::IpAddress).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostNamingStrategyKind {
    /// Derive the hostname from the interface's IP address (`10.1.2.3` ->
    /// `10-1-2-3`). Recomputed on every address change.
    #[default]
    IpAddress,
    /// Name the host after its hardware serial number (lower-cased).
    SerialNumber,
    /// Name each interface after its own MAC address (`0a-1b-2c-3d-4e-5f`).
    MacAddress,
    /// Create a stable, human-friendly adjective-noun handle (e.g.
    /// `wholesale-walrus`) and keep it across IP changes.
    Fun,
}

impl HostNamingStrategyKind {
    /// The strategy implementation for this variant.
    pub fn strategy(self) -> &'static dyn HostNamingStrategy {
        match self {
            Self::IpAddress => &IpHostNamingStrategy,
            Self::SerialNumber => &SerialNumberHostNamingStrategy,
            Self::MacAddress => &MacAddressHostNamingStrategy,
            Self::Fun => &FunHostNamingStrategy,
        }
    }
}

// Process-wide policy, set once at startup (mirrors `crate::init_tools`).

static POLICY: OnceLock<HostNamingStrategyKind> = OnceLock::new();

/// Set the deployment-wide host naming policy from parsed config. Called once
/// during server startup; later calls are ignored (the first value wins), with
/// a warning when the ignored value differs from the active one.
pub fn configure(kind: HostNamingStrategyKind) {
    if POLICY.set(kind).is_err() && configured() != kind {
        tracing::warn!(
            requested = ?kind,
            active = ?configured(),
            "host naming policy is already set; keeping the active one"
        );
    }
}

/// The configured naming policy, or the [`IpAddress`](HostNamingStrategyKind::IpAddress)
/// default when unset (e.g. unit tests that never call [`configure`]).
pub fn configured() -> HostNamingStrategyKind {
    POLICY.get().copied().unwrap_or_default()
}

// Strategies

/// Names each interface after its address -- the historical behavior. Always
/// reassigns, so the hostname mirrors the current IP and follows it on change.
pub struct IpHostNamingStrategy;

#[async_trait::async_trait]
impl HostNamingStrategy for IpHostNamingStrategy {
    async fn name(
        &self,
        _txn: &mut PgConnection,
        ctx: &NamingContext<'_>,
    ) -> DatabaseResult<Naming> {
        Ok(Naming::Assign(ip_or_dormant(ctx)?))
    }
}

/// Restores the "fun" naming scheme: a stable adjective-noun handle assigned
/// once and preserved across IP changes.
pub struct FunHostNamingStrategy;

#[async_trait::async_trait]
impl HostNamingStrategy for FunHostNamingStrategy {
    async fn name(
        &self,
        txn: &mut PgConnection,
        ctx: &NamingContext<'_>,
    ) -> DatabaseResult<Naming> {
        match classify_stable(ctx) {
            StableDecision::Keep => {
                // Self-heal a name duplicated within this domain. Two interfaces
                // can end up sharing one (a creation race slipping past the
                // preflight); keeping it would wedge every later write on the
                // `fqdn_must_be_unique` constraint with no path to a new name.
                // The same name in two *different* domains is legal, settled
                // data and is left alone.
                let current = ctx.current_hostname.unwrap_or_default();
                if hostname_duplicated(txn, current, ctx.domain_id).await? {
                    tracing::warn!(
                        hostname = %current,
                        "hostname is shared by multiple interfaces in this domain; generating a replacement"
                    );
                    return Ok(Naming::Assign(generate_unique_hostname(txn).await?));
                }
                Ok(Naming::Keep)
            }
            StableDecision::Dormant => Ok(Naming::Assign(dormant_admin_hostname(ctx.mac_address))),
            StableDecision::Generate => Ok(Naming::Assign(generate_unique_hostname(txn).await?)),
        }
    }
}

/// Names a host after its hardware serial.
///
/// The serial is discovered out-of-band (BMC/Redfish), so it isn't known on the
/// DHCP/network plane -- until the machine is discovered and bound, this falls
/// back to the IP-derived name. Once the machine (and a real serial) is known,
/// the primary interface gets the bare serial (its DNS-visible name) and
/// secondary interfaces get `serial-<mac>`. The serial is resolved via
/// [`NamingContext::machine_id`].
pub struct SerialNumberHostNamingStrategy;

#[async_trait::async_trait]
impl HostNamingStrategy for SerialNumberHostNamingStrategy {
    async fn name(
        &self,
        txn: &mut PgConnection,
        ctx: &NamingContext<'_>,
    ) -> DatabaseResult<Naming> {
        match usable_serial(txn, ctx).await? {
            // The primary, DNS-visible interface gets the bare serial.
            Some(serial) if ctx.is_primary => Ok(Naming::Assign(
                claim_bare_serial_hostname(txn, &serial, ctx).await?,
            )),
            // Secondary interfaces take `serial-<mac>` -- the MAC keeps them
            // unique, since a machine's interfaces share the one serial.
            Some(serial) => {
                let hostname = serial_with_mac_hostname(&serial, ctx.mac_address)?;
                ensure_hostname_available(txn, &hostname, ctx, &serial).await?;
                Ok(Naming::Assign(hostname))
            }
            // No usable serial (yet): the temporary IP-based name.
            None => Ok(Naming::Assign(ip_or_dormant(ctx)?)),
        }
    }
}

/// The serial this interface should be named after, if there is one: the
/// machine must be known (discovery has bound it), the recorded serial must be
/// real (not a junk vendor placeholder like "To Be Filled By O.E.M."), and the
/// interface must belong to the machine itself -- its BMC is a different
/// device that happens to report the same serial, so it stays IP-named.
async fn usable_serial(
    txn: &mut PgConnection,
    ctx: &NamingContext<'_>,
) -> DatabaseResult<Option<String>> {
    if ctx.interface_type == InterfaceType::Bmc {
        return Ok(None);
    }
    let Some(machine_id) = ctx.machine_id else {
        return Ok(None);
    };
    Ok(
        crate::machine_topology::serial_for_machine(&mut *txn, &machine_id)
            .await?
            .filter(|serial| !is_placeholder_serial(serial)),
    )
}

/// Names each interface after its own MAC address (`0a-1b-2c-3d-4e-5f`).
///
/// The MAC is the one identifier already present in the very first DHCP packet,
/// so unlike serial naming there is no temporary-name phase and no
/// primary/secondary split -- every interface, the BMC included, gets its own
/// MAC name from the moment it appears.
pub struct MacAddressHostNamingStrategy;

#[async_trait::async_trait]
impl HostNamingStrategy for MacAddressHostNamingStrategy {
    async fn name(
        &self,
        _txn: &mut PgConnection,
        ctx: &NamingContext<'_>,
    ) -> DatabaseResult<Naming> {
        // No uniqueness preflight: the MAC name is injective, so only rows for
        // this very MAC can share it (this row itself, or a lingering row from
        // a segment move -- the same physical NIC, which must not be locked out
        // of DHCP). True duplicate MACs surface through the
        // `(segment_id, mac_address)` constraint and DHCP itself, and
        // same-domain name conflicts through `fqdn_must_be_unique`.
        Ok(Naming::Assign(mac_hostname(ctx.mac_address)))
    }
}

// Shared helpers

/// The decision shared by stable (assign-once) strategies.
enum StableDecision {
    /// Already has a real (non-placeholder) name -> leave it alone.
    Keep,
    /// Addressless and unnamed -> use the dormant placeholder so the row makes no
    /// DNS record, just as the IP style does.
    Dormant,
    /// Brand-new or a placeholder, but has an address -> generate a new name.
    Generate,
}

fn classify_stable(ctx: &NamingContext<'_>) -> StableDecision {
    if ctx.current_hostname.is_some_and(|h| !is_placeholder(h)) {
        StableDecision::Keep
    } else if ctx.addresses.is_empty() {
        StableDecision::Dormant
    } else {
        StableDecision::Generate
    }
}

/// Prefer IPv4 (more human-readable, and the usual name for a dual-stack
/// interface), else the first address.
fn preferred_address(addresses: &[IpAddr]) -> Option<&IpAddr> {
    addresses.iter().find(|a| a.is_ipv4()).or(addresses.first())
}

/// The IP-derived name for the preferred address, or the dormant placeholder
/// when the interface holds no addresses.
fn ip_or_dormant(ctx: &NamingContext<'_>) -> DatabaseResult<String> {
    match preferred_address(ctx.addresses) {
        Some(addr) => address_to_hostname(addr),
        None => Ok(dormant_admin_hostname(ctx.mac_address)),
    }
}

/// `address_to_hostname` builds the IP-derived hostname: IPv4 dotted-quad with
/// dashes (`10.1.2.3` -> `10-1-2-3`); IPv6 fully-expanded hex segments with
/// dashes. It validates the result as a legal DNS name before returning it.
pub fn address_to_hostname(address: &IpAddr) -> DatabaseResult<String> {
    let hostname = match address {
        IpAddr::V4(_) => address.to_string().replace('.', "-"),
        IpAddr::V6(v6) => v6
            .segments()
            .iter()
            .map(|s| format!("{s:04x}"))
            .collect::<Vec<_>>()
            .join("-"),
    };
    validate_dns_name(hostname)
}

/// Builds the deterministic placeholder for an addressless (dormant) interface.
fn dormant_admin_hostname(mac_address: MacAddress) -> String {
    format!("noip-{}", mac_address.to_string().replace(':', "-"))
}

/// True if a hostname is a non-identifying placeholder (empty, or the `noip-...`
/// dormant marker) and therefore eligible to be [re]named by a stable strategy.
///
/// Anything else -- an IP-derived name, a serial, a MAC -- is a real name,
/// and the fun strategy keeps real names. So switching a site to `fun`
/// renames nothing: fun names show up only on brand-new interfaces, or on
/// ones that lose all their addresses and pass through the `noip-`
/// placeholder first.
fn is_placeholder(hostname: &str) -> bool {
    hostname.is_empty() || hostname.starts_with("noip-")
}

/// Recognizes vendor placeholder serials -- the junk DMI/SMBIOS defaults that
/// aren't real identifiers -- so serial naming can fall back to the IP name
/// instead of turning a non-unique placeholder into a hostname.
fn is_placeholder_serial(serial: &str) -> bool {
    let s = serial.trim();
    if s.is_empty() {
        return true;
    }
    // Common board/BIOS defaults seen in the field.
    const PLACEHOLDERS: &[&str] = &[
        "to be filled by o.e.m.",
        "to be filled by oem",
        "default string",
        "system serial number",
        "not specified",
        "not applicable",
        "n/a",
        "none",
        "unknown",
        "oem",
    ];
    PLACEHOLDERS.iter().any(|p| s.eq_ignore_ascii_case(p))
        // all-zero, or nothing alphanumeric at all (would sanitize to nothing)
        || s.bytes().all(|b| b == b'0')
        || !s.chars().any(|c| c.is_ascii_alphanumeric())
}

/// Lower-cases a serial and reduces it to valid DNS-label characters
/// (alphanumerics, anything else collapsed to dashes), trimming dashes.
fn sanitize_serial(serial: &str) -> String {
    let sanitized: String = serial
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    sanitized.trim_matches('-').to_string()
}

/// Sanitizes a serial and truncates it to `max_len`, re-trimming any dash the
/// cut exposes. The byte slice is char-safe because [`sanitize_serial`] yields
/// pure ASCII. Errors when nothing usable remains.
fn truncated_serial_label(serial: &str, max_len: usize) -> DatabaseResult<String> {
    let s = sanitize_serial(serial);
    let label = s[..s.len().min(max_len)].trim_end_matches('-');
    if label.is_empty() {
        return Err(DatabaseError::internal(format!(
            "serial yields an empty hostname: {serial:?}"
        )));
    }
    Ok(label.to_string())
}

/// The bare serial as a single valid DNS label -- used for the primary,
/// DNS-visible interface.
fn serial_to_hostname(serial: &str) -> DatabaseResult<String> {
    validate_dns_name(truncated_serial_label(serial, MAX_HOSTNAME_LEN)?)
}

/// `<serial>-<mac>` for a secondary interface: serial-recognizable but unique,
/// since a machine's interfaces all share the one serial. The serial part is
/// truncated so the whole label still fits the 63-char DNS limit alongside the
/// 12-character MAC.
fn serial_with_mac_hostname(serial: &str, mac: MacAddress) -> DatabaseResult<String> {
    let mac_label = compact_mac_label(mac);
    let serial_part =
        truncated_serial_label(serial, MAX_HOSTNAME_LEN.saturating_sub(mac_label.len() + 1))?;
    validate_dns_name(format!("{serial_part}-{mac_label}"))
}

/// The MAC as a compact lower-case hex label (`aabbccddeeff`) -- the suffix
/// form used by `serial-<mac>` names.
fn compact_mac_label(mac: MacAddress) -> String {
    mac.to_string().to_ascii_lowercase().replace(':', "")
}

/// The MAC-derived hostname (`0a-1b-2c-3d-4e-5f`), always a valid DNS label.
fn mac_hostname(mac: MacAddress) -> String {
    mac.to_string().to_ascii_lowercase().replace(':', "-")
}

fn validate_dns_name(hostname: String) -> DatabaseResult<String> {
    if domain::base::Name::<octseq::array::Array<255>>::from_str(&hostname).is_ok() {
        Ok(hostname)
    } else {
        Err(DatabaseError::internal(format!(
            "invalid hostname: {hostname}"
        )))
    }
}

/// Generates an adjective-noun hostname not already present in
/// `machine_interfaces`, falling back to a numeric suffix under heavy contention.
///
/// All candidate names are built before any database `await`, since the name
/// generator can't be held across one.
async fn generate_unique_hostname(txn: &mut PgConnection) -> DatabaseResult<String> {
    let candidates: Vec<String> = {
        let mut generator = names::Generator::default();
        (0..MAX_PLAIN_TRIES)
            .filter_map(|_| generator.next())
            .filter(|name| name.len() <= MAX_HOSTNAME_LEN)
            .collect()
    };
    for candidate in candidates {
        if !hostname_taken(txn, &candidate).await? {
            return Ok(candidate);
        }
    }

    // We had a collision, so make a new one, and if that doesn't work, then
    // just start appending a number to the end (e.g. -1).
    let base = names::Generator::default()
        .next()
        .ok_or_else(|| DatabaseError::internal("names generator produced no name".to_string()))?;
    if base.len() <= MAX_HOSTNAME_LEN && !hostname_taken(txn, &base).await? {
        return Ok(base);
    }
    for suffix in 1..=MAX_SUFFIX_PROBE {
        let candidate = format!("{base}-{suffix}");
        if candidate.len() <= MAX_HOSTNAME_LEN && !hostname_taken(txn, &candidate).await? {
            return Ok(candidate);
        }
    }
    Err(DatabaseError::internal(
        "could not allocate a unique hostname".to_string(),
    ))
}

/// True if any machine interface already holds this hostname. Uniqueness is
/// enforced in-app (not via a DB constraint) so existing duplicate-friendly
/// rows -- e.g. the same IP-derived name across distinct domains -- are untouched.
async fn hostname_taken(txn: &mut PgConnection, hostname: &str) -> DatabaseResult<bool> {
    let query = "SELECT EXISTS(SELECT 1 FROM machine_interfaces WHERE hostname = $1)";
    sqlx::query_scalar(query)
        .bind(hostname)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// True if more than one interface row in this domain holds this hostname.
/// Scoped to the domain because that is the grain of `fqdn_must_be_unique` --
/// the same hostname in two different domains is legal, settled data.
async fn hostname_duplicated(
    txn: &mut PgConnection,
    hostname: &str,
    domain_id: Option<DomainId>,
) -> DatabaseResult<bool> {
    let query = "SELECT COUNT(*) > 1 FROM machine_interfaces \
                 WHERE hostname = $1 AND domain_id IS NOT DISTINCT FROM $2";
    sqlx::query_scalar(query)
        .bind(hostname)
        .bind(domain_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// True if an interface OTHER than `interface_id` holds this hostname. The id
/// exclusion matters: `current_hostname` can be stale after an earlier rename
/// in the same transaction, so name comparison alone can mistake the row
/// itself for a rival holder.
async fn hostname_taken_by_other(
    txn: &mut PgConnection,
    hostname: &str,
    interface_id: Option<MachineInterfaceId>,
) -> DatabaseResult<bool> {
    let query = "SELECT EXISTS(SELECT 1 FROM machine_interfaces \
                 WHERE hostname = $1 AND id IS DISTINCT FROM $2)";
    sqlx::query_scalar(query)
        .bind(hostname)
        .bind(interface_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Errors unless `hostname` is free or held only by this interface itself.
/// Used by serial naming: a taken name there means a duplicated hardware
/// identifier, which deserves a loud failure -- a quiet substitute name would
/// break the "reach it by its identifier" promise.
///
/// The preflight is best-effort: a concurrent claim can still slip through and
/// is caught by the `fqdn_must_be_unique` constraint at write time.
async fn ensure_hostname_available(
    txn: &mut PgConnection,
    hostname: &str,
    ctx: &NamingContext<'_>,
    identifier: &str,
) -> DatabaseResult<()> {
    if ctx.current_hostname == Some(hostname)
        || !hostname_taken_by_other(txn, hostname, ctx.interface_id).await?
    {
        return Ok(());
    }
    Err(DatabaseError::FailedPrecondition(format!(
        "hostname {hostname:?} derived from {identifier:?} is already held by another \
         interface; hardware identifiers must be unique under this naming strategy"
    )))
}

/// An interface row currently holding a contested hostname.
#[derive(sqlx::FromRow)]
struct HostnameHolder {
    id: MachineInterfaceId,
    machine_id: Option<MachineId>,
    primary_interface: bool,
    mac_address: MacAddress,
}

/// The bare serial name for a primary interface, enforcing the "the name is the
/// serial" promise. The name's current holders decide the outcome:
///
/// - nobody, or this interface itself: the name is taken as-is.
/// - non-primary interfaces of the same machine (an ex-primary demoted during
///   e.g. DPU attach, still wearing the bare serial): each is renamed to its
///   `serial-<mac>` form first, then the primary takes the bare name.
/// - anyone else: another machine claims the same serial -- duplicate or
///   unrecognized-placeholder data -- and naming fails loudly rather than
///   creating a substitute name.
async fn claim_bare_serial_hostname(
    txn: &mut PgConnection,
    serial: &str,
    ctx: &NamingContext<'_>,
) -> DatabaseResult<String> {
    let bare = serial_to_hostname(serial)?;
    if ctx.current_hostname == Some(bare.as_str()) {
        return Ok(bare);
    }
    // This row itself can show up as a holder when an earlier write in this
    // transaction already gave it the bare name; that is ours, not a rival.
    let holders: Vec<HostnameHolder> = hostname_holders(txn, &bare)
        .await?
        .into_iter()
        .filter(|holder| Some(holder.id) != ctx.interface_id)
        .collect();

    // Validate every holder before renaming any of them.
    if holders
        .iter()
        .any(|holder| holder.machine_id != ctx.machine_id || holder.primary_interface)
    {
        return Err(DatabaseError::FailedPrecondition(format!(
            "hostname {bare:?} derived from serial {serial:?} is already held by another \
             machine's interface; serials must be unique under serial-number naming"
        )));
    }
    for sibling in holders {
        let renamed = serial_with_mac_hostname(serial, sibling.mac_address)?;
        if hostname_taken_by_other(txn, &renamed, Some(sibling.id)).await? {
            return Err(DatabaseError::FailedPrecondition(format!(
                "cannot move the demoted holder of {bare:?} aside: its serial-<mac> name \
                 {renamed:?} is already held by another interface"
            )));
        }
        tracing::info!(
            machine_interface_id = %sibling.id,
            hostname = %bare,
            renamed = %renamed,
            "reclaiming the bare serial name from a demoted interface of the same machine"
        );
        update_interface_hostname(txn, sibling.id, &renamed).await?;
    }
    Ok(bare)
}

/// Every interface currently holding `hostname`.
async fn hostname_holders(
    txn: &mut PgConnection,
    hostname: &str,
) -> DatabaseResult<Vec<HostnameHolder>> {
    let query = "SELECT id, machine_id, primary_interface, mac_address \
                 FROM machine_interfaces WHERE hostname = $1";
    sqlx::query_as(query)
        .bind(hostname)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Rewrites just the hostname of one interface row (the domain is untouched);
/// used for the bare-serial reclaim handoff.
async fn update_interface_hostname(
    txn: &mut PgConnection,
    interface_id: MachineInterfaceId,
    hostname: &str,
) -> DatabaseResult<()> {
    let query = "UPDATE machine_interfaces SET hostname = $1 WHERE id = $2";
    sqlx::query(query)
        .bind(hostname)
        .bind(interface_id)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    crate::dns::domain::bump_serial_for_interface(txn, interface_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac() -> MacAddress {
        "aa:bb:cc:dd:ee:ff".parse().unwrap()
    }

    fn ctx<'a>(
        mac: MacAddress,
        addresses: &'a [IpAddr],
        current_hostname: Option<&'a str>,
    ) -> NamingContext<'a> {
        NamingContext {
            mac_address: mac,
            addresses,
            current_hostname,
            machine_id: None,
            is_primary: false,
            interface_type: InterfaceType::Data,
            interface_id: None,
            domain_id: None,
        }
    }

    #[test]
    fn address_to_hostname_v4_uses_dashes() {
        let address: IpAddr = "192.168.1.0".parse().unwrap();
        assert_eq!("192-168-1-0", address_to_hostname(&address).unwrap());
    }

    #[test]
    fn address_to_hostname_v6_is_fully_expanded() {
        let address: IpAddr = "2001:db8:abcd::2".parse().unwrap();
        assert_eq!(
            "2001-0db8-abcd-0000-0000-0000-0000-0002",
            address_to_hostname(&address).unwrap()
        );
    }

    #[test]
    fn address_to_hostname_v4_mapped_ipv6_is_stable() {
        // An IPv4-mapped IPv6 address renders as a fully-expanded v6 label, like any
        // other v6 -- locking it guards the persisted-label format against drift.
        let address: IpAddr = "::ffff:192.168.1.1".parse().unwrap();
        assert_eq!(
            "0000-0000-0000-0000-0000-ffff-c0a8-0101",
            address_to_hostname(&address).unwrap()
        );
    }

    #[test]
    fn address_to_hostname_v6_loopback() {
        let address: IpAddr = "::1".parse().unwrap();
        assert_eq!(
            "0000-0000-0000-0000-0000-0000-0000-0001",
            address_to_hostname(&address).unwrap()
        );
    }

    #[test]
    fn dormant_hostname_is_mac_derived() {
        // `mac_address::MacAddress` renders hex in upper-case; preserved verbatim.
        assert_eq!("noip-AA-BB-CC-DD-EE-FF", dormant_admin_hostname(mac()));
    }

    #[test]
    fn placeholder_detection() {
        assert!(is_placeholder(""));
        assert!(is_placeholder("noip-aa-bb-cc-dd-ee-ff"));
        assert!(!is_placeholder("10-1-2-3"));
        assert!(!is_placeholder("wholesale-walrus"));
    }

    #[test]
    fn ip_strategy_prefers_v4_in_dual_stack() {
        let mac = mac();
        let v4: IpAddr = "10.1.2.3".parse().unwrap();
        let v6: IpAddr = "2001:db8::2".parse().unwrap();
        // IPv6 listed first, but IPv4 wins the name.
        let dual = [v6, v4];
        assert_eq!("10-1-2-3", ip_or_dormant(&ctx(mac, &dual, None)).unwrap());
    }

    #[test]
    fn ip_strategy_v6_only_uses_v6_name() {
        let mac = mac();
        let v6: IpAddr = "2001:db8::2".parse().unwrap();
        let only = [v6];
        assert_eq!(
            "2001-0db8-0000-0000-0000-0000-0000-0002",
            ip_or_dormant(&ctx(mac, &only, None)).unwrap()
        );
    }

    #[test]
    fn ip_strategy_dormant_when_addressless() {
        let mac = mac();
        let none: [IpAddr; 0] = [];
        assert_eq!(
            "noip-AA-BB-CC-DD-EE-FF",
            ip_or_dormant(&ctx(mac, &none, None)).unwrap()
        );
    }

    #[test]
    fn stable_keeps_real_name_even_when_addressless() {
        let mac = mac();
        let none: [IpAddr; 0] = [];
        assert!(matches!(
            classify_stable(&ctx(mac, &none, Some("wholesale-walrus"))),
            StableDecision::Keep
        ));
    }

    #[test]
    fn stable_renames_placeholder_or_new_with_address() {
        let mac = mac();
        let v4: IpAddr = "10.1.2.3".parse().unwrap();
        let addrs = [v4];
        // Brand-new interface.
        assert!(matches!(
            classify_stable(&ctx(mac, &addrs, None)),
            StableDecision::Generate
        ));
        // Was a dormant placeholder, now has an address.
        assert!(matches!(
            classify_stable(&ctx(mac, &addrs, Some("noip-aa-bb-cc-dd-ee-ff"))),
            StableDecision::Generate
        ));
        // Was an IP-derived name (a real name) -- a strategy switch must not
        // retroactively rename it.
        assert!(matches!(
            classify_stable(&ctx(mac, &addrs, Some("10-1-2-3"))),
            StableDecision::Keep
        ));
    }

    #[test]
    fn stable_dormant_when_addressless_and_unnamed() {
        let mac = mac();
        let none: [IpAddr; 0] = [];
        assert!(matches!(
            classify_stable(&ctx(mac, &none, None)),
            StableDecision::Dormant
        ));
        assert!(matches!(
            classify_stable(&ctx(mac, &none, Some("noip-aa-bb-cc-dd-ee-ff"))),
            StableDecision::Dormant
        ));
    }

    #[test]
    fn serial_is_sanitized_to_a_dns_label() {
        assert_eq!("abc-123-xyz", serial_to_hostname("  ABC-123/XYZ ").unwrap());
        assert_eq!("sn0machine1", serial_to_hostname("SN0MACHINE1").unwrap());
        assert!(serial_to_hostname("///").is_err());
    }

    #[test]
    fn serial_with_mac_is_unique_and_fits() {
        assert_eq!(
            "sn-test-123-aabbccddeeff",
            serial_with_mac_hostname("SN-TEST-123", mac()).unwrap()
        );
        // A long serial is truncated so the whole label still fits 63 chars.
        let long = "x".repeat(80);
        let name = serial_with_mac_hostname(&long, mac()).unwrap();
        assert!(
            name.len() <= MAX_HOSTNAME_LEN,
            "too long ({}): {name}",
            name.len()
        );
        assert!(name.ends_with("-aabbccddeeff"));
    }

    #[test]
    fn mac_hostname_is_dashed_lowercase() {
        assert_eq!("aa-bb-cc-dd-ee-ff", mac_hostname(mac()));
        validate_dns_name(mac_hostname(mac())).unwrap();
    }

    #[test]
    fn placeholder_serials_fall_back() {
        for junk in [
            "",
            "  ",
            "To Be Filled By O.E.M.",
            "to be filled by oem",
            "Default string",
            "System Serial Number",
            "Not Specified",
            "N/A",
            "None",
            "00000000",
            "....",
        ] {
            assert!(
                is_placeholder_serial(junk),
                "expected placeholder: {junk:?}"
            );
        }
        for real in ["SN-TEST-123", "ABC123", "fm100h-xyz"] {
            assert!(
                !is_placeholder_serial(real),
                "expected real serial: {real:?}"
            );
        }
    }

    #[test]
    fn generated_names_are_valid_short_labels() {
        let mut generator = names::Generator::default();
        for _ in 0..32 {
            let name = generator.next().unwrap();
            assert!(name.len() <= MAX_HOSTNAME_LEN, "too long: {name}");
            validate_dns_name(name.clone()).unwrap_or_else(|_| panic!("invalid: {name}"));
        }
    }

    #[test]
    fn default_policy_is_ip_address() {
        assert_eq!(
            HostNamingStrategyKind::default(),
            HostNamingStrategyKind::IpAddress
        );
    }

    #[test]
    fn kind_deserializes_from_snake_case() {
        let parse = |s: &str| serde_json::from_str::<HostNamingStrategyKind>(s).unwrap();
        assert_eq!(parse("\"ip_address\""), HostNamingStrategyKind::IpAddress);
        assert_eq!(
            parse("\"serial_number\""),
            HostNamingStrategyKind::SerialNumber
        );
        assert_eq!(parse("\"mac_address\""), HostNamingStrategyKind::MacAddress);
        assert_eq!(parse("\"fun\""), HostNamingStrategyKind::Fun);
    }

    #[crate::sqlx_test]
    async fn fun_generates_a_unique_name_against_db(pool: sqlx::PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        let mac = mac();
        let addr: IpAddr = "10.1.2.3".parse().unwrap();
        let addresses = [addr];
        // A brand-new interface (no stored name) with an address gets a new
        // adjective-noun handle, created via the real DB uniqueness check.
        let ctx = ctx(mac, &addresses, None);
        match FunHostNamingStrategy.name(&mut conn, &ctx).await.unwrap() {
            Naming::Assign(name) => {
                assert!(
                    name.contains('-'),
                    "expected an adjective-noun name, got {name:?}"
                );
                assert!(name.len() <= MAX_HOSTNAME_LEN);
                validate_dns_name(name).unwrap();
            }
            Naming::Keep => panic!("a new interface must be assigned a name"),
        }
        // The uniqueness query itself runs and reports an absent name as free.
        assert!(
            !hostname_taken(&mut conn, "definitely-absent-hostname")
                .await
                .unwrap()
        );
    }

    fn test_machine_id() -> MachineId {
        MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30").unwrap()
    }

    #[crate::sqlx_test]
    async fn serial_uses_ip_when_machine_not_yet_known(pool: sqlx::PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        let mac = mac();
        let addr: IpAddr = "10.1.2.3".parse().unwrap();
        let addresses = [addr];
        // Primary interface, but the machine isn't associated yet -> temporary IP-based name.
        let ctx = NamingContext {
            mac_address: mac,
            addresses: &addresses,
            current_hostname: None,
            machine_id: None,
            is_primary: true,
            interface_type: InterfaceType::Data,
            interface_id: None,
            domain_id: None,
        };
        assert_eq!(
            SerialNumberHostNamingStrategy
                .name(&mut conn, &ctx)
                .await
                .unwrap(),
            Naming::Assign("10-1-2-3".to_string())
        );
    }

    #[crate::sqlx_test]
    async fn serial_uses_ip_when_no_serial_recorded(pool: sqlx::PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        let mac = mac();
        let addr: IpAddr = "10.1.2.3".parse().unwrap();
        let addresses = [addr];
        // Machine is bound but no topology/serial has been recorded yet -- both
        // primary and secondary interfaces fall back to the IP name until it is.
        let ctx = NamingContext {
            mac_address: mac,
            addresses: &addresses,
            current_hostname: None,
            machine_id: Some(test_machine_id()),
            is_primary: false,
            interface_type: InterfaceType::Data,
            interface_id: None,
            domain_id: None,
        };
        assert_eq!(
            SerialNumberHostNamingStrategy
                .name(&mut conn, &ctx)
                .await
                .unwrap(),
            Naming::Assign("10-1-2-3".to_string())
        );
    }

    #[crate::sqlx_test]
    async fn serial_for_machine_is_none_without_topology(pool: sqlx::PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        assert_eq!(
            crate::machine_topology::serial_for_machine(&mut *conn, &test_machine_id())
                .await
                .unwrap(),
            None
        );
    }

    #[crate::sqlx_test]
    async fn serial_strategy_switches_primary_to_the_serial_name(pool: sqlx::PgPool) {
        use model::hardware_info::{DmiData, HardwareInfo};

        let mut conn = pool.acquire().await.unwrap();
        let machine_id = test_machine_id();

        // A machine row (FK target) plus a discovered topology that includes a serial.
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(machine_id)
            .execute(&mut *conn)
            .await
            .unwrap();
        let hardware_info = HardwareInfo {
            dmi_data: Some(DmiData {
                product_serial: "SN-TEST-123".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        crate::machine_topology::create_or_update(&mut conn, &machine_id, &hardware_info)
            .await
            .unwrap();

        // The forward lookup reads the serial back.
        assert_eq!(
            crate::machine_topology::serial_for_machine(&mut *conn, &machine_id)
                .await
                .unwrap()
                .as_deref(),
            Some("SN-TEST-123")
        );

        // The primary interface's temporary IP-based name switches to the serial.
        let mac = mac();
        let addr: IpAddr = "10.1.2.3".parse().unwrap();
        let addresses = [addr];
        let ctx = NamingContext {
            mac_address: mac,
            addresses: &addresses,
            current_hostname: Some("10-1-2-3"),
            machine_id: Some(machine_id),
            is_primary: true,
            interface_type: InterfaceType::Data,
            interface_id: None,
            domain_id: None,
        };
        assert_eq!(
            SerialNumberHostNamingStrategy
                .name(&mut conn, &ctx)
                .await
                .unwrap(),
            Naming::Assign("sn-test-123".to_string())
        );

        // A secondary interface on the same machine gets `serial-<mac>` so it
        // stays unique while still showing the serial.
        let secondary = NamingContext {
            mac_address: mac,
            addresses: &addresses,
            current_hostname: None,
            machine_id: Some(machine_id),
            is_primary: false,
            interface_type: InterfaceType::Data,
            interface_id: None,
            domain_id: None,
        };
        assert_eq!(
            SerialNumberHostNamingStrategy
                .name(&mut conn, &secondary)
                .await
                .unwrap(),
            Naming::Assign("sn-test-123-aabbccddeeff".to_string())
        );

        // A BMC interface is the machine's management controller, not the
        // machine -- it stays IP-named even with the serial known.
        let bmc = NamingContext {
            mac_address: mac,
            addresses: &addresses,
            current_hostname: None,
            machine_id: Some(machine_id),
            is_primary: false,
            interface_type: InterfaceType::Bmc,
            interface_id: None,
            domain_id: None,
        };
        assert_eq!(
            SerialNumberHostNamingStrategy
                .name(&mut conn, &bmc)
                .await
                .unwrap(),
            Naming::Assign("10-1-2-3".to_string())
        );
    }

    /// Records a machine plus a topology carrying the test serial, and a spare
    /// network segment to attach fixture interfaces to.
    async fn serial_collision_fixture(conn: &mut PgConnection) -> MachineId {
        use model::hardware_info::{DmiData, HardwareInfo};

        let machine_id = test_machine_id();
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(machine_id)
            .execute(&mut *conn)
            .await
            .unwrap();
        let hardware_info = HardwareInfo {
            dmi_data: Some(DmiData {
                product_serial: "SN-TEST-123".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        crate::machine_topology::create_or_update(conn, &machine_id, &hardware_info)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO network_segments (id, name, version)
             VALUES ('00000000-0000-0000-0000-0000000000aa'::uuid, 'collision-seg', 'v1')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        machine_id
    }

    #[crate::sqlx_test]
    async fn serial_primary_errors_when_another_machine_holds_the_bare_serial(pool: sqlx::PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        let machine_id = serial_collision_fixture(&mut conn).await;

        // An interface with no machine binding -- think a second machine
        // reporting the same serial -- already holds the bare `sn-test-123`.
        sqlx::query(
            "INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname)
             VALUES ('00000000-0000-0000-0000-0000000000aa'::uuid, '02:00:00:00:00:99'::macaddr, false, 'sn-test-123')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();

        // The serial's name is its identity: a duplicate is a data problem to
        // surface, not something to paper over with a substitute name.
        let addr: IpAddr = "10.1.2.3".parse().unwrap();
        let addresses = [addr];
        let ctx = NamingContext {
            mac_address: mac(),
            addresses: &addresses,
            current_hostname: None,
            machine_id: Some(machine_id),
            is_primary: true,
            interface_type: InterfaceType::Data,
            interface_id: None,
            domain_id: None,
        };
        let err = SerialNumberHostNamingStrategy
            .name(&mut conn, &ctx)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DatabaseError::FailedPrecondition(_)),
            "expected FailedPrecondition, got {err:?}"
        );
    }

    #[crate::sqlx_test]
    async fn serial_primary_reclaims_the_bare_serial_from_a_demoted_sibling(pool: sqlx::PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        let machine_id = serial_collision_fixture(&mut conn).await;

        // A non-primary interface of the SAME machine -- an ex-primary demoted
        // during e.g. DPU attach -- still wears the bare serial name.
        sqlx::query(
            "INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname, machine_id, association_type)
             VALUES ('00000000-0000-0000-0000-0000000000aa'::uuid, '02:00:00:00:00:99'::macaddr, false, 'sn-test-123', $1, 'Machine')",
        )
        .bind(machine_id)
        .execute(&mut *conn)
        .await
        .unwrap();

        // The primary takes the bare name back; the demoted sibling is moved to
        // its `serial-<mac>` form in the same transaction.
        let addr: IpAddr = "10.1.2.3".parse().unwrap();
        let addresses = [addr];
        let ctx = NamingContext {
            mac_address: mac(),
            addresses: &addresses,
            current_hostname: Some("10-1-2-3"),
            machine_id: Some(machine_id),
            is_primary: true,
            interface_type: InterfaceType::Data,
            interface_id: None,
            domain_id: None,
        };
        assert_eq!(
            SerialNumberHostNamingStrategy
                .name(&mut conn, &ctx)
                .await
                .unwrap(),
            Naming::Assign("sn-test-123".to_string())
        );
        let sibling: String = sqlx::query_scalar(
            "SELECT hostname FROM machine_interfaces WHERE mac_address = '02:00:00:00:00:99'::macaddr",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(sibling, "sn-test-123-020000000099");
    }

    #[crate::sqlx_test]
    async fn mac_strategy_names_every_interface_by_its_mac(pool: sqlx::PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        let addr: IpAddr = "10.1.2.3".parse().unwrap();
        let addresses = [addr];

        // Every interface gets its own MAC name, machine binding or not.
        let fresh = ctx(mac(), &addresses, Some("10-1-2-3"));
        assert_eq!(
            MacAddressHostNamingStrategy
                .name(&mut conn, &fresh)
                .await
                .unwrap(),
            Naming::Assign("aa-bb-cc-dd-ee-ff".to_string())
        );

        // A lingering row for the same MAC (e.g. left on another segment by a
        // physical move) must not block the name -- it's the same NIC, and the
        // MAC name is injective, so only same-MAC rows can ever share it.
        sqlx::query(
            "INSERT INTO network_segments (id, name, version)
             VALUES ('00000000-0000-0000-0000-0000000000aa'::uuid, 'mac-seg', 'v1')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname)
             VALUES ('00000000-0000-0000-0000-0000000000aa'::uuid, 'aa:bb:cc:dd:ee:ff'::macaddr, false, 'aa-bb-cc-dd-ee-ff')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        let moved = ctx(mac(), &addresses, Some("10-1-2-3"));
        assert_eq!(
            MacAddressHostNamingStrategy
                .name(&mut conn, &moved)
                .await
                .unwrap(),
            Naming::Assign("aa-bb-cc-dd-ee-ff".to_string())
        );
    }

    #[crate::sqlx_test]
    async fn serial_primary_recognizes_itself_despite_a_stale_context_name(pool: sqlx::PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        let machine_id = serial_collision_fixture(&mut conn).await;

        // The primary row already wears the bare serial -- say an earlier write
        // in this very transaction renamed it -- but the caller's context still
        // has the older name.
        let interface_id: MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname, machine_id, association_type)
             VALUES ('00000000-0000-0000-0000-0000000000aa'::uuid, 'aa:bb:cc:dd:ee:ff'::macaddr, true, 'sn-test-123', $1, 'Machine')
             RETURNING id",
        )
        .bind(machine_id)
        .fetch_one(&mut *conn)
        .await
        .unwrap();

        // With its own id in the context, the row is recognized as the holder
        // -- no phantom "another machine has this serial" error.
        let addr: IpAddr = "10.1.2.3".parse().unwrap();
        let addresses = [addr];
        let ctx = NamingContext {
            mac_address: mac(),
            addresses: &addresses,
            current_hostname: Some("10-1-2-3"),
            machine_id: Some(machine_id),
            is_primary: true,
            interface_type: InterfaceType::Data,
            interface_id: Some(interface_id),
            domain_id: None,
        };
        assert_eq!(
            SerialNumberHostNamingStrategy
                .name(&mut conn, &ctx)
                .await
                .unwrap(),
            Naming::Assign("sn-test-123".to_string())
        );
    }

    #[crate::sqlx_test]
    async fn fun_replaces_a_name_shared_by_multiple_interfaces(pool: sqlx::PgPool) {
        let mut conn = pool.acquire().await.unwrap();

        // Two interfaces share one hostname (possible on NULL-domain rows, e.g.
        // duplicate IP-derived names inherited from a strategy switch).
        sqlx::query(
            "INSERT INTO network_segments (id, name, version)
             VALUES ('00000000-0000-0000-0000-0000000000aa'::uuid, 'dup-seg', 'v1')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        for mac in ["02:00:00:00:00:01", "02:00:00:00:00:02"] {
            sqlx::query(
                "INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname)
                 VALUES ('00000000-0000-0000-0000-0000000000aa'::uuid, $1::macaddr, false, '10-1-2-3')",
            )
            .bind(mac)
            .execute(&mut *conn)
            .await
            .unwrap();
        }

        // Instead of keeping the duplicated name (which would wedge any later
        // domain assignment on `fqdn_must_be_unique`), fun heals it.
        let addr: IpAddr = "10.1.2.3".parse().unwrap();
        let addresses = [addr];
        let shared = ctx(mac(), &addresses, Some("10-1-2-3"));
        match FunHostNamingStrategy
            .name(&mut conn, &shared)
            .await
            .unwrap()
        {
            Naming::Assign(name) => assert_ne!(name, "10-1-2-3"),
            Naming::Keep => panic!("a duplicated name must be replaced"),
        }

        // The same name in a *different* domain is legal, settled data --
        // uniqueness is per-domain -- so it must NOT trigger the heal.
        sqlx::query(
            "INSERT INTO domains (id, name)
             VALUES ('00000000-0000-0000-0000-0000000000dd'::uuid, 'other.example.com')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname, domain_id)
             VALUES ('00000000-0000-0000-0000-0000000000aa'::uuid, '02:00:00:00:00:03'::macaddr, false, '10-9-9-9', '00000000-0000-0000-0000-0000000000dd'::uuid)",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname)
             VALUES ('00000000-0000-0000-0000-0000000000aa'::uuid, '02:00:00:00:00:04'::macaddr, false, '10-9-9-9')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        let cross_domain = ctx(mac(), &addresses, Some("10-9-9-9"));
        assert_eq!(
            FunHostNamingStrategy
                .name(&mut conn, &cross_domain)
                .await
                .unwrap(),
            Naming::Keep
        );
    }
}
