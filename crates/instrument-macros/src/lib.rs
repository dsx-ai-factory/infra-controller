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

//! Derive macros for `carbide-instrument`: `#[derive(Event)]` and
//! `#[derive(LabelValue)]`. See the `carbide-instrument` crate documentation
//! for the model and usage; these macros are re-exported from there.

use carbide_observability_schema::{is_event_log_reserved_field, validate_event_name};
use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Data, DeriveInput, Field, Fields, Ident, LitStr, Meta, Token};

/// Metric-name unit suffixes a histogram may use, with the OpenTelemetry unit
/// string each one implies.
const UNIT_SUFFIXES: &[(&str, &str)] = &[
    ("_seconds", "s"),
    ("_milliseconds", "ms"),
    ("_microseconds", "us"),
    ("_bytes", "By"),
];

/// Derives `carbide_instrument::LabelValue` for a fieldless enum: each variant
/// renders as its snake_case name. Enums are the only derivable label type --
/// that is the cardinality guarantee. For a bounded-but-not-enum value,
/// implement `LabelValue` by hand on a newtype (the reviewed escape hatch).
#[proc_macro_derive(LabelValue)]
pub fn derive_label_value(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    match expand_label_value(input) {
        Ok(ts) => ts,
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_label_value(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "LabelValue can only be derived for enums: metric label values must come from a \
             closed set. For a bounded-but-not-enum value, implement LabelValue by hand on a \
             newtype.",
        ));
    };
    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "LabelValue needs at least one variant",
        ));
    }

    let mut arms = Vec::new();
    for variant in &data.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                variant,
                "LabelValue variants must be unit variants (no fields): the label value set \
                 must be closed",
            ));
        }
        let ident = &variant.ident;
        let value = snake_case(&ident.to_string());
        arms.push(quote! { Self::#ident => #value, });
    }

    Ok(quote! {
        impl ::carbide_instrument::LabelValue for #name {
            fn label_value(&self) -> ::carbide_instrument::__private::opentelemetry::StringValue {
                ::carbide_instrument::__private::opentelemetry::StringValue::from(match self {
                    #(#arms)*
                })
            }
        }
    }
    .into())
}

/// Derives `carbide_instrument::Event` for a struct declared with an
/// `#[event(...)]` attribute. Every field takes exactly one of `#[label]`
/// (`LabelValue`; supplies the metric label and, when logging, the matching log
/// field, with `name = "..."` available as a metric-only compatibility alias),
/// `#[context]` (any `Display`; log-only), `#[context(value)]` (`bool`, `i64`,
/// `f64`, or `String` retained as a native structured value; log-only), or
/// `#[observation]` (the histogram value). The metric name is validated at
/// compile time: `carbide_` prefix,
/// `_total` for counters (never a doubled `_total_total`), a unit suffix for
/// histograms. A counter's `describe` is checked too -- present and opening
/// with "Number of ..." -- with `describe_unchecked` as the escape hatch for
/// grandfathered text, mirroring `metric_name_unchecked` for names.
#[proc_macro_derive(Event, attributes(event, label, context, observation))]
pub fn derive_event(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    match expand_event(input) {
        Ok(ts) => ts,
        Err(e) => e.to_compile_error().into(),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum LogSpec {
    Off,
    Dynamic,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Clone, Copy, PartialEq)]
enum MetricSpec {
    Counter,
    Histogram,
    Gauge,
    None,
}

impl MetricSpec {
    /// Whether the kind takes its value from an `#[observation]` rather than
    /// counting the emit.
    fn records_observation(self) -> bool {
        matches!(self, MetricSpec::Histogram | MetricSpec::Gauge)
    }
}

/// The metric-side declaration shared by an inline `#[event(...)]` metric and a
/// `#[metric(...)]` family: one validator, so a family and an inline metric are
/// held to the same naming and documentation conventions.
struct MetricDeclaration<'a> {
    metric_name: &'a LitStr,
    kind: MetricSpec,
    unit: Option<&'a LitStr>,
    describe: Option<&'a LitStr>,
    metric_name_unchecked: bool,
    describe_unchecked: bool,
}

/// Validates a metric name, its `describe`, and its unit, and returns the
/// OpenTelemetry unit string a histogram records in (empty for a counter).
///
/// `item` spans the diagnostics belonging to the declaration as a whole rather
/// than to one attribute value.
fn validate_metric(
    decl: &MetricDeclaration<'_>,
    item: &dyn quote::ToTokens,
) -> syn::Result<String> {
    // The metric name in the attribute is the exposed name, verbatim, so a
    // dashboard greps straight back to this line. Validate the conventions
    // unless the site is migrating a grandfathered pre-standard name.
    let metric_name_value = decl.metric_name.value();
    let mut suffix_unit: Option<&'static str> = None;
    if decl.kind.records_observation() {
        suffix_unit = UNIT_SUFFIXES
            .iter()
            .find(|(suffix, _)| metric_name_value.ends_with(suffix))
            .map(|(_, unit)| *unit);
    }
    // A counter never takes a unit, whatever its name, so answer that first --
    // otherwise the histogram rule below reports a condition a counter cannot
    // satisfy and the diagnostic names the wrong mistake.
    if let Some(unit) = decl.unit
        && decl.kind == MetricSpec::Counter
    {
        return Err(syn::Error::new_spanned(
            unit,
            "`unit` is only valid for histogram and gauge metrics",
        ));
    }
    if !decl.metric_name_unchecked {
        if !metric_name_value.starts_with("carbide_") {
            return Err(syn::Error::new_spanned(
                decl.metric_name,
                "metric names use the `carbide_` prefix (use metric_name_unchecked only to \
                 keep a grandfathered pre-standard name)",
            ));
        }
        if decl.kind == MetricSpec::Counter {
            if !metric_name_value.ends_with("_total") {
                return Err(syn::Error::new_spanned(
                    decl.metric_name,
                    "counter names end in `_total` (Prometheus convention)",
                ));
            }
            // The OpenTelemetry instrument name must not carry `_total`
            // itself: the Prometheus exporter appends it, so a name that
            // still ends in `_total` after one is stripped ships a doubled
            // `_total_total` series (the #3431 footgun).
            if metric_name_value
                .strip_suffix("_total")
                .is_some_and(|base| base.ends_with("_total"))
            {
                return Err(syn::Error::new_spanned(
                    decl.metric_name,
                    "counter name ends in `_total_total`: the Prometheus exporter appends the \
                     `_total` suffix, so the instrument name must carry only one. Drop a \
                     `_total` (use metric_name_unchecked only to keep a grandfathered doubled name)",
                ));
            }
        } else if decl.kind == MetricSpec::Gauge {
            // A gauge rises and falls, so the counter's `_total` reads as the
            // wrong instrument to anyone querying it. Its unit suffix is
            // optional: plenty of gauges count things rather than measure one.
            if metric_name_value.ends_with("_total") {
                return Err(syn::Error::new_spanned(
                    decl.metric_name,
                    "`_total` is the counter suffix (Prometheus convention); a gauge names what \
                     it measures, optionally ending in its unit (use metric_name_unchecked only \
                     to keep a grandfathered pre-standard name)",
                ));
            }
        } else if suffix_unit.is_none() {
            return Err(syn::Error::new_spanned(
                decl.metric_name,
                "histogram names end in their unit: one of `_seconds`, `_milliseconds`, \
                 `_microseconds`, `_bytes`",
            ));
        }
        if let Some(unit) = decl.unit {
            return Err(syn::Error::new_spanned(
                unit,
                "`unit` is only for metric_name_unchecked histograms; a standard histogram \
                 name already declares its unit as the suffix",
            ));
        }
    }
    // A counter's `describe` is its Prometheus HELP text and the row the
    // `core_metrics.md` catalogue records, so a counter must document itself,
    // and the tech-writer house rule is that the text opens with "Number of ".
    // `describe_unchecked` is the escape hatch for a grandfathered describe --
    // legacy phrasings, or the "Total number of ..." on a metric_name_unchecked
    // counter -- mirroring `metric_name_unchecked` for names.
    if decl.kind == MetricSpec::Gauge
        && !decl.describe_unchecked
        && decl
            .describe
            .is_none_or(|describe| describe.value().is_empty())
    {
        return Err(syn::Error::new_spanned(
            item,
            "a gauge must document itself: add describe = \"...\" (its Prometheus HELP text, and \
             the core_metrics.md catalogue row)",
        ));
    }
    if decl.kind == MetricSpec::Counter && !decl.describe_unchecked {
        match decl.describe {
            None => {
                return Err(syn::Error::new_spanned(
                    item,
                    "a counter must document itself: add describe = \"Number of ...\" (its \
                     Prometheus HELP text, and the core_metrics.md catalogue row). Use \
                     describe_unchecked to keep a grandfathered counter's describe",
                ));
            }
            Some(describe) if !describe.value().starts_with("Number of ") => {
                return Err(syn::Error::new_spanned(
                    describe,
                    "a counter's describe opens with \"Number of ...\" (the tech-writer house \
                     rule). Use describe_unchecked to keep a grandfathered describe",
                ));
            }
            Some(_) => {}
        }
    }

    let unit_value: String = match (decl.unit, suffix_unit) {
        (Some(explicit), _) => explicit.value(),
        (None, Some(from_suffix)) => from_suffix.to_string(),
        (None, None) => String::new(),
    };
    if decl.kind == MetricSpec::Histogram && unit_value.is_empty() {
        return Err(syn::Error::new_spanned(
            decl.metric_name,
            "a metric_name_unchecked histogram without a recognized suffix needs an explicit \
             unit = \"...\"",
        ));
    }
    Ok(unit_value)
}

/// The `message` knob: absent, a static string, or `dynamic` -- the last routed
/// through the hand-implemented `DynamicMessage`.
enum MessageSpec {
    None,
    Static(LitStr),
    Dynamic,
}

struct EventArgs {
    event_name: Option<LitStr>,
    metric_name: Option<LitStr>,
    component: Option<LitStr>,
    message: MessageSpec,
    describe: Option<LitStr>,
    unit: Option<LitStr>,
    log: LogSpec,
    metric: MetricSpec,
    metric_family: Option<syn::Path>,
    metric_name_unchecked: bool,
    describe_unchecked: bool,
    /// The metric label schema, declared once on an enum Event. Empty for a
    /// struct Event, whose `#[label]` fields are the schema.
    labels: Vec<LabelSchemaEntry>,
}

fn parse_event_args(input: &DeriveInput) -> syn::Result<EventArgs> {
    let mut args = EventArgs {
        event_name: None,
        metric_name: None,
        component: None,
        message: MessageSpec::None,
        describe: None,
        unit: None,
        log: LogSpec::Info,
        metric: MetricSpec::None,
        metric_family: None,
        metric_name_unchecked: false,
        describe_unchecked: false,
        labels: Vec::new(),
    };
    let mut saw_attr = false;

    for attr in &input.attrs {
        if !attr.path().is_ident("event") {
            continue;
        }
        saw_attr = true;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("labels") {
                let content;
                syn::parenthesized!(content in meta.input);
                for entry in content.parse_terminated(LabelSchemaEntry::parse, Token![,])? {
                    if args.labels.iter().any(|held| held.name == entry.name) {
                        return Err(meta.error(format!("duplicate label `{}`", entry.name)));
                    }
                    args.labels.push(entry);
                }
            } else if meta.path.is_ident("event_name") {
                if args.event_name.is_some() {
                    return Err(meta.error("duplicate `event_name`"));
                }
                args.event_name = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("metric_name") {
                if args.metric_name.is_some() {
                    return Err(meta.error("duplicate `metric_name`"));
                }
                args.metric_name = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("name") {
                return Err(meta.error(
                    "`name` has been split into `event_name` and `metric_name`; every Event \
                     needs event_name, and metric-backed Events also need metric_name",
                ));
            } else if meta.path.is_ident("component") {
                args.component = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("message") {
                let value = meta.value()?;
                args.message = if value.peek(LitStr) {
                    MessageSpec::Static(value.parse()?)
                } else {
                    let ident: Ident = value.parse()?;
                    if ident == "dynamic" {
                        MessageSpec::Dynamic
                    } else {
                        return Err(meta.error("message must be a string literal or `dynamic`"));
                    }
                };
            } else if meta.path.is_ident("describe") {
                args.describe = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("unit") {
                args.unit = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("metric_name_unchecked") {
                args.metric_name_unchecked = true;
            } else if meta.path.is_ident("name_unchecked") {
                return Err(meta.error(
                    "`name_unchecked` was renamed to `metric_name_unchecked` because it only \
                     relaxes validation of a grandfathered metric name",
                ));
            } else if meta.path.is_ident("describe_unchecked") {
                args.describe_unchecked = true;
            } else if meta.path.is_ident("log") {
                let ident: Ident = meta.value()?.parse()?;
                args.log = match ident.to_string().as_str() {
                    "off" => LogSpec::Off,
                    "dynamic" => LogSpec::Dynamic,
                    "error" => LogSpec::Error,
                    "warn" => LogSpec::Warn,
                    "info" => LogSpec::Info,
                    "debug" => LogSpec::Debug,
                    "trace" => LogSpec::Trace,
                    other => {
                        return Err(meta.error(format!(
                            "unknown log level `{other}`; expected one of \
                             error | warn | info | debug | trace | off | dynamic"
                        )));
                    }
                };
            } else if meta.path.is_ident("metric") {
                let ident: Ident = meta.value()?.parse()?;
                args.metric = match ident.to_string().as_str() {
                    "counter" => MetricSpec::Counter,
                    "histogram" => MetricSpec::Histogram,
                    "gauge" => MetricSpec::Gauge,
                    "none" => MetricSpec::None,
                    other => {
                        return Err(meta.error(format!(
                            "unknown metric kind `{other}`; expected counter | histogram | none \
                             (a shared metric declares its kind on its MetricFamily, named here \
                             with metric_family = ...)"
                        )));
                    }
                };
            } else if meta.path.is_ident("metric_family") {
                if args.metric_family.is_some() {
                    return Err(meta.error("duplicate `metric_family`"));
                }
                args.metric_family = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error(
                    "unknown `event` key; expected event_name, metric_name, metric_family, \
                     component, message, describe, log, metric, unit, metric_name_unchecked, or \
                     describe_unchecked",
                ));
            }
            Ok(())
        })?;
    }

    if !saw_attr {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "deriving Event requires an #[event(event_name = ..., component = ..., ...)] \
             attribute",
        ));
    }
    Ok(args)
}

#[derive(Clone, Copy, PartialEq)]
enum ContextMode {
    Display,
    Value,
}

#[derive(Clone, Copy, PartialEq)]
enum FieldKind {
    Label,
    Context(ContextMode),
    Observation,
}

fn context_mode(attr: &syn::Attribute) -> syn::Result<ContextMode> {
    match &attr.meta {
        Meta::Path(_) => Ok(ContextMode::Display),
        Meta::List(_) => {
            let mode: Ident = attr.parse_args()?;
            if mode == "value" {
                Ok(ContextMode::Value)
            } else {
                Err(syn::Error::new_spanned(
                    mode,
                    "unknown context mode; use #[context] for Display formatting or \
                     #[context(value)] to preserve a native tracing value",
                ))
            }
        }
        Meta::NameValue(_) => Err(syn::Error::new_spanned(
            attr,
            "context is an attribute: use #[context] or #[context(value)]",
        )),
    }
}

/// Whether a context field's type is written as `Option<...>`, which is how an
/// Event says a field does not apply to every case it covers. The check is
/// syntactic, so a type alias for an option (`type MaybeWorker = Option<Uuid>`)
/// is not recognized -- that surfaces as a compile error at the generated call,
/// never as a silently wrong log line.
fn is_option(ty: &syn::Type) -> bool {
    let syn::Type::Path(path) = ty else {
        return false;
    };
    if path.qself.is_some() {
        return false;
    }
    let segments: Vec<String> = path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    // `Option<T>`, or the same spelled out in full through `std`/`core`.
    // Anything else named `Option` -- including a bare `option::Option` from
    // somebody's own module -- is their type and is left alone.
    let spelled_as_the_std_option = matches!(
        segments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["Option"] | ["std" | "core", "option", "Option"]
    );
    let takes_one_type_argument = path.path.segments.last().is_some_and(|segment| {
        matches!(&segment.arguments, syn::PathArguments::AngleBracketed(args)
            if args.args.len() == 1
                && matches!(args.args.first(), Some(syn::GenericArgument::Type(_))))
    });
    spelled_as_the_std_option && takes_one_type_argument
}

fn classify_field(field: &Field) -> syn::Result<FieldKind> {
    let mut kinds = Vec::new();
    for attr in &field.attrs {
        if attr.path().is_ident("label") {
            kinds.push(FieldKind::Label);
        } else if attr.path().is_ident("context") {
            kinds.push(FieldKind::Context(context_mode(attr)?));
        } else if attr.path().is_ident("observation") {
            require_bare_field_attribute(attr, "observation")?;
            kinds.push(FieldKind::Observation);
        }
    }
    match kinds.as_slice() {
        [kind] => Ok(*kind),
        [] => Err(syn::Error::new_spanned(
            field,
            "every Event field needs exactly one of #[label] (bounded, on the metric and the \
             log), #[context] (log-only), or #[observation] (the histogram value)",
        )),
        _ => Err(syn::Error::new_spanned(
            field,
            "an Event field takes only one of #[label], #[context], #[observation]",
        )),
    }
}

fn require_bare_field_attribute(attr: &syn::Attribute, name: &str) -> syn::Result<()> {
    if matches!(&attr.meta, Meta::Path(_)) {
        return Ok(());
    }
    Err(syn::Error::new_spanned(
        attr,
        format!("#[{name}] does not accept arguments; use bare #[{name}]"),
    ))
}

/// Validate an exposed metric label key, whether it comes from the Rust field
/// name or an explicit compatibility alias.
fn validate_metric_label_name(value: String, span: proc_macro2::Span) -> syn::Result<String> {
    let mut chars = value.chars();
    let valid_first = chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic());
    let valid_rest = chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric());
    if !valid_first || !valid_rest {
        return Err(syn::Error::new(
            span,
            "metric label names start with an ASCII letter or underscore and contain only ASCII \
             letters, digits, and underscores",
        ));
    }
    Ok(value)
}

/// `label_metric_name` resolves the metric key for one `#[label]` field. A bare
/// attribute uses the Rust field name for both the metric and generated log;
/// `name = "..."` changes only the metric key. That lets a frozen metric label
/// such as `component` coexist with the `Event` log's reserved field names.
fn label_metric_name(field: &Field) -> syn::Result<String> {
    let ident = field.ident.as_ref().expect("named field");
    let attr = field
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("label"))
        .expect("label field was classified above");

    match &attr.meta {
        Meta::Path(_) => validate_metric_label_name(ident.to_string(), ident.span()),
        Meta::List(_) => {
            let mut name: Option<LitStr> = None;
            attr.parse_nested_meta(|meta| {
                if !meta.path.is_ident("name") {
                    return Err(meta.error("unknown `label` key; expected `name`"));
                }
                if name.is_some() {
                    return Err(meta.error("duplicate label `name`"));
                }
                name = Some(meta.value()?.parse()?);
                Ok(())
            })?;

            let name = name.ok_or_else(|| {
                syn::Error::new_spanned(attr, "#[label(...)] requires name = \"...\"")
            })?;
            validate_metric_label_name(name.value(), name.span())
        }
        Meta::NameValue(_) => Err(syn::Error::new_spanned(
            attr,
            "use #[label(name = \"...\")] to alias a metric label key",
        )),
    }
}

fn validate_event_log_field(log: LogSpec, kind: FieldKind, ident: &Ident) -> syn::Result<()> {
    if ident == "message" {
        return Err(syn::Error::new_spanned(
            ident,
            "`message` is reserved for the event message; pick another field name",
        ));
    }
    if log != LogSpec::Off
        && matches!(kind, FieldKind::Label | FieldKind::Context(_))
        && is_event_log_reserved_field(&ident.to_string())
    {
        return Err(syn::Error::new_spanned(
            ident,
            format!(
                "`{ident}` is reserved by Event-generated logs or the log formatter; choose a \
                 domain-specific field name"
            ),
        ));
    }
    Ok(())
}

/// One entry of an enum Event's `labels(name: Type, ...)` schema: the metric
/// label key and the `LabelValue` type every variant supplies it as.
struct LabelSchemaEntry {
    name: Ident,
    ty: syn::Type,
}

impl Parse for LabelSchemaEntry {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: Ident = input.parse()?;
        input.parse::<Token![:]>()?;
        let ty: syn::Type = input.parse()?;
        Ok(Self { name, ty })
    }
}

/// One entry of a variant's `labels(name = Variant, ...)`: a label this case
/// fixes to a constant, rather than taking as data.
struct LabelFixedEntry {
    name: Ident,
    /// The label's value. A bare variant (`Release`) is resolved against the
    /// enum's own `labels(...)` schema; a family-backed enum has no schema
    /// here, so it names the type in full (`WorkLockOperation::Release`).
    value: syn::Path,
}

impl Parse for LabelFixedEntry {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: Ident = input.parse()?;
        input.parse::<Token![=]>()?;
        let value: syn::Path = input.parse()?;
        Ok(Self { name, value })
    }
}

/// The per-variant `#[event(...)]` on an enum Event.
struct VariantArgs {
    fixed: Vec<LabelFixedEntry>,
    message: MessageSpec,
    log: Option<LogSpec>,
}

fn parse_variant_args(variant: &syn::Variant) -> syn::Result<VariantArgs> {
    let mut args = VariantArgs {
        fixed: Vec::new(),
        message: MessageSpec::None,
        log: None,
    };
    for attr in &variant.attrs {
        if !attr.path().is_ident("event") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("labels") {
                let content;
                syn::parenthesized!(content in meta.input);
                for entry in content.parse_terminated(LabelFixedEntry::parse, Token![,])? {
                    if args.fixed.iter().any(|held| held.name == entry.name) {
                        return Err(syn::Error::new_spanned(
                            &entry.name,
                            format!("`{}` is fixed twice by this variant", entry.name),
                        ));
                    }
                    args.fixed.push(entry);
                }
            } else if meta.path.is_ident("message") {
                let value = meta.value()?;
                if value.peek(LitStr) {
                    args.message = MessageSpec::Static(value.parse()?);
                } else {
                    return Err(meta.error("a variant message must be a string literal"));
                }
            } else if meta.path.is_ident("log") {
                let ident: Ident = meta.value()?.parse()?;
                args.log = Some(match ident.to_string().as_str() {
                    "off" => LogSpec::Off,
                    "error" => LogSpec::Error,
                    "warn" => LogSpec::Warn,
                    "info" => LogSpec::Info,
                    "debug" => LogSpec::Debug,
                    "trace" => LogSpec::Trace,
                    other => {
                        return Err(meta.error(format!("unknown log level `{other}`")));
                    }
                });
            } else {
                return Err(meta.error(
                    "an enum Event variant takes only `labels(...)`, `message`, and `log`; the \
                     metric side is declared once on the enum",
                ));
            }
            Ok(())
        })?;
    }
    Ok(args)
}

/// A fixed label's value. Written bare (`Release`) it names a variant of the
/// schema's type; written in full (`WorkLockOperation::Release`) it stands on
/// its own, which is how a family-backed enum spells it since the family owns
/// the schema.
fn qualify(value: &syn::Path, schema_ty: Option<&syn::Type>) -> proc_macro2::TokenStream {
    match (value.segments.len(), schema_ty) {
        (1, Some(ty)) => {
            let variant = &value.segments[0].ident;
            quote! { #ty::#variant }
        }
        _ => quote! { #value },
    }
}

/// Expands `#[derive(Event)]` on an enum: one metric, one `event_name`, and a
/// case per variant. The enum declares the metric label schema once; each
/// variant accounts for every key in it, either by fixing a constant or by
/// taking one as a `#[label]` field. Message and level live on the variant they
/// describe, so a variant's log line renders exactly its own fields -- there is
/// no union struct and so nothing needs to be `Option` to say "not this case".
fn expand_event_enum(input: &DeriveInput, args: &EventArgs) -> syn::Result<TokenStream> {
    let enum_ident = &input.ident;
    let Data::Enum(data) = &input.data else {
        unreachable!("expand_event_enum is only called for enums");
    };

    let event_name = args.event_name.as_ref().ok_or_else(|| {
        syn::Error::new_spanned(enum_ident, "#[event(...)] requires event_name = \"...\"")
    })?;
    if let Err(error) = validate_event_name(&event_name.value()) {
        return Err(syn::Error::new_spanned(event_name, error));
    }
    // The message is the one thing an enum Event moves to the variant: a shared
    // message here would read as the message every case logs, and it is not.
    if !matches!(args.message, MessageSpec::None) {
        return Err(syn::Error::new_spanned(
            enum_ident,
            "an enum Event declares message = \"...\" on each variant, next to the case it \
             describes",
        ));
    }
    let family = args.metric_family.as_ref();
    if family.is_some() && !args.labels.is_empty() {
        return Err(syn::Error::new_spanned(
            enum_ident,
            "the metric family declares the label schema; drop `labels(...)` here and name each \
             variant's values in full",
        ));
    }
    if family.is_none() && args.metric == MetricSpec::None {
        return Err(syn::Error::new_spanned(
            enum_ident,
            "an enum Event declares metric = counter or metric = histogram",
        ));
    }
    if family.is_some() && args.metric != MetricSpec::None {
        return Err(syn::Error::new_spanned(
            enum_ident,
            "`metric` is declared by the metric family, not by an Event that uses it",
        ));
    }
    // The family owns every metric-side key. Taking one here silently exports
    // the family's value instead, so the declaration and the metric disagree.
    if let Some(family) = family {
        let moved: [(Option<&LitStr>, &str); 4] = [
            (args.metric_name.as_ref(), "metric_name"),
            (args.describe.as_ref(), "describe"),
            (args.unit.as_ref(), "unit"),
            (args.component.as_ref(), "component"),
        ];
        for (value, key) in moved {
            if let Some(value) = value {
                return Err(syn::Error::new_spanned(
                    value,
                    format!(
                        "`{key}` is declared by the `{}` metric family, not by an Event that uses \
                         it; remove it here",
                        family_name(family)
                    ),
                ));
            }
        }
        if args.metric_name_unchecked || args.describe_unchecked {
            return Err(syn::Error::new_spanned(
                enum_ident,
                "the metric_name_unchecked and describe_unchecked escape hatches belong on the \
                 metric family's #[metric(...)], not on an Event that uses it",
            ));
        }
    }
    // A family owns the metric side; without one the enum declares it, under
    // the same name/unit/describe conventions a struct Event follows.
    let (unit_value, declared_kind) = match (family, args.metric_name.as_ref()) {
        (Some(_), _) => (String::new(), MetricSpec::None),
        (None, Some(metric_name)) => {
            let unit = validate_metric(
                &MetricDeclaration {
                    metric_name,
                    kind: args.metric,
                    unit: args.unit.as_ref(),
                    describe: args.describe.as_ref(),
                    metric_name_unchecked: args.metric_name_unchecked,
                    describe_unchecked: args.describe_unchecked,
                },
                enum_ident,
            )?;
            (unit, args.metric)
        }
        (None, None) => {
            return Err(syn::Error::new_spanned(
                enum_ident,
                "an enum Event requires metric_name = \"...\", or a metric_family that declares it",
            ));
        }
    };

    let schema_names: Vec<&Ident> = args.labels.iter().map(|entry| &entry.name).collect();
    // A schema key every variant fixes never reaches a #[label] field, so this
    // is the only place it is held to the metric-label grammar. The reserved
    // log names depend on the level, which a variant can raise, so that check
    // belongs in the variant loop.
    for name in &schema_names {
        validate_metric_label_name(name.to_string(), name.span())?;
    }
    let n_labels = schema_names.len();
    let label_keys: Vec<String> = schema_names.iter().map(|name| name.to_string()).collect();

    let mut label_arms = Vec::new();
    let mut message_arms = Vec::new();
    let mut log_at_arms = Vec::new();
    let mut log_arms = Vec::new();
    let mut observation_arms = Vec::new();
    let mut context_arms = Vec::new();
    // Without a family the enum declares the kind, so whether a variant supplies
    // an `#[observation]` is settled here. With one the kind is only known at
    // the type level, so the variants are held to each other and then to the
    // family in a const assertion below.
    let declared_observation = family
        .is_none()
        .then(|| declared_kind.records_observation());
    let mut inferred_observation: Option<bool> = None;
    let observation_unit = match family {
        Some(family) => quote! {
            ::carbide_instrument::__private::metric_unit(
                <#family as ::carbide_instrument::MetricFamily>::METRIC,
            )
        },
        None => quote! { #unit_value },
    };
    let metric_name_log = match family {
        Some(family) => quote! { <#family as ::carbide_instrument::MetricFamily>::METRIC_NAME },
        None => {
            let name = args.metric_name.as_ref().expect("checked above");
            quote! { #name }
        }
    };

    for variant in &data.variants {
        let variant_ident = &variant.ident;
        let variant_args = parse_variant_args(variant)?;
        let log = variant_args.log.unwrap_or(args.log);
        // An enum Event's level is a property of the case, so it is declared on
        // the variant. `log = dynamic` is the struct-Event escape hatch for a
        // level that varies per instance, which a variant does not need.
        if log == LogSpec::Dynamic {
            return Err(syn::Error::new_spanned(
                variant,
                "log = dynamic is not used on an enum Event: declare the level on each variant",
            ));
        }
        // A key this variant fixes reaches its log line, so it is held to the
        // reserved names at the level this variant actually logs at.
        for name in &schema_names {
            validate_event_log_field(log, FieldKind::Label, name)?;
        }
        // A variant that logs needs its own message, so the line a reader sees
        // sits next to the case that produces it. Without this the variant
        // would log an empty message.
        if log != LogSpec::Off && matches!(variant_args.message, MessageSpec::None) {
            return Err(syn::Error::new_spanned(
                variant,
                format!("`{variant_ident}` logs, so it declares its own message = \"...\""),
            ));
        }

        let fields: Vec<&Field> = match &variant.fields {
            Fields::Named(named) => named.named.iter().collect(),
            Fields::Unit => Vec::new(),
            Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    variant,
                    "an enum Event variant uses named fields (or none)",
                ));
            }
        };

        let mut field_labels: Vec<(&Ident, String)> = Vec::new();
        let mut contexts: Vec<(&Ident, ContextMode, bool)> = Vec::new();
        let mut observations: Vec<&Ident> = Vec::new();
        for field in fields {
            let ident = field.ident.as_ref().expect("named field");
            let field_kind = classify_field(field)?;
            validate_event_log_field(log, field_kind, ident)?;
            match field_kind {
                FieldKind::Label => {
                    // Two fields can resolve to one metric key through
                    // #[label(name = "...")], and only the first would be read.
                    let key = label_metric_name(field)?;
                    if field_labels.iter().any(|(_, held)| held == &key) {
                        return Err(syn::Error::new_spanned(
                            field,
                            format!("duplicate metric label name `{key}`"),
                        ));
                    }
                    field_labels.push((ident, key));
                }
                FieldKind::Context(mode) => {
                    if mode == ContextMode::Value && is_option(&field.ty) {
                        return Err(syn::Error::new_spanned(
                            field,
                            "#[context(value)] does not take an Option; a variant that does not \
                             have this field simply omits it",
                        ));
                    }
                    contexts.push((ident, mode, is_option(&field.ty)));
                }
                FieldKind::Observation => observations.push(ident),
            }
        }

        let fixed_keys: Vec<&Ident> = variant_args.fixed.iter().map(|f| &f.name).collect();
        let fixed_vals: Vec<proc_macro2::TokenStream> = variant_args
            .fixed
            .iter()
            .map(|f| qualify(&f.value, None))
            .collect();

        // Every schema key is accounted for exactly once: fixed by the variant,
        // or taken as one of its fields.
        let mut values = Vec::new();
        for entry in &args.labels {
            let fixed = variant_args
                .fixed
                .iter()
                .find(|held| held.name == entry.name);
            let from_field = field_labels
                .iter()
                .find(|(_, key)| entry.name == key.as_str());
            match (fixed, from_field) {
                (Some(fixed), None) => {
                    let value = qualify(&fixed.value, Some(&entry.ty));
                    values.push(quote! { &#value });
                }
                (None, Some((ident, _))) => values.push(quote! { #ident }),
                (Some(fixed), Some(_)) => {
                    return Err(syn::Error::new_spanned(
                        &fixed.name,
                        format!(
                            "label `{}` is both fixed by `{}` and taken as one of its fields; \
                             pick one",
                            entry.name, variant_ident
                        ),
                    ));
                }
                (None, None) => {
                    return Err(syn::Error::new_spanned(
                        variant,
                        format!(
                            "`{}` does not supply the `{}` label: fix it with \
                             #[event(labels({} = ...))] or take it as a #[label] field",
                            variant_ident, entry.name, entry.name
                        ),
                    ));
                }
            }
        }
        for (ident, key) in &field_labels {
            // With a family there is no schema here to check against; the
            // family's struct literal catches an unknown or missing label.
            if family.is_none() && !schema_names.iter().any(|name| *name == key.as_str()) {
                return Err(syn::Error::new_spanned(
                    ident,
                    format!(
                        "`{key}` is not in this Event's label schema; add it to \
                         #[event(labels(...))] on the enum, or make it #[context]"
                    ),
                ));
            }
        }
        // The same check for a value the variant fixes: off the schema, nothing
        // reads it, so it would neither label the metric nor reach the log.
        if family.is_none() {
            for fixed in &variant_args.fixed {
                if !schema_names.iter().any(|name| **name == fixed.name) {
                    return Err(syn::Error::new_spanned(
                        &fixed.name,
                        format!(
                            "`{}` is not in this Event's label schema; add it to \
                             #[event(labels(...))] on the enum",
                            fixed.name
                        ),
                    ));
                }
            }
        }

        // A histogram records a value per emit, so every case must supply one;
        // a counter just increments and must not.
        if observations.len() > 1 {
            return Err(syn::Error::new_spanned(
                variant,
                format!("`{variant_ident}` has more than one #[observation] field"),
            ));
        }
        let variant_observes = observations.len() == 1;
        match declared_observation {
            Some(true) if !variant_observes => {
                return Err(syn::Error::new_spanned(
                    variant,
                    format!(
                        "`{variant_ident}` records a histogram, so it needs exactly one \
                         #[observation] field"
                    ),
                ));
            }
            Some(false) if variant_observes => {
                return Err(syn::Error::new_spanned(
                    variant,
                    format!(
                        "`{variant_ident}` records a counter, which counts emits rather than \
                         values; remove the #[observation] field"
                    ),
                ));
            }
            Some(_) => {}
            None => match inferred_observation {
                None => inferred_observation = Some(variant_observes),
                Some(first) if first != variant_observes => {
                    return Err(syn::Error::new_spanned(
                        variant,
                        format!(
                            "`{variant_ident}` disagrees with the earlier variants about \
                             #[observation]: a histogram family needs one on every variant, and a \
                             counter family needs none"
                        ),
                    ));
                }
                Some(_) => {}
            },
        }

        // Each arm binds exactly what it reads: `labels` reads the label
        // fields, the log call reads those plus context, and `message`/`log_at`
        // answer from the variant alone. Binding more would trip
        // `unused_variables` under `-D warnings`.
        let label_binds: Vec<&Ident> = field_labels.iter().map(|(ident, _)| *ident).collect();
        let log_binds: Vec<&Ident> = field_labels
            .iter()
            .map(|(ident, _)| *ident)
            .chain(contexts.iter().map(|(ident, _, _)| *ident))
            .collect();
        let label_pattern = quote! { Self::#variant_ident { #(#label_binds,)* .. } };
        if let Some(obs) = observations.first() {
            observation_arms.push(quote! {
                Self::#variant_ident { #obs, .. } => {
                    ::carbide_instrument::Observation::observe_as(#obs, #observation_unit)
                }
            });
        }
        let pattern = quote! { Self::#variant_ident { #(#log_binds,)* .. } };
        let bare_pattern = quote! { Self::#variant_ident { .. } };

        // The same fields the log line renders, for callers that read an
        // event's context rather than log it.
        let context_binds: Vec<&Ident> = contexts.iter().map(|(ident, _, _)| *ident).collect();
        let context_pushes = contexts.iter().map(|(ident, mode, optional)| {
            let name = ident.to_string();
            match (mode, optional) {
                (ContextMode::Display, true) => quote! {
                    if let ::std::option::Option::Some(__value) = #ident {
                        __context.push(
                            ::carbide_instrument::__private::opentelemetry::KeyValue::new(
                                #name,
                                ::std::string::ToString::to_string(__value),
                            ),
                        );
                    }
                },
                (ContextMode::Display, false) => quote! {
                    __context.push(
                        ::carbide_instrument::__private::opentelemetry::KeyValue::new(
                            #name,
                            ::std::string::ToString::to_string(#ident),
                        ),
                    );
                },
                (ContextMode::Value, _) => quote! {
                    __context.push(
                        ::carbide_instrument::__private::opentelemetry::KeyValue::new(
                            #name,
                            ::std::clone::Clone::clone(#ident),
                        ),
                    );
                },
            }
        });
        context_arms.push(quote! {
            Self::#variant_ident { #(#context_binds,)* .. } => {
                #(#context_pushes)*
            }
        });

        label_arms.push(match family {
            // The family's struct literal is the check: a variant that misses a
            // label is E0063, an unknown one E0560, a wrong type E0308 -- all
            // spanned at the family, exactly as for a struct Event.
            Some(family) => {
                let keys: Vec<&Ident> = fixed_keys
                    .iter()
                    .copied()
                    .chain(field_labels.iter().map(|(ident, _)| *ident))
                    .collect();
                let vals: Vec<proc_macro2::TokenStream> = fixed_vals
                    .iter()
                    .cloned()
                    .chain(field_labels.iter().map(|(ident, _)| {
                        quote! {
                            ::std::clone::Clone::clone(#ident)
                        }
                    }))
                    .collect();
                quote! {
                    #label_pattern => ::carbide_instrument::MetricFamily::labels(&#family {
                        #(#keys: #vals,)*
                    }),
                }
            }
            None => quote! {
                #label_pattern => [
                    #(
                        ::carbide_instrument::__private::opentelemetry::KeyValue::new(
                            #label_keys,
                            ::carbide_instrument::LabelValue::label_value(#values),
                        ),
                    )*
                ],
            },
        });

        let message = match &variant_args.message {
            MessageSpec::Static(message) => quote! { #message },
            MessageSpec::Dynamic | MessageSpec::None => quote! { "" },
        };
        message_arms.push(quote! { #bare_pattern => #message, });

        let log_at = match log {
            LogSpec::Off => quote! { ::carbide_instrument::LogAt::Off },
            LogSpec::Error => {
                quote! { ::carbide_instrument::LogAt::Level(::carbide_instrument::__private::tracing::Level::ERROR) }
            }
            LogSpec::Warn => {
                quote! { ::carbide_instrument::LogAt::Level(::carbide_instrument::__private::tracing::Level::WARN) }
            }
            LogSpec::Info => {
                quote! { ::carbide_instrument::LogAt::Level(::carbide_instrument::__private::tracing::Level::INFO) }
            }
            LogSpec::Debug => {
                quote! { ::carbide_instrument::LogAt::Level(::carbide_instrument::__private::tracing::Level::DEBUG) }
            }
            LogSpec::Trace => {
                quote! { ::carbide_instrument::LogAt::Level(::carbide_instrument::__private::tracing::Level::TRACE) }
            }
            LogSpec::Dynamic => unreachable!("rejected above"),
        };
        log_at_arms.push(quote! { #bare_pattern => #log_at, });

        // Only the variant's declared level gets a `tracing::event!`, so the
        // generated log path is one site per variant rather than one per level.
        if log == LogSpec::Off {
            log_arms.push(quote! { #bare_pattern => {} });
        } else {
            let level = match log {
                LogSpec::Error => quote! { ERROR },
                LogSpec::Warn => quote! { WARN },
                LogSpec::Info => quote! { INFO },
                LogSpec::Debug => quote! { DEBUG },
                LogSpec::Trace => quote! { TRACE },
                LogSpec::Off | LogSpec::Dynamic => unreachable!("rejected above"),
            };
            let mut log_fields = vec![
                quote! { event_name = #event_name },
                quote! { metric_name = #metric_name_log },
            ];
            log_fields.extend(field_labels.iter().map(|(ident, _)| {
                quote! { #ident = ::carbide_instrument::LabelValue::label_value(#ident).as_str() }
            }));
            if family.is_some() {
                for f in &variant_args.fixed {
                    let name = &f.name;
                    let value = qualify(&f.value, None);
                    log_fields.push(quote! {
                        #name = ::carbide_instrument::LabelValue::label_value(&#value).as_str()
                    });
                }
            }
            for entry in &args.labels {
                if let Some(fixed) = variant_args
                    .fixed
                    .iter()
                    .find(|held| held.name == entry.name)
                {
                    let name = &entry.name;
                    let value = qualify(&fixed.value, Some(&entry.ty));
                    log_fields.push(quote! {
                        #name = ::carbide_instrument::LabelValue::label_value(&#value).as_str()
                    });
                }
            }
            log_fields.extend(contexts.iter().map(|(ident, mode, optional)| {
                match (mode, optional) {
                    (ContextMode::Display, true) => quote! {
                        #ident = #ident.as_ref().map(
                            ::carbide_instrument::__private::tracing::field::display,
                        )
                    },
                    (ContextMode::Display, false) => quote! { #ident = %#ident },
                    (ContextMode::Value, _) => quote! { #ident = #ident },
                }
            }));
            let message_expr = match &variant_args.message {
                MessageSpec::Static(message) => quote! { #message },
                MessageSpec::Dynamic | MessageSpec::None => quote! { "" },
            };
            log_arms.push(quote! {
                #pattern => ::carbide_instrument::__private::tracing::event!(
                    name: #event_name,
                    ::carbide_instrument::__private::tracing::Level::#level,
                    #(#log_fields,)*
                    "{}",
                    #message_expr
                ),
            });
        }
    }

    // Either the family answers for the metric side, or the enum declares it.
    let (
        metric_name_const,
        component_const,
        describe_const,
        metric_const,
        labels_ty,
        instrument_fn,
    ) = match family {
        Some(family) => (
            quote! {
                ::std::option::Option::Some(
                    <#family as ::carbide_instrument::MetricFamily>::METRIC_NAME,
                )
            },
            quote! { <#family as ::carbide_instrument::MetricFamily>::COMPONENT },
            quote! { <#family as ::carbide_instrument::MetricFamily>::DESCRIBE },
            quote! { <#family as ::carbide_instrument::MetricFamily>::METRIC },
            quote! { <#family as ::carbide_instrument::MetricFamily>::Labels },
            quote! {
                fn __instrument(&self) -> &'static ::carbide_instrument::__private::CachedInstrument {
                    <#family as ::carbide_instrument::MetricFamily>::__instrument()
                }
            },
        ),
        None => {
            let metric_name = args.metric_name.as_ref().expect("checked above");
            let component = args.component.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(enum_ident, "#[event(...)] requires component = \"...\"")
            })?;
            let describe = args
                .describe
                .as_ref()
                .map(LitStr::value)
                .unwrap_or_default();
            let kind = match declared_kind {
                MetricSpec::Histogram => {
                    quote! { ::carbide_instrument::MetricKind::Histogram { unit: #unit_value } }
                }
                MetricSpec::Gauge => {
                    quote! { ::carbide_instrument::MetricKind::Gauge { unit: #unit_value } }
                }
                _ => quote! { ::carbide_instrument::MetricKind::Counter },
            };
            (
                quote! { ::std::option::Option::Some(#metric_name) },
                quote! { #component },
                quote! { #describe },
                kind,
                quote! { [::carbide_instrument::__private::opentelemetry::KeyValue; #n_labels] },
                quote! {
                    fn __instrument(&self) -> &'static ::carbide_instrument::__private::CachedInstrument {
                        static INSTRUMENT: ::std::sync::OnceLock<
                            ::carbide_instrument::__private::CachedInstrument,
                        > = ::std::sync::OnceLock::new();
                        INSTRUMENT.get_or_init(|| {
                            ::carbide_instrument::__private::new_instrument(
                                <Self as ::carbide_instrument::Event>::METRIC_NAME,
                                <Self as ::carbide_instrument::Event>::METRIC,
                                <Self as ::carbide_instrument::Event>::DESCRIBE,
                            )
                        })
                    }
                },
            )
        }
    };

    // A family's kind is only known at the type level from here, so the
    // histogram/observation agreement becomes a const assertion instead of a
    // macro-time check.
    let has_observation = declared_observation
        .or(inferred_observation)
        .unwrap_or(false);
    let observation_agreement = family.map(|family| {
        quote! {
            const _: () = {
                assert!(
                    ::carbide_instrument::__private::records_observation(
                        <#family as ::carbide_instrument::MetricFamily>::METRIC,
                    ) == #has_observation,
                    "a histogram metric family needs exactly one #[observation] field on every \
                     variant of an Event that uses it, and a counter family needs none",
                );
            };
        }
    });
    let observation_fn = has_observation.then(|| {
        quote! {
            fn observation(&self) -> f64 {
                match self { #(#observation_arms)* }
            }
        }
    });

    Ok(quote! {
        #observation_agreement

        impl ::carbide_instrument::Event for #enum_ident {
            const EVENT_NAME: &'static str = #event_name;
            const METRIC_NAME: ::std::option::Option<&'static str> = #metric_name_const;
            const COMPONENT: &'static str = #component_const;
            const DESCRIBE: &'static str = #describe_const;
            const METRIC: ::carbide_instrument::MetricKind = #metric_const;
            type Labels = #labels_ty;

            fn message(&self) -> &'static str {
                match self { #(#message_arms)* }
            }

            fn labels(&self) -> Self::Labels {
                match self { #(#label_arms)* }
            }

            fn log_at(&self) -> ::carbide_instrument::LogAt {
                match self { #(#log_at_arms)* }
            }

            fn context(&self) -> ::std::vec::Vec<::carbide_instrument::__private::opentelemetry::KeyValue> {
                let mut __context = ::std::vec::Vec::new();
                match self { #(#context_arms)* }
                __context
            }

            #observation_fn

            fn __log(&self, _level: ::carbide_instrument::__private::tracing::Level) {
                match self { #(#log_arms)* }
            }

            #instrument_fn
        }
    }
    .into())
}

fn expand_event(input: DeriveInput) -> syn::Result<TokenStream> {
    let struct_ident = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "Event structs must be concrete (no generics or lifetimes): declare the event with \
             owned fields",
        ));
    }

    let args = parse_event_args(&input)?;

    if matches!(&input.data, Data::Enum(_)) {
        return expand_event_enum(&input, &args);
    }
    // A struct Event's #[label] fields are its schema, so a `labels(...)` list
    // here would name a schema nothing reads.
    if let Some(entry) = args.labels.first() {
        return Err(syn::Error::new_spanned(
            &entry.name,
            "`labels(...)` declares an enum Event's label schema; a struct Event's #[label] \
             fields are its schema",
        ));
    }

    let event_name = args.event_name.as_ref().ok_or_else(|| {
        syn::Error::new_spanned(&input.ident, "#[event(...)] requires event_name = \"...\"")
    })?;
    if let Err(error) = validate_event_name(&event_name.value()) {
        return Err(syn::Error::new_spanned(event_name, error));
    }
    // A family already declares the metric side, so an Event that names one
    // states only what is its own: identity, level, and message.
    let family = args.metric_family.as_ref();
    if let Some(family) = family {
        if args.metric != MetricSpec::None {
            return Err(syn::Error::new_spanned(
                family,
                format!(
                    "`metric` is declared by the `{}` metric family, not by an Event that uses it; \
                     remove it here",
                    family_name(family)
                ),
            ));
        }
        let moved: [(Option<&LitStr>, &str); 4] = [
            (args.metric_name.as_ref(), "metric_name"),
            (args.describe.as_ref(), "describe"),
            (args.unit.as_ref(), "unit"),
            (args.component.as_ref(), "component"),
        ];
        for (value, key) in moved {
            if let Some(value) = value {
                return Err(syn::Error::new_spanned(
                    value,
                    format!(
                        "`{key}` is declared by the `{}` metric family, not by an Event that uses \
                         it; remove it here",
                        family_name(family)
                    ),
                ));
            }
        }
        if args.metric_name_unchecked || args.describe_unchecked {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "the metric_name_unchecked and describe_unchecked escape hatches belong on the \
                 metric family's #[metric(...)], not on an Event that uses it",
            ));
        }
    }

    let component = match (family, args.component.as_ref()) {
        (Some(_), _) => None,
        (None, Some(component)) => Some(component),
        (None, None) => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "#[event(...)] requires component = \"...\"",
            ));
        }
    };

    let metric_name = match (&args.metric, args.metric_name.as_ref()) {
        (MetricSpec::None, Some(metric_name)) => {
            return Err(syn::Error::new_spanned(
                metric_name,
                "metric_name is only valid when metric is counter or histogram",
            ));
        }
        (MetricSpec::None, _) => None,
        (_, Some(metric_name)) => Some(metric_name),
        (_, None) => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "metric_name = \"...\" is required when metric is counter or histogram",
            ));
        }
    };
    if args.metric_name_unchecked && metric_name.is_none() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "metric_name_unchecked is only valid for a metric-backed Event",
        ));
    }
    if let Some(describe) = &args.describe
        && args.metric == MetricSpec::None
    {
        return Err(syn::Error::new_spanned(
            describe,
            "`describe` documents a metric (the Prometheus HELP text); this event has \
             metric = none",
        ));
    }
    if let Some(unit) = &args.unit
        && args.metric == MetricSpec::None
    {
        return Err(syn::Error::new_spanned(
            unit,
            "`unit` is only valid for histogram metrics",
        ));
    }

    let unit_value = match metric_name {
        Some(metric_name) => validate_metric(
            &MetricDeclaration {
                metric_name,
                kind: args.metric,
                unit: args.unit.as_ref(),
                describe: args.describe.as_ref(),
                metric_name_unchecked: args.metric_name_unchecked,
                describe_unchecked: args.describe_unchecked,
            },
            &input.ident,
        )?,
        None => String::new(),
    };

    if matches!(args.message, MessageSpec::None) && args.log != LogSpec::Off {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "a message is required when the event logs: set message = \"...\" or \
             message = dynamic (or set log = off for a metric-only event)",
        ));
    }
    if args.log == LogSpec::Off && args.metric == MetricSpec::None && family.is_none() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "an event with log = off and metric = none emits nothing; declare at least one side",
        ));
    }

    // Classify the fields.
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "Event can only be derived for structs and enums",
        ));
    };
    let fields: Vec<&Field> = match &data.fields {
        Fields::Named(named) => named.named.iter().collect(),
        Fields::Unit => Vec::new(),
        Fields::Unnamed(_) => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "Event structs use named fields (or none)",
            ));
        }
    };

    let mut labels: Vec<(&Ident, String)> = Vec::new();
    let mut contexts: Vec<(&Ident, ContextMode, bool)> = Vec::new();
    let mut observations: Vec<&Ident> = Vec::new();
    for field in fields {
        let ident = field.ident.as_ref().expect("named field");
        let field_kind = classify_field(field)?;
        validate_event_log_field(args.log, field_kind, ident)?;
        match field_kind {
            FieldKind::Label => {
                let metric_name = label_metric_name(field)?;
                // A family-backed Event supplies label *values*; the metric
                // keys come from the family's own fields, so an alias here
                // would silently do nothing.
                if let Some(family) = family
                    && *ident != metric_name
                {
                    return Err(syn::Error::new_spanned(
                        field,
                        format!(
                            "#[label(name = \"...\")] aliases a metric label key, which the `{}` \
                             metric family owns; declare the alias on the family's field instead",
                            family_name(family)
                        ),
                    ));
                }
                if labels.iter().any(|(_, name)| name == &metric_name) {
                    return Err(syn::Error::new_spanned(
                        field,
                        format!("duplicate metric label name `{metric_name}`"),
                    ));
                }
                labels.push((ident, metric_name));
            }
            FieldKind::Context(mode) => {
                // The native-value mode exists for fields whose tracing type is
                // part of the structured-log contract; nothing needs an
                // optional one yet, so say that rather than guess at it.
                if mode == ContextMode::Value && is_option(&field.ty) {
                    return Err(syn::Error::new_spanned(
                        field,
                        "#[context(value)] does not take an Option; use #[context] for a field \
                         that does not apply to every case",
                    ));
                }
                contexts.push((ident, mode, is_option(&field.ty)));
            }
            FieldKind::Observation => observations.push(ident),
        }
    }

    match (&args.metric, observations.len()) {
        (MetricSpec::Histogram | MetricSpec::Gauge, 1) => {}
        (MetricSpec::Histogram, _) => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "a histogram event needs exactly one #[observation] field",
            ));
        }
        (MetricSpec::Gauge, _) => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "a gauge event needs exactly one #[observation] field: the value to set",
            ));
        }
        // A family declares the kind, which is only known at the type level
        // from here, so the generated const assertion checks that a histogram
        // family got its observation and a counter family did not.
        (_, 0 | 1) if family.is_some() => {}
        (_, _) if family.is_some() => {
            return Err(syn::Error::new_spanned(
                observations[1],
                "an Event records at most one #[observation]",
            ));
        }
        (_, 0) => {}
        (_, _) => {
            return Err(syn::Error::new_spanned(
                observations[0],
                "#[observation] requires metric = histogram",
            ));
        }
    }

    // The pieces of the generated impl.
    let n_labels = labels.len();
    let label_idents: Vec<&Ident> = labels.iter().map(|(ident, _)| *ident).collect();
    let label_names: Vec<&str> = labels.iter().map(|(_, name)| name.as_str()).collect();
    let context_names: Vec<String> = contexts
        .iter()
        .map(|(ident, _, _)| ident.to_string())
        .collect();

    // `log = dynamic` keeps the trait's nominal LOG and routes the decision
    // through the hand-implemented `DynamicLog` -- per-instance levels (count
    // everything, log only failures).
    let log_items = match args.log {
        LogSpec::Dynamic => quote! {
            fn log_at(&self) -> ::carbide_instrument::LogAt {
                ::carbide_instrument::DynamicLog::log_at(self)
            }
        },
        LogSpec::Off => log_const_item(quote! { ::carbide_instrument::LogAt::Off }),
        LogSpec::Error => log_const_item(level_const(quote! { ERROR })),
        LogSpec::Warn => log_const_item(level_const(quote! { WARN })),
        LogSpec::Info => log_const_item(level_const(quote! { INFO })),
        LogSpec::Debug => log_const_item(level_const(quote! { DEBUG })),
        LogSpec::Trace => log_const_item(level_const(quote! { TRACE })),
    };
    // The metric side is either declared inline or delegated wholesale to the
    // named family, which owns the name, kind, description, and label array.
    let (
        metric_const,
        metric_name_const,
        component_const,
        describe_const,
        labels_type,
        labels_fn,
        instrument_fn,
    ) = match family {
        Some(family) => (
            quote! { <#family as ::carbide_instrument::MetricFamily>::METRIC },
            quote! {
                ::std::option::Option::Some(
                    <#family as ::carbide_instrument::MetricFamily>::METRIC_NAME,
                )
            },
            quote! { <#family as ::carbide_instrument::MetricFamily>::COMPONENT },
            quote! { <#family as ::carbide_instrument::MetricFamily>::DESCRIBE },
            quote! { <#family as ::carbide_instrument::MetricFamily>::Labels },
            // Building the family by name is the whole check: a missing
            // label is E0063, an extra one E0560, a wrong type E0308 --
            // ordinary rustc diagnostics that name the family.
            quote! {
                fn labels(&self) -> Self::Labels {
                    ::carbide_instrument::MetricFamily::labels(&#family {
                        #(
                            #label_idents: ::std::clone::Clone::clone(&self.#label_idents),
                        )*
                    })
                }
            },
            quote! {
                fn __instrument(&self) -> &'static ::carbide_instrument::__private::CachedInstrument {
                    <#family as ::carbide_instrument::MetricFamily>::__instrument()
                }
            },
        ),
        None => {
            let metric_const = match &args.metric {
                MetricSpec::Counter => quote! { ::carbide_instrument::MetricKind::Counter },
                MetricSpec::Histogram => {
                    quote! { ::carbide_instrument::MetricKind::Histogram { unit: #unit_value } }
                }
                MetricSpec::Gauge => {
                    quote! { ::carbide_instrument::MetricKind::Gauge { unit: #unit_value } }
                }
                _ => quote! { ::carbide_instrument::MetricKind::None },
            };
            let metric_name_const = match metric_name {
                Some(metric_name) => quote! { ::std::option::Option::Some(#metric_name) },
                None => quote! { ::std::option::Option::None },
            };
            let describe_value = args
                .describe
                .as_ref()
                .map(LitStr::value)
                .unwrap_or_default();
            let component = component.expect("a non-family Event requires component");
            (
                metric_const,
                metric_name_const,
                quote! { #component },
                quote! { #describe_value },
                quote! { [::carbide_instrument::__private::opentelemetry::KeyValue; #n_labels] },
                quote! {
                    fn labels(&self) -> Self::Labels {
                        [
                            #(
                                ::carbide_instrument::__private::opentelemetry::KeyValue::new(
                                    #label_names,
                                    ::carbide_instrument::LabelValue::label_value(&self.#label_idents),
                                ),
                            )*
                        ]
                    }
                },
                quote! {
                    fn __instrument(&self) -> &'static ::carbide_instrument::__private::CachedInstrument {
                        static INSTRUMENT: ::std::sync::OnceLock<
                            ::carbide_instrument::__private::CachedInstrument,
                        > = ::std::sync::OnceLock::new();
                        INSTRUMENT.get_or_init(|| {
                            ::carbide_instrument::__private::new_instrument(
                                <Self as ::carbide_instrument::Event>::METRIC_NAME,
                                <Self as ::carbide_instrument::Event>::METRIC,
                                <Self as ::carbide_instrument::Event>::DESCRIBE,
                            )
                        })
                    }
                },
            )
        }
    };
    let message_body = match &args.message {
        MessageSpec::Static(message) => quote! { #message },
        MessageSpec::Dynamic => quote! { ::carbide_instrument::DynamicMessage::message(self) },
        MessageSpec::None => quote! { "" },
    };

    // A family's kind is only known at the type level from here, so the
    // histogram/observation agreement becomes a const assertion instead of a
    // macro-time check.
    let has_observation = !observations.is_empty();
    let observation_agreement = family.map(|family| {
        quote! {
            const _: () = {
                assert!(
                    ::carbide_instrument::__private::records_observation(
                        <#family as ::carbide_instrument::MetricFamily>::METRIC,
                    ) == #has_observation,
                    "a histogram metric family needs exactly one #[observation] field on each \
                     Event that uses it, and a counter family needs none",
                );
            };
        }
    });
    let observation_fn = observations.first().map(|obs| {
        let unit = match family {
            Some(family) => quote! {
                ::carbide_instrument::__private::metric_unit(
                    <#family as ::carbide_instrument::MetricFamily>::METRIC,
                )
            },
            None => quote! { #unit_value },
        };
        quote! {
            fn observation(&self) -> f64 {
                ::carbide_instrument::Observation::observe_as(&self.#obs, #unit)
            }
        }
    });

    // One tracing::event! per level: the macro needs a const level and static
    // field names, so the dispatch is generated here rather than written by hand.
    let mut log_fields = vec![quote! { event_name = #event_name }];
    match (family, metric_name) {
        (Some(family), _) => log_fields.push(quote! {
            metric_name = <#family as ::carbide_instrument::MetricFamily>::METRIC_NAME
        }),
        (None, Some(metric_name)) => log_fields.push(quote! { metric_name = #metric_name }),
        (None, None) => {}
    }
    log_fields.extend(label_idents.iter().map(|ident| {
        quote! {
            #ident = ::carbide_instrument::LabelValue::label_value(&self.#ident).as_str()
        }
    }));
    log_fields.extend(
        contexts
            .iter()
            .map(|(ident, mode, optional)| match (mode, optional) {
                // `tracing` records nothing for a `None`, so the key is absent from the
                // line rather than blank -- which is the whole point of declaring the
                // field optional.
                (ContextMode::Display, true) => quote! {
                    #ident = self.#ident.as_ref().map(
                        ::carbide_instrument::__private::tracing::field::display,
                    )
                },
                (ContextMode::Display, false) => quote! { #ident = %self.#ident },
                (ContextMode::Value, _) => quote! { #ident = self.#ident },
            }),
    );

    let context_pushes =
        contexts
            .iter()
            .zip(&context_names)
            .map(|((ident, mode, optional), name)| match (mode, optional) {
                (ContextMode::Display, true) => quote! {
                    if let ::std::option::Option::Some(__value) = &self.#ident {
                        __context.push(
                            ::carbide_instrument::__private::opentelemetry::KeyValue::new(
                                #name,
                                ::std::string::ToString::to_string(__value),
                            ),
                        );
                    }
                },
                (ContextMode::Display, false) => quote! {
                    __context.push(
                        ::carbide_instrument::__private::opentelemetry::KeyValue::new(
                            #name,
                            ::std::string::ToString::to_string(&self.#ident),
                        ),
                    );
                },
                (ContextMode::Value, _) => quote! {
                    __context.push(
                        ::carbide_instrument::__private::opentelemetry::KeyValue::new(
                            #name,
                            ::std::clone::Clone::clone(&self.#ident),
                        ),
                    );
                },
            });
    let log_arm = |level: proc_macro2::TokenStream| {
        quote! {
            ::carbide_instrument::__private::tracing::event!(
                name: #event_name,
                ::carbide_instrument::__private::tracing::Level::#level,
                #(#log_fields,)*
                "{}",
                __message
            )
        }
    };
    let (arm_error, arm_warn, arm_info, arm_debug, arm_trace) = (
        log_arm(quote! { ERROR }),
        log_arm(quote! { WARN }),
        log_arm(quote! { INFO }),
        log_arm(quote! { DEBUG }),
        log_arm(quote! { TRACE }),
    );
    let log_fn = if args.log == LogSpec::Off {
        quote! {
            fn __log(&self, _level: ::carbide_instrument::__private::tracing::Level) {}
        }
    } else {
        quote! {
            fn __log(&self, level: ::carbide_instrument::__private::tracing::Level) {
                let __message = ::carbide_instrument::Event::message(self);
                if level == ::carbide_instrument::__private::tracing::Level::ERROR {
                    #arm_error;
                } else if level == ::carbide_instrument::__private::tracing::Level::WARN {
                    #arm_warn;
                } else if level == ::carbide_instrument::__private::tracing::Level::INFO {
                    #arm_info;
                } else if level == ::carbide_instrument::__private::tracing::Level::DEBUG {
                    #arm_debug;
                } else {
                    #arm_trace;
                }
            }
        }
    };

    Ok(quote! {
        #observation_agreement

        impl ::carbide_instrument::Event for #struct_ident {
            const EVENT_NAME: &'static str = #event_name;
            const METRIC_NAME: ::std::option::Option<&'static str> = #metric_name_const;
            const COMPONENT: &'static str = #component_const;
            const DESCRIBE: &'static str = #describe_const;
            #log_items
            const METRIC: ::carbide_instrument::MetricKind = #metric_const;
            type Labels = #labels_type;

            fn message(&self) -> &'static str {
                #message_body
            }

            #labels_fn

            fn context(&self) -> ::std::vec::Vec<::carbide_instrument::__private::opentelemetry::KeyValue> {
                let mut __context = ::std::vec::Vec::new();
                #(#context_pushes)*
                __context
            }

            #observation_fn
            #log_fn
            #instrument_fn
        }
    }
    .into())
}

/// The family's own name, for a diagnostic that points at the declaration a
/// contributor has to go read.
fn family_name(path: &syn::Path) -> String {
    path.segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_default()
}

/// Derives `carbide_instrument::MetricFamily` for a struct declared with a
/// `#[metric(...)]` attribute, whose fields are the metric's labels. The family
/// owns the whole metric side -- `name`, `kind`, `component`, `describe`, and a
/// histogram's `unit` -- so every Event that names it moves one instrument with
/// one label set. Metric names and describe text are validated exactly as they
/// are for a metric declared inline on an Event, with the same
/// `metric_name_unchecked` and `describe_unchecked` escape hatches.
#[proc_macro_derive(MetricFamily, attributes(metric, label))]
pub fn derive_metric_family(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    match expand_metric_family(input) {
        Ok(ts) => ts,
        Err(e) => e.to_compile_error().into(),
    }
}

struct MetricArgs {
    name: Option<LitStr>,
    kind: Option<MetricSpec>,
    component: Option<LitStr>,
    describe: Option<LitStr>,
    unit: Option<LitStr>,
    metric_name_unchecked: bool,
    describe_unchecked: bool,
}

fn parse_metric_args(input: &DeriveInput) -> syn::Result<MetricArgs> {
    let mut args = MetricArgs {
        name: None,
        kind: None,
        component: None,
        describe: None,
        unit: None,
        metric_name_unchecked: false,
        describe_unchecked: false,
    };
    let mut saw_attr = false;

    for attr in &input.attrs {
        if !attr.path().is_ident("metric") {
            continue;
        }
        saw_attr = true;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                if args.name.is_some() {
                    return Err(meta.error("duplicate `name`"));
                }
                args.name = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("kind") {
                let ident: Ident = meta.value()?.parse()?;
                args.kind = Some(match ident.to_string().as_str() {
                    "counter" => MetricSpec::Counter,
                    "histogram" => MetricSpec::Histogram,
                    "gauge" => MetricSpec::Gauge,
                    other => {
                        return Err(meta.error(format!(
                            "unknown metric kind `{other}`; a family is counter | histogram | \
                             gauge (an Event with no metric declares metric = none instead)"
                        )));
                    }
                });
            } else if meta.path.is_ident("component") {
                args.component = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("describe") {
                args.describe = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("unit") {
                args.unit = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("metric_name_unchecked") {
                args.metric_name_unchecked = true;
            } else if meta.path.is_ident("describe_unchecked") {
                args.describe_unchecked = true;
            } else if meta.path.is_ident("metric_name") {
                return Err(meta.error(
                    "a metric family declares its exposed name as `name`; `metric_name` is the \
                     Event-side key",
                ));
            } else {
                return Err(meta.error(
                    "unknown `metric` key; expected name, kind, component, describe, unit, \
                     metric_name_unchecked, or describe_unchecked",
                ));
            }
            Ok(())
        })?;
    }

    if !saw_attr {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "deriving MetricFamily requires a #[metric(name = ..., kind = ..., component = ...)] \
             attribute",
        ));
    }
    Ok(args)
}

fn expand_metric_family(input: DeriveInput) -> syn::Result<TokenStream> {
    let struct_ident = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "MetricFamily structs must be concrete (no generics or lifetimes): the family is one \
             metric with one label set",
        ));
    }

    let args = parse_metric_args(&input)?;
    let name = args.name.as_ref().ok_or_else(|| {
        syn::Error::new_spanned(&input.ident, "#[metric(...)] requires name = \"...\"")
    })?;
    let component = args.component.as_ref().ok_or_else(|| {
        syn::Error::new_spanned(&input.ident, "#[metric(...)] requires component = \"...\"")
    })?;
    let kind = args.kind.ok_or_else(|| {
        syn::Error::new_spanned(
            &input.ident,
            "#[metric(...)] requires kind = counter, kind = histogram, or kind = gauge",
        )
    })?;

    let unit_value = validate_metric(
        &MetricDeclaration {
            metric_name: name,
            kind,
            unit: args.unit.as_ref(),
            describe: args.describe.as_ref(),
            metric_name_unchecked: args.metric_name_unchecked,
            describe_unchecked: args.describe_unchecked,
        },
        &input.ident,
    )?;

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "MetricFamily can only be derived for structs",
        ));
    };
    let fields: Vec<&Field> = match &data.fields {
        Fields::Named(named) => named.named.iter().collect(),
        Fields::Unit => Vec::new(),
        Fields::Unnamed(_) => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "MetricFamily structs use named fields (or none): every field is a label",
            ));
        }
    };

    // Every field is a label -- that is what a family is -- so the only field
    // attribute is the narrow `#[label(name = "...")]` metric-key alias.
    let mut labels: Vec<(&Ident, String)> = Vec::new();
    let mut labels_fields: Vec<&Field> = Vec::new();
    for field in fields {
        let ident = field.ident.as_ref().expect("named field");
        for attr in &field.attrs {
            if !attr.path().is_ident("label") && !attr.path().is_ident("doc") {
                return Err(syn::Error::new_spanned(
                    attr,
                    "a MetricFamily field is a label already; the only field attribute is \
                     #[label(name = \"...\")] to alias its metric key",
                ));
            }
        }
        let metric_name = if field.attrs.iter().any(|a| a.path().is_ident("label")) {
            label_metric_name(field)?
        } else {
            validate_metric_label_name(ident.to_string(), ident.span())?
        };
        if labels.iter().any(|(_, name)| name == &metric_name) {
            return Err(syn::Error::new_spanned(
                field,
                format!("duplicate metric label name `{metric_name}`"),
            ));
        }
        labels.push((ident, metric_name));
        labels_fields.push(field);
    }

    // Labels the family computes rather than takes: declared on the struct so
    // the family's fields stay exactly what an Event has to supply.

    let n_labels = labels.len();
    let label_idents: Vec<&Ident> = labels.iter().map(|(ident, _)| *ident).collect();
    let label_names: Vec<&str> = labels.iter().map(|(_, name)| name.as_str()).collect();
    let describe_value = args
        .describe
        .as_ref()
        .map(LitStr::value)
        .unwrap_or_default();
    let metric_const = match kind {
        MetricSpec::Gauge => {
            quote! { ::carbide_instrument::MetricKind::Gauge { unit: #unit_value } }
        }
        MetricSpec::Histogram => {
            quote! { ::carbide_instrument::MetricKind::Histogram { unit: #unit_value } }
        }
        _ => quote! { ::carbide_instrument::MetricKind::Counter },
    };

    let label_types: Vec<&syn::Type> = labels_fields.iter().map(|field| &field.ty).collect();

    Ok(quote! {
        // An Event supplies its labels by value, so a family's label types are
        // Clone. Assert it here, at the family, rather than letting every Event
        // that uses the family report the same missing bound.
        const _: () = {
            fn __assert_label_is_clone<T: ::std::clone::Clone + ::carbide_instrument::LabelValue>() {}
            fn __assert_family_labels() {
                #(__assert_label_is_clone::<#label_types>();)*
            }
        };

        impl ::carbide_instrument::MetricFamily for #struct_ident {
            const METRIC_NAME: &'static str = #name;
            const COMPONENT: &'static str = #component;
            const DESCRIBE: &'static str = #describe_value;
            const METRIC: ::carbide_instrument::MetricKind = #metric_const;
            type Labels = [::carbide_instrument::__private::opentelemetry::KeyValue; #n_labels];

            fn labels(&self) -> Self::Labels {
                [
                    #(
                        ::carbide_instrument::__private::opentelemetry::KeyValue::new(
                            #label_names,
                            ::carbide_instrument::LabelValue::label_value(&self.#label_idents),
                        ),
                    )*
                ]
            }

            fn __instrument() -> &'static ::carbide_instrument::__private::CachedInstrument {
                static INSTRUMENT: ::std::sync::OnceLock<
                    ::carbide_instrument::__private::CachedInstrument,
                > = ::std::sync::OnceLock::new();
                INSTRUMENT.get_or_init(|| {
                    ::carbide_instrument::__private::new_instrument(
                        ::std::option::Option::Some(
                            <Self as ::carbide_instrument::MetricFamily>::METRIC_NAME,
                        ),
                        <Self as ::carbide_instrument::MetricFamily>::METRIC,
                        <Self as ::carbide_instrument::MetricFamily>::DESCRIBE,
                    )
                })
            }
        }
    }
    .into())
}

fn log_const_item(value: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    quote! { const LOG: ::carbide_instrument::LogAt = #value; }
}

fn level_const(level: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    quote! {
        ::carbide_instrument::LogAt::Level(
            ::carbide_instrument::__private::tracing::Level::#level,
        )
    }
}

/// `PowerControl` -> `power_control`, `Rms` -> `rms`, `DHCPServer` -> `dhcp_server`.
fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            let prev_lower = i > 0 && chars[i - 1].is_lowercase();
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if i > 0 && (prev_lower || next_lower) {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(*c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case as TestCase, Check, check_cases, check_values};
    use proc_macro2::Span;
    use syn::{Data, DeriveInput, Fields, Ident};

    use super::{
        ContextMode, FieldKind, LogSpec, classify_field, expand_event, snake_case,
        validate_event_log_field,
    };

    fn expansion_error(source: &str) -> String {
        let input: DeriveInput = syn::parse_str(source).expect("valid derive input");
        expand_event(input)
            .expect_err("input should be rejected")
            .to_string()
    }

    #[test]
    fn snake_case_variants() {
        assert_eq!(snake_case("Rms"), "rms");
        assert_eq!(snake_case("PowerControl"), "power_control");
        assert_eq!(snake_case("DHCPServer"), "dhcp_server");
        assert_eq!(snake_case("Ok"), "ok");
        assert_eq!(snake_case("NoDpu"), "no_dpu");
        assert_eq!(snake_case("A"), "a");
    }

    #[test]
    fn event_identity_diagnostics_are_specific() {
        struct Case {
            scenario: &'static str,
            source: &'static str,
            expected: &'static str,
        }

        for Case {
            scenario,
            source,
            expected,
        } in [
            Case {
                scenario: "event name is required",
                source: r#"#[event(component = "demo", message = "demo")] struct Demo {}"#,
                expected: "requires event_name",
            },
            Case {
                scenario: "event name follows the shared grammar",
                source: r#"#[event(event_name = "demo.started", component = "demo", message = "demo")] struct Demo {}"#,
                expected: "ASCII lower_snake_case",
            },
            Case {
                scenario: "metric-backed event needs a metric name",
                source: r#"#[event(event_name = "demo", component = "demo", log = off, metric = counter)] struct Demo {}"#,
                expected: "metric_name = \"...\" is required",
            },
            Case {
                scenario: "log-only event rejects a metric name",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", message = "demo")] struct Demo {}"#,
                expected: "metric_name is only valid",
            },
            Case {
                scenario: "legacy name explains the split",
                source: r#"#[event(name = "carbide_demo_total", component = "demo", log = off, metric = counter)] struct Demo {}"#,
                expected: "has been split into `event_name` and `metric_name`",
            },
            Case {
                scenario: "unchecked escape hatch is metric-specific",
                source: r#"#[event(event_name = "demo", metric_name = "demo", component = "demo", log = off, metric = counter, name_unchecked)] struct Demo {}"#,
                expected: "renamed to `metric_name_unchecked`",
            },
            Case {
                scenario: "event name is declared once",
                source: r#"#[event(event_name = "first", event_name = "second", component = "demo", message = "demo")] struct Demo {}"#,
                expected: "duplicate `event_name`",
            },
            Case {
                scenario: "a gauge does not wear the counter's suffix",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", log = off, metric = gauge, describe = "Number of demo things")] struct Demo { #[observation] value: f64 }"#,
                expected: "`_total` is the counter suffix",
            },
            Case {
                scenario: "a gauge documents itself for the catalogue",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_things", component = "demo", log = off, metric = gauge)] struct Demo { #[observation] value: f64 }"#,
                expected: "a gauge must document itself",
            },
            Case {
                scenario: "a gauge needs the value it sets",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_things", component = "demo", log = off, metric = gauge, describe = "Number of demo things")] struct Demo {}"#,
                expected: "a gauge event needs exactly one #[observation] field",
            },
            Case {
                scenario: "metric name is declared once",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_first_total", metric_name = "carbide_second_total", component = "demo", log = off, metric = counter)] struct Demo {}"#,
                expected: "duplicate `metric_name`",
            },
        ] {
            let error = expansion_error(source);
            assert!(
                error.contains(expected),
                "{scenario}: expected `{expected}` in `{error}`"
            );
        }
    }

    /// The enum form moves the message and the level onto the variant, so the
    /// declarations that would quietly lose either one are rejected instead.
    #[test]
    fn enum_event_diagnostics_are_specific() {
        struct Case {
            scenario: &'static str,
            source: &'static str,
            expected: &'static str,
        }

        for Case {
            scenario,
            source,
            expected,
        } in [
            Case {
                scenario: "a shared message would not be the message any case logs",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", metric = counter, describe = "Number of demo events.", message = "shared", labels(outcome: Outcome))] enum Demo { #[event(labels(outcome = Ok), log = info)] Ok {} }"#,
                expected: "declares message = \"...\" on each variant",
            },
            Case {
                scenario: "a logging variant without a message would log an empty one",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", metric = counter, describe = "Number of demo events.", labels(outcome: Outcome))] enum Demo { #[event(labels(outcome = Ok), log = info)] Ok {} }"#,
                expected: "`Ok` logs, so it declares its own message",
            },
            Case {
                scenario: "a variant fixes each label once",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", metric = counter, describe = "Number of demo events.", labels(outcome: Outcome))] enum Demo { #[event(labels(outcome = Ok, outcome = Error), log = info, message = "demo")] Ok {} }"#,
                expected: "`outcome` is fixed twice",
            },
            Case {
                scenario: "a struct Event's #[label] fields are its schema",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", metric = counter, describe = "Number of demo events.", log = off, labels(outcome: Outcome))] struct Demo {}"#,
                expected: "declares an enum Event's label schema",
            },
            Case {
                scenario: "a schema key follows the metric label grammar",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", metric = counter, describe = "Number of demo events.", labels(café: Outcome))] enum Demo { #[event(labels(café = Ok), log = info, message = "demo")] Ok {} }"#,
                expected: "metric label names start with an ASCII letter",
            },
            Case {
                scenario: "a schema key does not collide with the log metadata",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", metric = counter, describe = "Number of demo events.", labels(message: Outcome))] enum Demo { #[event(labels(message = Ok), log = info, message = "demo")] Ok {} }"#,
                expected: "message",
            },
            Case {
                // The enum is silent, so only the variant's own level makes
                // this key reach a log line.
                scenario: "a reserved key is caught at the level the variant raises to",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", metric = counter, describe = "Number of demo events.", log = off, labels(level: Outcome))] enum Demo { #[event(labels(level = Ok), log = warn, message = "demo")] Ok {} }"#,
                expected: "level",
            },
            Case {
                scenario: "a variant fixes only keys the schema declares",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", metric = counter, describe = "Number of demo events.", labels(outcome: Outcome))] enum Demo { #[event(labels(outcome = Ok, stage = Apply), log = info, message = "demo")] Ok {} }"#,
                expected: "`stage` is not in this Event's label schema",
            },
            Case {
                scenario: "two fields do not resolve to one metric label",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", metric = counter, describe = "Number of demo events.", labels(outcome: Outcome))] enum Demo { #[event(log = info, message = "demo")] Ok { #[label] outcome: Outcome, #[label(name = "outcome")] other: Outcome } }"#,
                expected: "duplicate metric label name `outcome`",
            },
            Case {
                scenario: "the family owns the metric-side keys",
                source: r#"#[event(event_name = "demo", metric_family = Shared, metric_name = "carbide_demo_total")] enum Demo { #[event(labels(outcome = Ok), log = info, message = "demo")] Ok {} }"#,
                expected: "`metric_name` is declared by the `Shared` metric family",
            },
            Case {
                scenario: "a counter variant does not record a value",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", metric = counter, describe = "Number of demo events.", labels(outcome: Outcome))] enum Demo { #[event(labels(outcome = Ok), log = info, message = "demo")] Ok { #[observation] elapsed_seconds: f64 } }"#,
                expected: "records a counter",
            },
            Case {
                scenario: "a histogram variant records exactly one value",
                source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_seconds", component = "demo", metric = histogram, describe = "Time a demo takes.", labels(outcome: Outcome))] enum Demo { #[event(labels(outcome = Ok), log = info, message = "demo")] Ok {} }"#,
                expected: "records a histogram, so it needs exactly one",
            },
        ] {
            let error = expansion_error(source);
            assert!(
                error.contains(expected),
                "{scenario}: expected `{expected}` in `{error}`"
            );
        }
    }

    #[test]
    fn reserved_fields_apply_only_to_the_log_surface() {
        for field_name in carbide_observability_schema::EVENT_LOG_RESERVED_FIELDS {
            let ident = Ident::new(field_name, Span::call_site());
            if *field_name == "message" {
                for kind in [
                    FieldKind::Label,
                    FieldKind::Context(ContextMode::Display),
                    FieldKind::Observation,
                ] {
                    assert!(validate_event_log_field(LogSpec::Info, kind, &ident).is_err());
                    assert!(validate_event_log_field(LogSpec::Off, kind, &ident).is_err());
                }
                continue;
            }
            for kind in [FieldKind::Label, FieldKind::Context(ContextMode::Display)] {
                assert!(validate_event_log_field(LogSpec::Info, kind, &ident).is_err());
                assert!(validate_event_log_field(LogSpec::Dynamic, kind, &ident).is_err());
                assert!(validate_event_log_field(LogSpec::Off, kind, &ident).is_ok());
            }
            assert!(
                validate_event_log_field(LogSpec::Info, FieldKind::Observation, &ident).is_ok()
            );
        }

        let machine_id = Ident::new("machine_id", Span::call_site());
        assert!(
            validate_event_log_field(
                LogSpec::Info,
                FieldKind::Context(ContextMode::Display),
                &machine_id,
            )
            .is_ok()
        );
    }

    #[test]
    fn context_value_mode_is_explicit_and_validated() {
        #[derive(Debug, PartialEq)]
        enum ParsedContextMode {
            Display,
            Value,
        }

        check_cases(
            [
                TestCase {
                    scenario: "bare context uses Display formatting",
                    input: "#[context]",
                    expect: Yields(ParsedContextMode::Display),
                },
                TestCase {
                    scenario: "value context preserves its tracing type",
                    input: "#[context(value)]",
                    expect: Yields(ParsedContextMode::Value),
                },
                TestCase {
                    scenario: "empty context arguments are rejected",
                    input: "#[context()]",
                    expect: Fails,
                },
                TestCase {
                    scenario: "multiple context arguments are rejected",
                    input: "#[context(value, display)]",
                    expect: Fails,
                },
                TestCase {
                    scenario: "name-value context syntax is rejected",
                    input: "#[context = \"value\"]",
                    expect: FailsWith(
                        "context is an attribute: use #[context] or #[context(value)]".to_string(),
                    ),
                },
                TestCase {
                    scenario: "unknown context modes are rejected",
                    input: "#[context(native)]",
                    expect: FailsWith(
                        "unknown context mode; use #[context] for Display formatting or \
                         #[context(value)] to preserve a native tracing value"
                            .to_string(),
                    ),
                },
            ],
            |attribute| {
                let source = format!(
                    r#"
                    #[event(event_name = "demo", component = "demo", message = "demo")]
                    struct Demo {{
                        {attribute}
                        elapsed_seconds: f64,
                    }}
                    "#,
                );
                let input: DeriveInput = syn::parse_str(&source).expect("valid derive input");
                let Data::Struct(data) = input.data else {
                    panic!("fixture is a struct");
                };
                let Fields::Named(fields) = data.fields else {
                    panic!("fixture has a named field");
                };

                match classify_field(&fields.named[0]) {
                    Ok(FieldKind::Context(ContextMode::Display)) => Ok(ParsedContextMode::Display),
                    Ok(FieldKind::Context(ContextMode::Value)) => Ok(ParsedContextMode::Value),
                    Ok(_) => panic!("fixture field is context"),
                    Err(error) => Err(error.to_string()),
                }
            },
        );
    }

    #[test]
    fn label_metric_name_alias_diagnostics_are_specific() {
        struct DiagnosticInput {
            source: &'static str,
            expected: &'static str,
        }

        check_values(
            [
                Check {
                    scenario: "unknown alias key",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", log = info, metric = counter, message = "demo", describe = "Number of demo events")] struct Demo { #[label(rename = "component")] publisher: Stage }"#,
                        expected: "unknown `label` key; expected `name`",
                    },
                    expect: None,
                },
                Check {
                    scenario: "duplicate alias key",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", log = info, metric = counter, message = "demo", describe = "Number of demo events")] struct Demo { #[label(name = "component", name = "source")] publisher: Stage }"#,
                        expected: "duplicate label `name`",
                    },
                    expect: None,
                },
                Check {
                    scenario: "missing alias name",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", log = info, metric = counter, message = "demo", describe = "Number of demo events")] struct Demo { #[label()] publisher: Stage }"#,
                        expected: "requires name = \"...\"",
                    },
                    expect: None,
                },
                Check {
                    scenario: "invalid metric label name",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", log = info, metric = counter, message = "demo", describe = "Number of demo events")] struct Demo { #[label(name = "bad-label")] publisher: Stage }"#,
                        expected: "metric label names start with an ASCII letter",
                    },
                    expect: None,
                },
                Check {
                    scenario: "non-ASCII bare label name",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", log = info, metric = counter, message = "demo", describe = "Number of demo events")] struct Demo { #[label] café: Stage }"#,
                        expected: "metric label names start with an ASCII letter",
                    },
                    expect: None,
                },
                Check {
                    scenario: "name-value attribute syntax",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", log = info, metric = counter, message = "demo", describe = "Number of demo events")] struct Demo { #[label = "component"] publisher: Stage }"#,
                        expected: "use #[label(name = \"...\")]",
                    },
                    expect: None,
                },
                Check {
                    scenario: "duplicate resolved metric label name",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "demo", metric_name = "carbide_demo_total", component = "demo", log = info, metric = counter, message = "demo", describe = "Number of demo events")] struct Demo { #[label(name = "component")] publisher: Stage, #[label(name = "component")] source: Stage }"#,
                        expected: "duplicate metric label name `component`",
                    },
                    expect: None,
                },
            ],
            |DiagnosticInput { source, expected }| {
                let error = expansion_error(source);
                (!error.contains(expected)).then(|| format!("expected `{expected}` in `{error}`"))
            },
        );
    }

    #[test]
    fn observation_attributes_reject_arguments() {
        struct DiagnosticInput {
            source: &'static str,
            expected: &'static str,
        }

        check_values(
            [
                Check {
                    scenario: "observation list arguments",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "demo", component = "demo", message = "demo")] struct Demo { #[observation(name = "value")] value: f64 }"#,
                        expected: "#[observation] does not accept arguments; use bare #[observation]",
                    },
                    expect: None,
                },
                Check {
                    scenario: "observation name-value syntax",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "demo", component = "demo", message = "demo")] struct Demo { #[observation = "value"] value: f64 }"#,
                        expected: "#[observation] does not accept arguments; use bare #[observation]",
                    },
                    expect: None,
                },
            ],
            |DiagnosticInput { source, expected }| {
                let error = expansion_error(source);
                (!error.contains(expected)).then(|| format!("expected `{expected}` in `{error}`"))
            },
        );
    }

    #[test]
    fn metric_family_diagnostics_are_specific() {
        fn family_error(source: &str) -> String {
            let input: DeriveInput = syn::parse_str(source).expect("valid derive input");
            super::expand_metric_family(input)
                .expect_err("input should be rejected")
                .to_string()
        }

        struct DiagnosticInput {
            source: &'static str,
            expected: &'static str,
        }

        check_values(
            [
                Check {
                    scenario: "a family declares its exposed name",
                    input: DiagnosticInput {
                        source: r#"#[metric(kind = counter, component = "c", describe = "Number of things")] struct F {}"#,
                        expected: "requires name = \"...\"",
                    },
                    expect: None,
                },
                Check {
                    scenario: "a family declares its instrument kind",
                    input: DiagnosticInput {
                        source: r#"#[metric(name = "carbide_f_total", component = "c", describe = "Number of things")] struct F {}"#,
                        expected: "requires kind = counter, kind = histogram, or kind = gauge",
                    },
                    expect: None,
                },
                Check {
                    scenario: "a family owns the component",
                    input: DiagnosticInput {
                        source: r#"#[metric(name = "carbide_f_total", kind = counter, describe = "Number of things")] struct F {}"#,
                        expected: "requires component = \"...\"",
                    },
                    expect: None,
                },
                Check {
                    scenario: "the Event-side key name is explained",
                    input: DiagnosticInput {
                        source: r#"#[metric(metric_name = "carbide_f_total", kind = counter, component = "c")] struct F {}"#,
                        expected: "declares its exposed name as `name`",
                    },
                    expect: None,
                },
                Check {
                    scenario: "a family has no metric = none",
                    input: DiagnosticInput {
                        source: r#"#[metric(name = "carbide_f_total", kind = none, component = "c")] struct F {}"#,
                        expected: "a family is counter | histogram",
                    },
                    expect: None,
                },
                Check {
                    scenario: "the shared name validation applies to a family",
                    input: DiagnosticInput {
                        source: r#"#[metric(name = "f_total", kind = counter, component = "c", describe = "Number of things")] struct F {}"#,
                        expected: "metric names use the `carbide_` prefix",
                    },
                    expect: None,
                },
                Check {
                    scenario: "the shared describe rule applies to a family",
                    input: DiagnosticInput {
                        source: r#"#[metric(name = "carbide_f_total", kind = counter, component = "c", describe = "Total number of things")] struct F {}"#,
                        expected: "opens with \"Number of ...\"",
                    },
                    expect: None,
                },
                Check {
                    scenario: "a counter takes no unit, whatever its name",
                    input: DiagnosticInput {
                        source: r#"#[metric(name = "carbide_f_total", kind = counter, component = "c", describe = "Number of things", unit = "ms")] struct F {}"#,
                        expected: "`unit` is only valid for histogram and gauge metrics",
                    },
                    expect: None,
                },
                Check {
                    scenario: "a conventionally named histogram declares its unit as the suffix",
                    input: DiagnosticInput {
                        source: r#"#[metric(name = "carbide_f_seconds", kind = histogram, component = "c", unit = "s")] struct F {}"#,
                        expected: "only for metric_name_unchecked histograms",
                    },
                    expect: None,
                },
                Check {
                    scenario: "every family field is already a label",
                    input: DiagnosticInput {
                        source: r#"#[metric(name = "carbide_f_total", kind = counter, component = "c", describe = "Number of things")] struct F { #[context] detail: String }"#,
                        expected: "a MetricFamily field is a label already",
                    },
                    expect: None,
                },
                Check {
                    scenario: "family label keys are unique",
                    input: DiagnosticInput {
                        source: r#"#[metric(name = "carbide_f_total", kind = counter, component = "c", describe = "Number of things")] struct F { #[label(name = "stage")] a: Stage, #[label(name = "stage")] b: Stage }"#,
                        expected: "duplicate metric label name `stage`",
                    },
                    expect: None,
                },
            ],
            |DiagnosticInput { source, expected }| {
                let error = family_error(source);
                (!error.contains(expected)).then(|| format!("expected `{expected}` in `{error}`"))
            },
        );
    }

    /// An Event that names a family states only its own side; every
    /// metric-side key and escape hatch belongs on the family.
    #[test]
    fn family_backed_events_reject_the_metric_side_keys() {
        struct DiagnosticInput {
            source: &'static str,
            expected: &'static str,
        }

        check_values(
            [
                Check {
                    scenario: "restated metric name",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "e", metric_family = Fam, log = warn, message = "m", metric_name = "carbide_fam_total")] struct E {}"#,
                        expected: "`metric_name` is declared by the `Fam` metric family",
                    },
                    expect: None,
                },
                Check {
                    scenario: "restated describe",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "e", metric_family = Fam, log = warn, message = "m", describe = "Number of things")] struct E {}"#,
                        expected: "`describe` is declared by the `Fam` metric family",
                    },
                    expect: None,
                },
                Check {
                    scenario: "restated unit",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "e", metric_family = Fam, log = warn, message = "m", unit = "ms")] struct E {}"#,
                        expected: "`unit` is declared by the `Fam` metric family",
                    },
                    expect: None,
                },
                Check {
                    scenario: "restated component",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "e", metric_family = Fam, log = warn, message = "m", component = "demo")] struct E {}"#,
                        expected: "`component` is declared by the `Fam` metric family",
                    },
                    expect: None,
                },
                Check {
                    scenario: "an inline kind alongside a family",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "e", metric_family = Fam, metric = counter, log = warn, message = "m")] struct E {}"#,
                        expected: "`metric` is declared by the `Fam` metric family",
                    },
                    expect: None,
                },
                Check {
                    scenario: "the name escape hatch belongs on the family",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "e", metric_family = Fam, log = warn, message = "m", metric_name_unchecked)] struct E {}"#,
                        expected: "escape hatches belong on the metric family's #[metric(...)]",
                    },
                    expect: None,
                },
                Check {
                    scenario: "the describe escape hatch belongs on the family",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "e", metric_family = Fam, log = warn, message = "m", describe_unchecked)] struct E {}"#,
                        expected: "escape hatches belong on the metric family's #[metric(...)]",
                    },
                    expect: None,
                },
                Check {
                    scenario: "a label alias belongs on the family's field",
                    input: DiagnosticInput {
                        source: r#"#[event(event_name = "e", metric_family = Fam, log = warn, message = "m")] struct E { #[label(name = "phase")] stage: Stage }"#,
                        expected: "declare the alias on the family's field instead",
                    },
                    expect: None,
                },
            ],
            |DiagnosticInput { source, expected }| {
                let error = expansion_error(source);
                (!error.contains(expected)).then(|| format!("expected `{expected}` in `{error}`"))
            },
        );
    }

    /// The native-value mode has its own types and no optional form, so it says
    /// that rather than letting the field reach a trait error somewhere else.
    #[test]
    fn context_value_rejects_an_optional_field() {
        let error = expansion_error(
            r#"#[event(event_name = "demo", component = "demo", message = "demo")] struct Demo { #[context(value)] value: Option<String> }"#,
        );
        assert!(
            error.contains("#[context(value)] does not take an Option"),
            "expected the targeted rejection, got `{error}`"
        );
        assert!(
            error.contains("use #[context]"),
            "the rejection points at the mode that does take one, got `{error}`"
        );
    }

    /// `Option<T>` is recognized only in the spellings the docs promise, so a
    /// type of somebody else's that happens to be named `Option` keeps rendering
    /// through `Display` instead of being treated as an absent-able field.
    #[test]
    fn only_the_std_option_counts_as_optional() {
        struct SpellingCase {
            ty: &'static str,
            optional: bool,
        }

        check_values(
            [
                Check {
                    scenario: "the bare std option",
                    input: SpellingCase {
                        ty: "Option<String>",
                        optional: true,
                    },
                    expect: None,
                },
                Check {
                    scenario: "spelled out through std",
                    input: SpellingCase {
                        ty: "std::option::Option<String>",
                        optional: true,
                    },
                    expect: None,
                },
                Check {
                    scenario: "spelled out through core",
                    input: SpellingCase {
                        ty: "core::option::Option<String>",
                        optional: true,
                    },
                    expect: None,
                },
                Check {
                    scenario: "leading-colon std path",
                    input: SpellingCase {
                        ty: "::std::option::Option<String>",
                        optional: true,
                    },
                    expect: None,
                },
                Check {
                    scenario: "somebody else's option module",
                    input: SpellingCase {
                        ty: "option::Option<String>",
                        optional: false,
                    },
                    expect: None,
                },
                Check {
                    scenario: "somebody else's Option type",
                    input: SpellingCase {
                        ty: "mine::Option<String>",
                        optional: false,
                    },
                    expect: None,
                },
                Check {
                    scenario: "a non-generic type named Option",
                    input: SpellingCase {
                        ty: "Option",
                        optional: false,
                    },
                    expect: None,
                },
                Check {
                    scenario: "an option taking more than one argument is not one",
                    input: SpellingCase {
                        ty: "Option<String, u8>",
                        optional: false,
                    },
                    expect: None,
                },
            ],
            |SpellingCase { ty, optional }| {
                let parsed: syn::Type = syn::parse_str(ty).expect("valid type");
                (super::is_option(&parsed) != optional)
                    .then(|| format!("`{ty}`: expected optional={optional}"))
            },
        );
    }

    #[test]
    fn message_rejects_an_unknown_bare_word() {
        let error = expansion_error(
            r#"#[event(event_name = "demo", component = "demo", message = bogus)] struct Demo {}"#,
        );
        assert!(
            error.contains("string literal or `dynamic`"),
            "expected a message-value diagnostic, got `{error}`"
        );
    }
}
