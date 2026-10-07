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

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{DeriveInput, LitStr, Meta, Token};

mod db_table;

type AttributeArgs = syn::punctuated::Punctuated<syn::Meta, syn::Token![,]>;

/// `DbTable` derives `carbide_uuid::DbTable` from a struct's field names.
///
/// Set the table with exactly one `#[db_table(name = "table_name")]`
/// attribute. The struct must have at least one named field; columns follow
/// declaration order without inspecting field types or unrelated attributes.
/// `db_table_columns()` returns `carbide_uuid::DbColumns` for direct SQL
/// formatting, with the original names available through `as_slice()`.
/// Both table and field names must match `[a-z_][a-z0-9_]*`. Names are returned
/// unquoted, so callers must avoid reserved SQL keywords. Raw Rust identifiers, qualified
/// table names, and `#[sqlx(...)]` attributes on the struct or its fields are
/// unsupported; implement `DbTable` manually when the decoder needs mappings.
/// The consuming crate must depend on `carbide-uuid` as `carbide_uuid`.
#[proc_macro_derive(DbTable, attributes(db_table))]
pub fn derive_db_table(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    match db_table::expand(input) {
        Ok(output) => output.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// derive_dispatch is a derive macro that generates a `Dispatch` impl
/// for a CLI command enum. Each variant can either be a tuple variant
/// with a single field whose type implements `Run`, OR be a variant
/// annotated with `#[dispatch]`, which is then treated as nested command
/// group whose inner type implements `Dispatch` itself (with more `Run`
/// and/or #[dispatch] variants).
///
/// # Some examples, if you please.
///
/// A command where variants implement `Run`:
/// ```ignore
/// #[derive(Parser, Debug, Dispatch)]
/// pub enum Cmd {
///     Show(show::Args),
///     List(list::Args),
/// }
/// ```
///
/// A command with both `Run` and nested `Dispatch` variants:
/// ```ignore
/// #[derive(Parser, Debug, Dispatch)]
/// pub enum Cmd {
///     Show(show::Args),
///     #[dispatch]
///     SubGroup(sub::Cmd),
/// }
/// ```
#[proc_macro_derive(Dispatch, attributes(dispatch))]
pub fn derive_dispatch(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    match expand_dispatch(input) {
        Ok(ts) => ts,
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_dispatch(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;

    let data = match &input.data {
        syn::Data::Enum(data) => data,
        _ => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "Dispatch can only be derived for enums",
            ));
        }
    };

    let mut run_arms = Vec::new();
    let mut dispatch_arms = Vec::new();

    for variant in &data.variants {
        let variant_name = &variant.ident;
        let is_dispatch = variant.attrs.iter().any(|a| a.path().is_ident("dispatch"));

        if is_dispatch {
            dispatch_arms.push(quote! {
                #name::#variant_name(cmd) => cmd.dispatch(ctx).await,
            });
        } else {
            run_arms.push(quote! {
                #name::#variant_name(args) => args.run(&mut ctx).await,
            });
        }
    }

    let dispatch_import = if dispatch_arms.is_empty() {
        quote! {}
    } else {
        quote! { use crate::cfg::dispatch::Dispatch as _; }
    };

    let output = quote! {
        impl crate::cfg::dispatch::Dispatch for #name {
            async fn dispatch(
                self,
                mut ctx: crate::cfg::runtime::RuntimeContext,
            ) -> crate::errors::CarbideCliResult<()> {
                use crate::cfg::run::Run;
                #dispatch_import
                match self {
                    #(#run_arms)*
                    #(#dispatch_arms)*
                }
            }
        }
    };

    Ok(output.into())
}

/// Use this instead of `#[sqlx::test]`. This is because `#[sqlx::test]` inlines everything on every
/// usage, including:
///
/// - The entire migrations directory, inlined as a huge string constant
/// - Every fixture file you specify, as individual string constants
///
/// This ends up blowing up the test executable size tremendously, and causes link times to be very
/// long, even on incremental builds.
///
/// This wrapper delegates database setup to `sqlx_testing::TestFn::run_test`, which creates test
/// databases from a migrated template database. That avoids re-running migrations for every test.
///
/// # Specifying fixtures
///
/// - Fixtures are specified with `#[carbide_macros::sqlx_test(fixtures("fixture1", ...))]`.
/// - For each fixture name, the macro emits a call to `crate::tests::sqlx_fixture_from_str`.
/// - Crates using fixture arguments must therefore define or re-export
///   `crate::tests::sqlx_fixture_from_str`.
///
/// Tests that do not specify fixtures do not need any `crate::tests` helper items.
///
/// This does not support other options from `sqlx::test`, e.g. `path`, `scripts(...)`, etc.
///
/// # Creating new fixtures
///
/// Add fixtures to api/src/tests/fixtures, and edit `crate::tests::sqlx_fixture_from_str` and add
/// the name of your fixture.
///
/// # How does it work?
///
/// The macro wraps the async test body in a synchronous `#[test]`, builds a
/// `::sqlx::testing::TestArgs`, attaches any requested fixtures, and then delegates to
/// `sqlx_testing::TestFn::run_test`.
///
/// With fixtures, it expands roughly to:
///
/// ```ignore
/// // before:
/// #[carbide_macros::sqlx_test(fixtures("my_fixture"))]
/// async fn the_test(pool: sqlx::PgPool) { /* the test */ }
///
/// // after:
/// #[test]
/// fn the_test() {
///     async fn the_test(pool: sqlx::PgPool) { /* test is "pasted" here */ }
///     let mut args = ::sqlx::testing::TestArgs::new("carbide::tests::the_test");
///     args.fixtures(
///         Box::leak(Box::new(vec![
///             crate::tests::sqlx_fixture_from_str("my_fixture"),
///         ])),
///     );
///     let f: fn(_) -> _ = the_test;
///     sqlx_testing::TestFn::run_test(f, args)
/// }
/// ```
#[proc_macro_attribute]
pub fn sqlx_test(args: TokenStream, input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::ItemFn);
    match expand(args, input) {
        Ok(ts) => ts,
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(args: TokenStream, input: syn::ItemFn) -> Result<TokenStream, syn::Error> {
    let ret = &input.sig.output;
    let name = &input.sig.ident;
    let inputs = &input.sig.inputs;
    let body = &input.block;
    let attrs = &input.attrs;

    let parser = AttributeArgs::parse_terminated;
    let args = parser.parse2(args.into())?;

    // Fixtures need to be types with exported paths (e.g. tests::fixtures::SomeFixture)
    let fixtures = args
        .into_iter()
        .filter_map(|arg| match arg {
            Meta::List(list) => {
                if list.path.is_ident("fixtures") {
                    let args = list
                        .parse_args_with(<Punctuated<LitStr, Token![,]>>::parse_terminated)
                        .ok()?;
                    Some(args)
                } else {
                    None
                }
            }
            _ => None,
        })
        .flat_map(|str_lits| {
            str_lits
                .iter()
                .map(|str_lit| quote! { crate::tests::sqlx_fixture_from_str(#str_lit) })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let fn_arg_types = inputs.iter().map(|_| quote! { _ });

    let pm2_token_stream = quote! {
        #(#attrs)*
        #[::core::prelude::v1::test]
        fn #name() #ret {
            async fn #name(#inputs) #ret {
                #body
            }

            let mut args = ::sqlx::testing::TestArgs::new(concat!(module_path!(), "::", stringify!(#name)));

            // Note: we use Box::leak because args.fixtures expects a &'static slice, which is
            // normally only possible if you define the fixtures inline. Since each TestFixture is a
            // struct with two `&'static str`s inside it, this should only leak 16 bytes per unit
            // test, which is fine. (We're not leaking the entire fixtures, just pointers to them.)
            args.fixtures(Box::leak(Box::new(vec![#(#fixtures),*])));

            // We need to give a coercion site or else we get "unimplemented trait" errors.
            let f: fn(#(#fn_arg_types),*) -> _ = #name;

            sqlx_testing::TestFn::run_test(f, args)
        }
    };
    Ok(pm2_token_stream.into())
}
