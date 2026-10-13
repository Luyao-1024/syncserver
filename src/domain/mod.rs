use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
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
