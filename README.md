# price-indexer

Gets `pool_change_event`s from [trade-indexer](https://github.com/INTEARnear/trade-indexer), calculates 'price token0 in token1' and 'price token1 in token0', and pushes it as `price_pool_event`s and `price_token_event`s.
Also pushes updates for all tokens every 5 seconds, and hosts a HTTP server with `/prices` (simple json map `account_id -> price`) and `/list-token-price` (same format as Ref's [indexer-helper](https://github.com/ref-finance/indexer-helper) with additional token metadata).
