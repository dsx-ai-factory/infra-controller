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

use carbide_uuid::DbTable;

#[derive(carbide_macros::DbTable)]
#[db_table(name = "generic_records")]
struct Record<'a, T = u32, const N: usize = 2>
where
    T: Copy,
{
    z_value: &'a T,
    #[cfg(any())]
    omitted: (),
    samples: [u8; N],
}

#[test]
fn generics_and_active_fields_reach_the_generated_impl() {
    let record = Record {
        z_value: &7u32,
        samples: [1, 2],
    };
    assert_eq!((*record.z_value, record.samples), (7, [1, 2]));
    assert_eq!(Record::<'_, u32, 2>::db_table_name(), "generic_records");
    let columns = Record::<'_, u32, 2>::db_table_columns();
    assert_eq!(columns.as_slice(), &["z_value", "samples"]);
    assert_eq!(
        format!("{columns} | {columns}"),
        "z_value, samples | z_value, samples",
    );
}
