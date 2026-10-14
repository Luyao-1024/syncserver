//! Versioned and domain-separated HMAC tokens. No timestamps or short expiry:
//! retained metadata history and database generation determine read validity.
use crate::{
    Error, Result,
    domain::{
        records::{DomainResult, Failure},
        sync::{Claims, MAX_TOKEN_BYTES},
    },
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

pub(crate) struct TokenCodec {
    key: Zeroizing<[u8; 32]>,
}
impl TokenCodec {
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            key: Zeroizing::new(key),
        }
    }
    pub fn encode(&self, claims: &Claims) -> Result<String> {
        self.seal(claims.cursor.kind(), claims)
    }
    pub fn seal<T: serde::Serialize>(&self, kind: &str, value: &T) -> Result<String> {
        let payload =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).map_err(|_| Error::RecoveryFailed)?);
        let mut mac =
            Hmac::<Sha256>::new_from_slice(self.key.as_ref()).map_err(|_| Error::RecoveryFailed)?;
        mac.update(b"syncserver/read-token/v1\0");
        mac.update(kind.as_bytes());
        mac.update(b"\0");
        mac.update(payload.as_bytes());
        let token = format!(
            "{kind}.{payload}.{}",
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        );
        if token.len() > MAX_TOKEN_BYTES {
            return Err(Error::RecoveryFailed);
        }
        Ok(token)
    }
    pub fn decode(&self, token: &str, expected_kind: &str) -> DomainResult<Claims> {
        let claims: Claims = self.unseal(token, expected_kind)?;
        if claims.version != 1 || claims.cursor.kind() != expected_kind {
            return Err(Failure::bad());
        }
        Ok(claims)
    }
    pub fn unseal<T: serde::de::DeserializeOwned>(
        &self,
        token: &str,
        expected_kind: &str,
    ) -> DomainResult<T> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(Failure::bad());
        }
        let mut parts = token.split('.');
        let kind = parts.next().ok_or_else(Failure::bad)?;
        let payload = parts.next().ok_or_else(Failure::bad)?;
        let signature = parts.next().ok_or_else(Failure::bad)?;
        if parts.next().is_some() || kind != expected_kind {
            return Err(Failure::bad());
        }
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| Failure::bad())?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(self.key.as_ref()).map_err(|_| Failure::bad())?;
        mac.update(b"syncserver/read-token/v1\0");
        mac.update(kind.as_bytes());
        mac.update(b"\0");
        mac.update(payload.as_bytes());
        mac.verify_slice(&signature).map_err(|_| Failure::bad())?;
        let bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| Failure::bad())?;
        if URL_SAFE_NO_PAD.encode(&bytes) != payload {
            return Err(Failure::bad());
        }
        serde_json::from_slice(&bytes).map_err(|_| Failure::bad())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Namespace, sync::Cursor};
    use uuid::Uuid;
    #[test]
    fn signatures_separate_kinds_and_keys_and_bound_input() {
        let codec = TokenCodec::new([1; 32]);
        let claims = Claims {
            version: 1,
            namespace: Namespace::configured("test".into()),
            generation: Uuid::new_v4(),
            zone: None,
            cursor: Cursor::Database {
                position: 0,
                watermark: None,
            },
        };
        let token = codec.encode(&claims).unwrap();
        assert!(codec.decode(&token, "database").is_ok());
        assert!(codec.decode(&token, "zone").is_err());
        assert!(TokenCodec::new([2; 32]).decode(&token, "database").is_err());
        assert!(
            codec
                .decode(
                    &format!("zone.{}", token.split_once('.').unwrap().1),
                    "zone"
                )
                .is_err()
        );
        assert!(codec.decode(&format!("{token}.extra"), "database").is_err());
        assert!(
            codec
                .decode(&"x".repeat(MAX_TOKEN_BYTES + 1), "database")
                .is_err()
        );
    }
}
