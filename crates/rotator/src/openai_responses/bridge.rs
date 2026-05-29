use std::convert::Infallible;

use bytes::Bytes;
use futures::Stream;
use models::{chat::ChatCompletionResponse, responses::CreateResponseRequest};

use super::error::Result;
use super::id::{Clock, ResponseIdFactory, SystemClock};
use super::request::{
    ResponsesRequestContext, TranslatedResponsesRequest, responses_request_to_chat_request,
};
use super::response::chat_completion_to_response;
use super::stream::chat_sse_to_responses_sse;

#[derive(Debug, Clone)]
pub struct ResponsesBridge<C = SystemClock> {
    clock: C,
    id_factory: ResponseIdFactory,
}

impl Default for ResponsesBridge<SystemClock> {
    fn default() -> Self {
        Self {
            clock: SystemClock,
            id_factory: ResponseIdFactory::default(),
        }
    }
}

impl<C: Clock + Clone + Send + Sync + 'static> ResponsesBridge<C> {
    pub fn new(clock: C, id_factory: ResponseIdFactory) -> Self {
        Self { clock, id_factory }
    }

    pub fn translate_request(
        &self,
        request: CreateResponseRequest,
    ) -> Result<TranslatedResponsesRequest> {
        responses_request_to_chat_request(request)
    }

    pub fn translate_response(
        &self,
        chat: ChatCompletionResponse,
        context: &ResponsesRequestContext,
    ) -> Result<models::responses::Response> {
        Ok(chat_completion_to_response(
            chat,
            context,
            &self.id_factory,
            &self.clock,
        ))
    }

    pub fn translate_stream<S, E>(
        &self,
        chat_stream: S,
        context: ResponsesRequestContext,
    ) -> impl Stream<Item = std::result::Result<Bytes, Infallible>> + use<S, E, C>
    where
        S: Stream<Item = std::result::Result<Bytes, E>> + Send + 'static,
        E: std::fmt::Display,
    {
        chat_sse_to_responses_sse(chat_stream, context, self.id_factory, self.clock.clone())
    }
}
