/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES.
 * SPDX-License-Identifier: Apache-2.0
 */

use super::*;

fn group(id: &str) -> ExpectedRackGroup {
    ExpectedRackGroup {
        rack_group_id: RackGroupId::new(id),
        topology: RackGroupTopology::new("gb200_nvl72r1_c2g4"),
        rack_ids: vec![RackId::new("rack-02"), RackId::new("rack-01")],
        members: vec![ExpectedRackGroupMember {
            device_type: "compute-tray".to_string(),
            manufacturer: "NVIDIA".to_string(),
            id: "device-01".to_string(),
        }],
        metadata: Metadata {
            name: "nvl5-gp1-jhb01".to_string(),
            description: String::new(),
            labels: [("location.datacenter".to_string(), "JHB01".to_string())]
                .into_iter()
                .collect(),
        },
    }
}

#[crate::sqlx_test]
async fn expected_rack_group_persistence(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let mut expected = group("group-b");
    create(&mut txn, &expected).await?;
    assert_eq!(
        find_by_rack_group_id(&mut txn, &expected.rack_group_id).await?,
        Some(expected.clone())
    );
    // Declaration is independent of device discovery and rack ingestion.
    create(&mut txn, &group("group-a")).await?;
    let all = find_all(&mut txn).await?;
    assert_eq!(
        all.iter()
            .map(|g| g.rack_group_id.as_str())
            .collect::<Vec<_>>(),
        ["group-a", "group-b"]
    );
    expected.members.clear();
    expected.rack_ids.clear();
    expected.topology = RackGroupTopology::new("future-topology");
    update(&mut txn, &expected).await?;
    assert_eq!(
        find_by_rack_group_id(&mut txn, &expected.rack_group_id).await?,
        Some(expected.clone())
    );
    txn.commit().await?;

    let mut txn = pool.begin().await?;
    delete(&mut txn, &expected.rack_group_id).await?;
    txn.rollback().await?;
    let mut txn = pool.begin().await?;
    assert!(
        find_by_rack_group_id(&mut txn, &expected.rack_group_id)
            .await?
            .is_some()
    );
    clear(&mut txn).await?;
    assert!(find_all(&mut txn).await?.is_empty());
    assert!(matches!(
        update(&mut txn, &expected).await,
        Err(DatabaseError::NotFoundError { .. })
    ));
    assert!(matches!(
        delete(&mut txn, &expected.rack_group_id).await,
        Err(DatabaseError::NotFoundError { .. })
    ));
    Ok(())
}
