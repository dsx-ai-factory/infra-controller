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

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Attribute, Data, DeriveInput, Error, Fields, LitStr};

pub(super) fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(
            &input.ident,
            "DbTable requires a struct with named fields",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(Error::new_spanned(
            &input.ident,
            "DbTable requires a struct with named fields",
        ));
    };
    if fields.named.is_empty() {
        return Err(Error::new_spanned(
            fields,
            "DbTable requires at least one column",
        ));
    }

    reject_sqlx_attributes(&input.attrs)?;
    let table_name = parse_table_name(&input)?;
    let columns = fields
        .named
        .iter()
        .map(|field| {
            reject_sqlx_attributes(&field.attrs)?;
            if let Some(attribute) = field.attrs.iter().find(|a| a.path().is_ident("db_table")) {
                return Err(Error::new_spanned(
                    attribute,
                    "#[db_table(...)] belongs on the struct, not its fields",
                ));
            }
            let column = field.ident.as_ref().ok_or_else(|| {
                Error::new_spanned(field, "DbTable requires a struct with named fields")
            })?;
            let column_name = column.to_string();
            validate_identifier(&column_name, column.span())?;
            Ok(LitStr::new(&column_name, column.span()))
        })
        .collect::<syn::Result<Vec<_>>>()?;

    let name = &input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics ::carbide_uuid::DbTable for #name #type_generics #where_clause {
            fn db_table_name() -> &'static str {
                #table_name
            }

            fn db_table_columns() -> ::carbide_uuid::DbColumns {
                ::carbide_uuid::DbColumns::new(&[#(#columns),*])
            }
        }
    })
}

fn parse_table_name(input: &DeriveInput) -> syn::Result<LitStr> {
    let mut attributes = input.attrs.iter().filter(|a| a.path().is_ident("db_table"));
    let attribute = attributes.next().ok_or_else(|| {
        Error::new_spanned(&input.ident, "missing #[db_table(name = \"table_name\")]")
    })?;
    if let Some(duplicate) = attributes.next() {
        return Err(Error::new_spanned(
            duplicate,
            "duplicate #[db_table(...)] attribute",
        ));
    }

    let mut name = None;
    attribute.parse_nested_meta(|meta| {
        if !meta.path.is_ident("name") {
            return Err(meta.error("unsupported DbTable option; expected name = \"table_name\""));
        }
        if name.is_some() {
            return Err(meta.error("duplicate DbTable name"));
        }
        let value: LitStr = meta.value()?.parse()?;
        validate_identifier(&value.value(), value.span())?;
        name = Some(value);
        Ok(())
    })?;
    name.ok_or_else(|| Error::new_spanned(attribute, "missing DbTable name = \"table_name\""))
}

fn validate_identifier(name: &str, span: Span) -> syn::Result<()> {
    let mut bytes = name.bytes();
    if !matches!(bytes.next(), Some(b'a'..=b'z' | b'_'))
        || !bytes.all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
    {
        return Err(Error::new(
            span,
            "DbTable names must match [a-z_][a-z0-9_]*; implement DbTable manually for other names",
        ));
    }
    Ok(())
}

fn reject_sqlx_attributes(attributes: &[Attribute]) -> syn::Result<()> {
    if let Some(attribute) = attributes.iter().find(|a| a.path().is_ident("sqlx")) {
        return Err(Error::new_spanned(
            attribute,
            "DbTable does not support #[sqlx(...)] mappings; implement DbTable manually",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::Fails;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn columns_follow_declaration_order_without_serde_mappings() {
        let input = syn::parse_quote! {
            #[db_table(name = "records")]
            #[serde(rename_all = "camelCase")]
            struct Record {
                #[serde(rename = "wire_value")]
                z_value: Option<String>,
                #[doc = "An unrelated field attribute."]
                first: u64,
            }
        };
        let expected = quote! {
            impl ::carbide_uuid::DbTable for Record {
                fn db_table_name() -> &'static str {
                    "records"
                }

                fn db_table_columns() -> ::carbide_uuid::DbColumns {
                    ::carbide_uuid::DbColumns::new(&["z_value", "first"])
                }
            }
        };

        assert_eq!(
            expand(input).expect("derive column metadata").to_string(),
            expected.to_string(),
        );
    }

    #[test]
    fn rejects_incomplete_or_unsupported_column_contracts() {
        scenarios!(run = |source| {
            expand(syn::parse_str(source).expect("parse derive input"))
                .map(|_| ())
                .map_err(drop)
        };
            "explicit table name required" {
                "struct Record { id: u64 }" => Fails,
                "#[db_table(name = \"\")] struct Record { id: u64 }" => Fails,
            }
            "duplicate configuration" {
                "#[db_table(name = \"records\", name = \"other\")] struct Record { id: u64 }" => Fails,
                "#[db_table(name = \"records\")] #[db_table()] struct Record { id: u64 }" => Fails,
            }
            "unknown and malformed configuration" {
                "#[db_table(table = \"records\")] struct Record { id: u64 }" => Fails,
                "#[db_table(name = 1)] struct Record { id: u64 }" => Fails,
            }
            "named columns required" {
                "#[db_table(name = \"records\")] enum Record { Value }" => Fails,
                "#[db_table(name = \"records\")] struct Record(u64);" => Fails,
                "#[db_table(name = \"records\")] struct Record {}" => Fails,
            }
            "SQL syntax and raw identifiers unsupported" {
                "#[db_table(name = \"records; SELECT 1\")] struct Record { id: u64 }" => Fails,
                "#[db_table(name = \"records\")] struct Record { r#type: u64 }" => Fails,
            }
            "field configuration unsupported" {
                "#[db_table(name = \"records\")] struct Record { #[db_table(name = \"other\")] id: u64 }" => Fails,
            }
            "SQLx mappings require a manual implementation" {
                "#[db_table(name = \"records\")] #[sqlx(rename_all = \"camelCase\")] struct Record { id: u64 }" => Fails,
                "#[db_table(name = \"records\")] struct Record { #[sqlx(flatten)] nested: Other }" => Fails,
            }
        );
    }
}
