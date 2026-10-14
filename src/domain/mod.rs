pub mod assets;
pub mod events;
pub mod leases;
pub mod photos;
pub mod records;
pub(crate) mod sync;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Namespace {
    pub tenant: String,
    pub container: String,
    pub environment: String,
    pub database: String,
}

impl Namespace {
    pub(crate) fn configured(container: String) -> Self {
        Self {
            tenant: "local".into(),
            container,
            environment: "production".into(),
            database: "private".into(),
        }
    }
}
