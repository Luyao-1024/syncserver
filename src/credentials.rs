use crate::{Error, Result, store::files::open_private};
use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, os::unix::fs::MetadataExt, path::Path};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub(crate) struct Credentials {
    api_digest: [u8; 32],
    pub signing_fingerprint: [u8; 32],
}

impl Credentials {
    pub fn load(api_path: &Path, signing_path: &Path) -> Result<Self> {
        let api = read_line(api_path)?;
        if !(32..=512).contains(&api.len()) || !api.bytes().all(|b| (33..=126).contains(&b)) {
            return Err(Error::Config(
                "API key must be a single line of 32-512 printable non-whitespace ASCII characters",
            ));
        }
        let signing = read_line(signing_path)?;
        if api.as_str() == signing.as_str() {
            return Err(Error::Config("API and signing keys must be independent"));
        }
        let key = Zeroizing::new(STANDARD.decode(signing.as_bytes()).map_err(|_| {
            Error::Config("signing key must be canonical Base64 of exactly 32 bytes")
        })?);
        if key.len() != 32 || STANDARD.encode(&*key) != *signing {
            return Err(Error::Config(
                "signing key must be canonical Base64 of exactly 32 bytes",
            ));
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(&key)
            .map_err(|_| Error::Config("invalid signing key"))?;
        mac.update(b"syncserver/key-fingerprint/v1");
        Ok(Self {
            api_digest: Sha256::digest(api.as_bytes()).into(),
            signing_fingerprint: mac.finalize().into_bytes().into(),
        })
    }

    pub fn authenticate(&self, key: &str) -> bool {
        if !(32..=512).contains(&key.len()) {
            return false;
        }
        let digest: [u8; 32] = Sha256::digest(key.as_bytes()).into();
        bool::from(self.api_digest.ct_eq(&digest))
    }
}

fn read_line(path: &Path) -> Result<Zeroizing<String>> {
    let mut file: File = open_private(path, false)?;
    let metadata = file.metadata()?;
    if metadata.mode() & 0o777 != 0o600 || metadata.len() > 1024 {
        return Err(Error::Config(
            "credential files must be regular, owned by the service user, mode 0600 and at most 1024 bytes",
        ));
    }
    let mut data = Zeroizing::new(String::new());
    (&mut file).take(1025).read_to_string(&mut data)?;
    if data.len() > 1024 {
        return Err(Error::Config("credential file is too large"));
    }
    if data.ends_with('\n') {
        data.pop();
        if data.ends_with('\r') {
            data.pop();
        }
    }
    if data.contains(['\r', '\n']) {
        return Err(Error::Config("credential file must contain one line"));
    }
    Ok(data)
}
