use std::{
    fs,
    path::{Path as FsPath, PathBuf},
};

use axum::http::StatusCode;
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use tracing::warn;

use crate::{config::Config, errors::ApiError, CATALOG_CONTENT_TYPE};

pub(crate) fn read_catalog_state_value(config: &Config) -> Option<Value> {
    let raw = match fs::read_to_string(&config.catalog_state_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            warn!(
                path = %config.catalog_state_path.display(),
                "Could not read catalog state file: {err}"
            );
            return None;
        }
    };

    match serde_json::from_str::<Value>(&raw) {
        Ok(value) => Some(value),
        Err(err) => {
            let broken_path = catalog_state_broken_path(&config.catalog_state_path);
            match fs::rename(&config.catalog_state_path, &broken_path) {
                Ok(()) => warn!(
                    path = %config.catalog_state_path.display(),
                    broken_path = %broken_path.display(),
                    "Quarantined invalid catalog state file: {err}"
                ),
                Err(rename_err) => warn!(
                    path = %config.catalog_state_path.display(),
                    broken_path = %broken_path.display(),
                    "Invalid catalog state file could not be quarantined: {err}; rename failed: {rename_err}"
                ),
            }
            None
        }
    }
}

fn catalog_state_broken_path(path: &FsPath) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("catalog.json");
    path.with_file_name(format!("{file_name}.broken"))
}

fn catalog_address_from_state(value: &Value) -> Option<String> {
    value
        .get("catalog_address")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn read_catalog_address(config: &Config) -> Option<String> {
    read_catalog_state_value(config)
        .as_ref()
        .and_then(catalog_address_from_state)
        .or_else(|| config.catalog_bootstrap_address.clone())
}

pub(super) fn read_catalog_snapshot(config: &Config) -> Option<(Value, Option<String>)> {
    let value = read_catalog_state_value(config)?;
    let mut catalog = value.get("catalog")?.clone();
    if !catalog.is_object() {
        return None;
    }
    if !catalog.get("videos").is_some_and(Value::is_array) {
        catalog["videos"] = json!([]);
    }
    Some((
        catalog,
        catalog_address_from_state(&value).or_else(|| config.catalog_bootstrap_address.clone()),
    ))
}

pub(super) fn empty_catalog() -> Value {
    json!({
        "schema_version": 1,
        "content_type": CATALOG_CONTENT_TYPE,
        "updated_at": Utc::now().to_rfc3339(),
        "videos": [],
    })
}

pub(crate) fn write_catalog_state<T: Serialize + ?Sized>(
    config: &Config,
    address: Option<&str>,
    catalog: Option<&T>,
    publish_pending: bool,
) -> Result<(), ApiError> {
    if let Some(parent) = config.catalog_state_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not create catalog state directory: {err}"),
            )
        })?;
    }
    let tmp_path = config.catalog_state_path.with_extension("tmp");
    let mut payload = json!({
        "catalog_address": address.unwrap_or(""),
        "updated_at": Utc::now().to_rfc3339(),
        "publish_pending": publish_pending,
        "note": "Local catalog snapshot plus the latest network-hosted catalog address.",
    });
    if let Some(catalog) = catalog {
        payload["catalog"] = serde_json::to_value(catalog).map_err(|err| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not encode catalog state: {err}"),
            )
        })?;
    }
    fs::write(
        &tmp_path,
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not write catalog state: {err}"),
        )
    })?;
    fs::rename(&tmp_path, &config.catalog_state_path).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not update catalog state: {err}"),
        )
    })
}
