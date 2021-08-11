// Copyright 2020-2021 The Datafuse Authors.
//
// SPDX-License-Identifier: Apache-2.0.

use std::convert::TryInto;
use std::pin::Pin;
use std::sync::Arc;

use common_arrow::arrow_flight::flight_service_server::FlightService;
use common_arrow::arrow_flight::Action;
use common_arrow::arrow_flight::ActionType;
use common_arrow::arrow_flight::Criteria;
use common_arrow::arrow_flight::Empty;
use common_arrow::arrow_flight::FlightData;
use common_arrow::arrow_flight::FlightDescriptor;
use common_arrow::arrow_flight::FlightInfo;
use common_arrow::arrow_flight::HandshakeRequest;
use common_arrow::arrow_flight::HandshakeResponse;
use common_arrow::arrow_flight::PutResult;
use common_arrow::arrow_flight::Result as FlightResult;
use common_arrow::arrow_flight::SchemaResult;
use common_arrow::arrow_flight::Ticket;
use tokio_stream::Stream;
use tonic::Request;
use tonic::Response as RawResponse;
use tonic::Status;
use tonic::Streaming;

use crate::api::rpc::flight_actions::FlightAction;
use crate::api::rpc::flight_dispatcher::DatafuseQueryFlightDispatcher;
use crate::api::rpc::flight_service_stream::FlightDataStream;
use crate::api::rpc::flight_tickets::FlightTicket;
use crate::sessions::SessionManagerRef;

pub type FlightStream<T> =
    Pin<Box<dyn Stream<Item = Result<T, tonic::Status>> + Send + Sync + 'static>>;

pub struct DatafuseQueryFlightService {
    sessions: SessionManagerRef,
    dispatcher: Arc<DatafuseQueryFlightDispatcher>,
}

impl DatafuseQueryFlightService {
    pub fn create(
        dispatcher: Arc<DatafuseQueryFlightDispatcher>,
        sessions: SessionManagerRef,
    ) -> Self {
        DatafuseQueryFlightService {
            sessions,
            dispatcher,
        }
    }
}

type Response<T> = Result<RawResponse<T>, Status>;
type StreamRequest<T> = Request<Streaming<T>>;

#[async_trait::async_trait]
impl FlightService for DatafuseQueryFlightService {
    type HandshakeStream = FlightStream<HandshakeResponse>;

    async fn handshake(
        &self,
        _: StreamRequest<HandshakeRequest>,
    ) -> Response<Self::HandshakeStream> {
        Result::Err(Status::unimplemented(
            "DatafuseQuery does not implement handshake.",
        ))
    }

    type ListFlightsStream = FlightStream<FlightInfo>;

    async fn list_flights(&self, _: Request<Criteria>) -> Response<Self::ListFlightsStream> {
        Result::Err(Status::unimplemented(
            "DatafuseQuery does not implement list_flights.",
        ))
    }

    async fn get_flight_info(&self, _: Request<FlightDescriptor>) -> Response<FlightInfo> {
        Err(Status::unimplemented(
            "DatafuseQuery does not implement get_flight_info.",
        ))
    }

    async fn get_schema(&self, _: Request<FlightDescriptor>) -> Response<SchemaResult> {
        Err(Status::unimplemented(
            "DatafuseQuery does not implement get_schema.",
        ))
    }

    type DoGetStream = FlightStream<FlightData>;

    async fn do_get(&self, request: Request<Ticket>) -> Response<Self::DoGetStream> {
        let ticket: FlightTicket = request.into_inner().try_into()?;

        match ticket {
            FlightTicket::StreamTicket(steam_ticket) => {
                let receiver = self.dispatcher.get_stream(&steam_ticket)?;

                Ok(RawResponse::new(
                    Box::pin(FlightDataStream::create(receiver)) as FlightStream<FlightData>,
                ))
            }
        }
    }

    type DoPutStream = FlightStream<PutResult>;

    async fn do_put(&self, _: StreamRequest<FlightData>) -> Response<Self::DoPutStream> {
        Result::Err(Status::unimplemented(
            "DatafuseQuery does not implement do_put.",
        ))
    }

    type DoExchangeStream = FlightStream<FlightData>;

    async fn do_exchange(&self, _: StreamRequest<FlightData>) -> Response<Self::DoExchangeStream> {
        Result::Err(Status::unimplemented(
            "DatafuseQuery does not implement do_exchange.",
        ))
    }

    type DoActionStream = FlightStream<FlightResult>;

    async fn do_action(&self, request: Request<Action>) -> Response<Self::DoActionStream> {
        let action = request.into_inner();
        let flight_action: FlightAction = action.try_into()?;

        let do_flight_action = || -> common_exception::Result<FlightResult> {
            match &flight_action {
                FlightAction::CancelAction(action) => {
                    // We only destroy when session is exist
                    let session_id = action.query_id.clone();
                    if let Some(session) = self.sessions.get_session(&session_id) {
                        // TODO: remove streams
                        session.force_kill_session();
                    }

                    Ok(FlightResult { body: vec![] })
                }
                FlightAction::BroadcastAction(action) => {
                    let session_id = action.query_id.clone();
                    let is_aborted = self.dispatcher.is_aborted();
                    let session = self.sessions.create_rpc_session(session_id, is_aborted)?;

                    self.dispatcher.broadcast_action(session, flight_action)?;
                    Ok(FlightResult { body: vec![] })
                }
                FlightAction::PrepareShuffleAction(action) => {
                    let session_id = action.query_id.clone();
                    let is_aborted = self.dispatcher.is_aborted();
                    let session = self.sessions.create_rpc_session(session_id, is_aborted)?;

                    self.dispatcher.shuffle_action(session, flight_action)?;
                    Ok(FlightResult { body: vec![] })
                }
            }
        };

        let action_result = do_flight_action()?;
        Ok(RawResponse::new(
            Box::pin(tokio_stream::once(Ok(action_result))) as FlightStream<FlightResult>,
        ))
    }

    type ListActionsStream = FlightStream<ActionType>;

    async fn list_actions(&self, _: Request<Empty>) -> Response<Self::ListActionsStream> {
        Result::Ok(RawResponse::new(
            Box::pin(tokio_stream::iter(vec![
                Ok(ActionType {
                    r#type: "PrepareShuffleAction".to_string(),
                    description: "Prepare a query stage that can be sent to the remote after receiving data from remote".to_string(),
                })
            ])) as FlightStream<ActionType>
        ))
    }
}