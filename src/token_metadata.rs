use cached::proc_macro::cached;
use inindexer::near_indexer_primitives::{
    types::{AccountId, BlockReference, Finality},
    views::QueryRequest,
};
use near_jsonrpc_client::{
    errors::JsonRpcError,
    methods::{self, query::RpcQueryError},
    JsonRpcClient,
};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use serde::{Deserialize, Serialize};

use crate::utils::RPC_URL;

#[cached(time = 3600, result = true)]
pub async fn get_token_metadata(
    token_id: AccountId,
) -> Result<TokenMetadataWithoutIcon, MetadataError> {
    let client = JsonRpcClient::connect(RPC_URL);
    let request = methods::query::RpcQueryRequest {
        block_reference: BlockReference::Finality(Finality::None),
        request: QueryRequest::CallFunction {
            account_id: token_id,
            method_name: "ft_metadata".into(),
            args: Vec::new().into(),
        },
    };
    let response = client
        .call(request)
        .await
        .map_err(MetadataError::RpcQueryError)?;
    let QueryResponseKind::CallResult(call_result) = response.kind else {
        unreachable!()
    };
    serde_json::from_slice(&call_result.result).map_err(MetadataError::SerdeJsonError)
}

#[derive(Debug)]
pub enum MetadataError {
    RpcQueryError(JsonRpcError<RpcQueryError>),
    SerdeJsonError(#[allow(dead_code)] serde_json::Error),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TokenMetadataWithoutIcon {
    pub name: String,
    pub symbol: String,
    pub decimals: u32,
    pub reference: Option<String>,
}
