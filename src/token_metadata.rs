use cached::proc_macro::cached;
use inindexer::near_indexer_primitives::{
    types::{AccountId, BlockHeight, BlockId, BlockReference, Finality},
    views::QueryRequest,
};
use near_jsonrpc_client::{
    errors::JsonRpcError,
    methods::{self, query::RpcQueryError},
    JsonRpcClient,
};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::utils::get_rpc_url;

#[cached(time = 86400, result = true)]
pub async fn get_token_metadata(
    token_id: AccountId,
    block_height: Option<BlockHeight>,
) -> Result<TokenMetadataWithOptionalIcon, MetadataError> {
    if let Some(block_height) = block_height {
        if let Ok(metadata) =
            _get_token_metadata_internal(token_id.clone(), Some(block_height)).await
        {
            Ok(metadata)
        } else {
            _get_token_metadata_internal(token_id, None).await
        }
    } else {
        _get_token_metadata_internal(token_id, None).await
    }
}

async fn _get_token_metadata_internal(
    token_id: AccountId,
    block_height: Option<BlockHeight>,
) -> Result<TokenMetadataWithOptionalIcon, MetadataError> {
    let client = JsonRpcClient::connect(get_rpc_url());
    let request = methods::query::RpcQueryRequest {
        block_reference: if let Some(block_height) = block_height {
            BlockReference::BlockId(BlockId::Height(block_height))
        } else {
            BlockReference::Finality(Finality::None)
        },
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
pub struct TokenMetadataWithOptionalIcon {
    pub name: String,
    pub symbol: String,
    pub decimals: u32,
    pub reference: Option<String>,
    #[serde(skip_serializing, default)]
    pub icon: Option<String>,
}
