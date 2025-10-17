use crate::{AdapterError, AdapterResult};
use hft_core::{Price, Quantity};

/// Parse JSON text into a type using either simd-json (if enabled) or serde_json.
pub fn parse_json<T: serde::de::DeserializeOwned>(text: &str) -> AdapterResult<T> {
    #[cfg(feature = "json-simd")]
    {
        // simd-json requires &mut [u8]
        let mut bytes = text.as_bytes().to_vec();
        let val: T = simd_json::serde::from_slice(bytes.as_mut_slice())?;
        Ok(val)
    }
    #[cfg(not(feature = "json-simd"))]
    {
        let val: T = serde_json::from_str(text)?;
        Ok(val)
    }
}

/// Parse an owned JSON value into target type, using simd-json when可用。
pub fn parse_owned_value<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> AdapterResult<T> {
    #[cfg(feature = "json-simd")]
    {
        let owned: simd_json::OwnedValue = value
            .try_into()
            .map_err(|e| AdapterError::Serde(e.to_string()))?;
        let val: T = simd_json::serde::from_owned_value(owned)
            .map_err(|e| AdapterError::Serde(e.to_string()))?;
        Ok(val)
    }
    #[cfg(not(feature = "json-simd"))]
    {
        Ok(serde_json::from_value(value)?)
    }
}

#[inline]
pub fn parse_price(s: &str) -> AdapterResult<Price> {
    Price::from_str(s).map_err(|e| AdapterError::Parse(e.to_string()))
}

#[inline]
pub fn parse_quantity(s: &str) -> AdapterResult<Quantity> {
    Quantity::from_str(s).map_err(|e| AdapterError::Parse(e.to_string()))
}
