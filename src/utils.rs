pub fn get_rpc_url() -> String {
    std::env::var("RPC_URL").unwrap_or_else(|_| "https://rpc.shitzuapes.xyz".to_string())
}

pub mod serde_bigdecimal {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};
    use sqlx::types::BigDecimal;
    use std::str::FromStr;

    pub fn serialize<S>(value: &BigDecimal, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BigDecimal, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        BigDecimal::from_str(&s).map_err(D::Error::custom)
    }
}

pub mod serde_bigdecimal_tuple2 {
    use serde::{de::Error, Deserialize, Deserializer, Serialize, Serializer};
    use sqlx::types::BigDecimal;
    use std::str::FromStr;

    pub fn serialize<S>(value: &(BigDecimal, BigDecimal), serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        (value.0.to_string(), value.1.to_string()).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<(BigDecimal, BigDecimal), D::Error>
    where
        D: Deserializer<'de>,
    {
        let tuple = <(String, String)>::deserialize(deserializer)?;
        Ok((
            BigDecimal::from_str(&tuple.0).map_err(D::Error::custom)?,
            BigDecimal::from_str(&tuple.1).map_err(D::Error::custom)?,
        ))
    }
}
