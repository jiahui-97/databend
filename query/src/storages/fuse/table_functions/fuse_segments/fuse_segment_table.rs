//  Copyright 2021 Datafuse Labs.
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
//

use std::any::Any;
use std::future::Future;
use std::sync::Arc;

use common_datablocks::DataBlock;
use common_exception::ErrorCode;
use common_exception::Result;
use common_meta_types::TableIdent;
use common_meta_types::TableInfo;
use common_meta_types::TableMeta;
use common_planners::Expression;
use common_planners::Extras;
use common_planners::Partitions;
use common_planners::ReadDataSourcePlan;
use common_planners::Statistics;
use common_streams::DataBlockStream;
use common_streams::SendableDataBlockStream;

use super::fuse_segment::FuseSegment;
use super::table_args::parse_func_history_args;
use crate::catalogs::CATALOG_DEFAULT;
use crate::pipelines::new::processors::port::OutputPort;
use crate::pipelines::new::processors::processor::ProcessorPtr;
use crate::pipelines::new::processors::AsyncSource;
use crate::pipelines::new::processors::AsyncSourcer;
use crate::pipelines::new::NewPipe;
use crate::pipelines::new::NewPipeline;
use crate::sessions::QueryContext;
use crate::storages::fuse::table_functions::string_literal;
use crate::storages::fuse::FuseTable;
use crate::storages::Table;
use crate::table_functions::TableArgs;
use crate::table_functions::TableFunction;

const FUSE_FUNC_SEGMENT: &str = "fuse_segment";

pub struct FuseSegmentTable {
    table_info: TableInfo,
    arg_database_name: String,
    arg_table_name: String,
    arg_snapshot_id: String,
}

impl FuseSegmentTable {
    pub fn create(
        database_name: &str,
        table_func_name: &str,
        table_id: u64,
        table_args: TableArgs,
    ) -> Result<Arc<dyn TableFunction>> {
        let (arg_database_name, arg_table_name, arg_snapshot_id) =
            parse_func_history_args(&table_args)?;

        let engine = FUSE_FUNC_SEGMENT.to_owned();

        let table_info = TableInfo {
            ident: TableIdent::new(table_id, 0),
            desc: format!("'{}'.'{}'", database_name, table_func_name),
            name: table_func_name.to_string(),
            meta: TableMeta {
                schema: FuseSegment::schema(),
                engine,
                ..Default::default()
            },
        };

        Ok(Arc::new(FuseSegmentTable {
            table_info,
            arg_database_name,
            arg_table_name,
            arg_snapshot_id,
        }))
    }
}

#[async_trait::async_trait]
impl Table for FuseSegmentTable {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_table_info(&self) -> &TableInfo {
        &self.table_info
    }

    async fn read_partitions(
        &self,
        _ctx: Arc<QueryContext>,
        _push_downs: Option<Extras>,
    ) -> Result<(Statistics, Partitions)> {
        Ok((Statistics::default(), vec![]))
    }

    fn table_args(&self) -> Option<Vec<Expression>> {
        Some(vec![
            string_literal(self.arg_database_name.as_str()),
            string_literal(self.arg_table_name.as_str()),
            string_literal(self.arg_snapshot_id.as_str()),
        ])
    }

    async fn read(
        &self,
        ctx: Arc<QueryContext>,
        _plan: &ReadDataSourcePlan,
    ) -> Result<SendableDataBlockStream> {
        let tenant_id = ctx.get_tenant();
        let tbl = ctx
            .get_catalog(CATALOG_DEFAULT)?
            .get_table(
                tenant_id.as_str(),
                self.arg_database_name.as_str(),
                self.arg_table_name.as_str(),
            )
            .await?;

        let tbl = tbl.as_any().downcast_ref::<FuseTable>().ok_or_else(|| {
            ErrorCode::BadArguments(format!(
                "expecting fuse table, but got table of engine type: {}",
                tbl.get_table_info().meta.engine
            ))
        })?;

        let blocks = vec![
            FuseSegment::new(ctx.clone(), tbl, self.arg_snapshot_id.clone())
                .get_segments()
                .await?,
        ];
        Ok(Box::pin(DataBlockStream::create(
            FuseSegment::schema(),
            None,
            blocks,
        )))
    }

    fn read2(
        &self,
        ctx: Arc<QueryContext>,
        _: &ReadDataSourcePlan,
        pipeline: &mut NewPipeline,
    ) -> Result<()> {
        let output = OutputPort::create();
        pipeline.add_pipe(NewPipe::SimplePipe {
            inputs_port: vec![],
            outputs_port: vec![output.clone()],
            processors: vec![FuseHistorySource::create(
                ctx,
                output,
                self.arg_database_name.to_owned(),
                self.arg_table_name.to_owned(),
                self.arg_snapshot_id.to_owned(),
            )?],
        });

        Ok(())
    }
}

struct FuseHistorySource {
    finish: bool,
    ctx: Arc<QueryContext>,
    arg_database_name: String,
    arg_table_name: String,
    arg_snapshot_id: String,
}

impl FuseHistorySource {
    pub fn create(
        ctx: Arc<QueryContext>,
        output: Arc<OutputPort>,
        arg_database_name: String,
        arg_table_name: String,
        arg_snapshot_id: String,
    ) -> Result<ProcessorPtr> {
        AsyncSourcer::create(ctx.clone(), output, FuseHistorySource {
            ctx,
            finish: false,
            arg_table_name,
            arg_database_name,
            arg_snapshot_id,
        })
    }
}

impl AsyncSource for FuseHistorySource {
    const NAME: &'static str = "fuse_snapshot";

    type BlockFuture<'a> = impl Future<Output = Result<Option<DataBlock>>> where Self: 'a;

    fn generate(&mut self) -> Self::BlockFuture<'_> {
        async {
            if self.finish {
                return Ok(None);
            }

            self.finish = true;
            let tenant_id = self.ctx.get_tenant();
            let tbl = self
                .ctx
                .get_catalog(CATALOG_DEFAULT)?
                .get_table(
                    tenant_id.as_str(),
                    self.arg_database_name.as_str(),
                    self.arg_table_name.as_str(),
                )
                .await?;

            let tbl = FuseTable::try_from_table(tbl.as_ref())?;
            Ok(Some(
                FuseSegment::new(self.ctx.clone(), tbl, self.arg_snapshot_id.clone())
                    .get_segments()
                    .await?,
            ))
        }
    }
}

impl TableFunction for FuseSegmentTable {
    fn function_name(&self) -> &str {
        self.name()
    }

    fn as_table<'a>(self: Arc<Self>) -> Arc<dyn Table + 'a>
    where Self: 'a {
        self
    }
}
