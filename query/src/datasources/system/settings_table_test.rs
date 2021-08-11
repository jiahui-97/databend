// Copyright 2020-2021 The Datafuse Authors.
//
// SPDX-License-Identifier: Apache-2.0.

use common_exception::Result;
use common_planners::*;
use common_runtime::tokio;
use futures::TryStreamExt;
use pretty_assertions::assert_eq;

use crate::datasources::system::*;
use crate::datasources::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_settings_table() -> Result<()> {
    let ctx = crate::tests::try_create_context()?;
    ctx.get_settings().set_max_threads(2)?;

    let table = SettingsTable::create();
    let source_plan = table.read_plan(
        ctx.clone(),
        &ScanPlan::empty(),
        ctx.get_settings().get_max_threads()? as usize,
    )?;

    let stream = table.read(ctx, &source_plan).await?;
    let result = stream.try_collect::<Vec<_>>().await?;
    let block = &result[0];
    assert_eq!(block.num_columns(), 4);

    Ok(())
}