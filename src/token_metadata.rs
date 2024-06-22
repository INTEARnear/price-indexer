use cached::proc_macro::cached;
use inindexer::near_indexer_primitives::{
    types::{AccountId, BlockReference, Finality},
    views::QueryRequest,
};
use near_jsonrpc_client::{methods, JsonRpcClient};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use serde::{Deserialize, Serialize};

const RPC_URL: &str = "https://rpc.shitzuapes.xyz";

#[cached(time = 3600, result = true)]
pub async fn get_token_metadata(token_id: AccountId) -> anyhow::Result<TokenMetadata> {
    let client = JsonRpcClient::connect(RPC_URL);
    let request = methods::query::RpcQueryRequest {
        block_reference: BlockReference::Finality(Finality::Final),
        request: QueryRequest::CallFunction {
            account_id: token_id,
            method_name: "ft_metadata".into(),
            args: Vec::new().into(),
        },
    };
    let response = client.call(request).await?;
    let QueryResponseKind::CallResult(call_result) = response.kind else {
        unreachable!()
    };
    Ok(serde_json::from_slice(&call_result.result)?)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TokenMetadata {
    pub decimals: u32,
    pub symbol: String,
}
