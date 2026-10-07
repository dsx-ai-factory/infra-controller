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
use rustc_errors::DiagDecorator;
use rustc_lint_defs::{Lint, LintPass, LintVec};
use rustc_middle::mir::{self, LocalKind};
use rustc_middle::ty::{Ty as MiddleTy, TyCtxt};
use rustc_mir_dataflow::move_paths::MoveData;
use rustc_span::Symbol;
use rustc_span::def_id::LocalDefId;

///  ### What it does
///
///  Finds cases where a `sqlx::Transaction` owned by a function is always dropped by that function
///  without ever being committed or moved elsewhere.
///
///  ### Why is this bad?
///
///  If a function owns a transaction it is responsible for either committing it, rolling it back,
/// or moving it along to some other owner. Dropping a transaction in the same function leaves the
/// transaction uncommitted.
///
///  ### Example
///
///  ```ignore
///  async fn bad(pool: PgPool) {
///      let txn = pool.begin().await.unwrap();
///      do_work().await;
///  } // txn is dropped here without commit()
///  ```
///
/// Any move of the transaction will avoid the lint, whether it's calling .commit(), .rollback(), or
/// moving it to some other function.
pub(crate) static TXN_WITHOUT_COMMIT: &Lint = &Lint {
    name: "txn_without_commit",
    default_level: ::rustc_lint_defs::Warn,
    desc: "A sqlx::Transaction is owned by this function but dropped without calling commit() or moving it out",
    is_externally_loaded: false,
    ..Lint::default_fields_for_macro()
};

pub(crate) struct TxnWithoutCommit {
    txn_symbol_paths: Vec<Vec<Symbol>>,
}

impl Default for TxnWithoutCommit {
    fn default() -> Self {
        let txn_symbol_paths = vec![
            // Note: Unlike txn_held_across_await, we don't include PgConnection or similar types here.
            "sqlx_core::transaction::Transaction",
            "sqlx_postgres::PgTransaction",
            "db::Transaction",
        ]
        .into_iter()
        .map(|txn| {
            txn.split("::")
                .filter_map(|s| {
                    if s.is_empty() {
                        None
                    } else {
                        Some(Symbol::intern(s))
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect();

        Self { txn_symbol_paths }
    }
}

impl LintPass for TxnWithoutCommit {
    fn name(&self) -> &'static str {
        "TxnWithoutCommit"
    }
    fn get_lints(&self) -> LintVec {
        [TXN_WITHOUT_COMMIT].to_vec()
    }
}

impl TxnWithoutCommit {
    /// Entrypoint for the lint. Runs on the MIR body for the given def_id and emits when a
    /// Transaction local is definitely still initialized at the end of the function *and* was never
    /// moved out along any path.
    pub(crate) fn check_fn_body(&mut self, tcx: TyCtxt, def_id: LocalDefId) {
        // Only check nodes that actually have a body.
        let Some((_owner_def_id, body_id)) = tcx.hir_node_by_def_id(def_id).associated_body()
        else {
            return;
        };

        let body_borrow = tcx.mir_promoted(def_id).0.borrow();
        let body = &*body_borrow;

        // We only care about locals/args, not temporaries or the return place.
        let candidate_locals = body
            .local_decls
            .iter_enumerated()
            .filter(|(local, decl)| {
                !matches!(body.local_kind(*local), LocalKind::ReturnPointer)
                    && self.is_owned_txn_ty(tcx, &decl.ty)
            })
            .collect::<Vec<_>>();

        if candidate_locals.is_empty() {
            return;
        }

        // Gather move data so we can check whether a txn local is ever moved out (commit(), return, etc)
        let move_data = MoveData::gather_moves(body, tcx, |_| true);

        for (local, decl) in candidate_locals {
            let has_move_out = Self::local_has_move_out(local, body, &move_data);

            // If there exists any path where this txn local was moved (commit(), returned, passed
            // by value), we don't lint: the txn is not always dropped here.
            if has_move_out {
                continue;
            }

            tcx.emit_node_span_lint(
                TXN_WITHOUT_COMMIT,
                body_id.hir_id,
                decl.source_info.span,
                DiagDecorator(|diag| {
                    diag.primary_message(
                        "sqlx::Transaction is dropped by this function without commit()/rollback(), or being moved out",
                    );
                }),
            );
        }
    }

    fn is_owned_txn_ty(&self, tcx: TyCtxt<'_>, ty: &MiddleTy) -> bool {
        if ty.is_ref() {
            return false;
        }

        self.is_sqlx_transaction_ty(tcx, ty)
    }

    /// Check if the given rustc_middle::Ty (ie. output from typeck) resolves to a sqlx::Transaction
    fn is_sqlx_transaction_ty(&self, tcx: TyCtxt<'_>, ty: &MiddleTy) -> bool {
        let def_id = match ty.peel_refs().kind() {
            rustc_middle::ty::Adt(adt, _) => adt.did(),
            _ => {
                return false;
            }
        };

        let path = tcx.def_path(def_id);
        let path_as_syms = path
            .data
            .iter()
            .map(|i| i.as_sym(false))
            .collect::<Vec<_>>();
        self.txn_symbol_paths
            .iter()
            // TODO: ignoring crates via skip(1), we shouldn't do that
            .any(|txn_path| txn_path.iter().skip(1).eq(path_as_syms.iter()))
    }

    fn local_has_move_out(
        local: mir::Local,
        body: &mir::Body<'_>,
        move_data: &MoveData<'_>,
    ) -> bool {
        move_data.moves.iter().any(|mov| {
            let mut current_idx = mov.path;
            loop {
                let move_path = &move_data.move_paths[current_idx];
                if move_path.place.local == local {
                    let loc = mov.source;
                    if loc.statement_index == body.basic_blocks[loc.block].statements.len() {
                        if let mir::TerminatorKind::Drop { .. } =
                            body.basic_blocks[loc.block].terminator().kind
                        {
                            return false;
                        }
                    } else if let mir::StatementKind::StorageDead(..) =
                        body.basic_blocks[loc.block].statements[loc.statement_index].kind
                    {
                        return false;
                    }
                    return true;
                }
                if let Some(parent) = move_path.parent {
                    current_idx = parent;
                } else {
                    return false;
                }
            }
        })
    }
}
