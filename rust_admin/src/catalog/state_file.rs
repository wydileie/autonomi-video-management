//! Local catalog bookmark and recovery state.
//!
//! The admin service intentionally writes catalog state to this JSON file even
//! though SQLite stores the canonical admin metadata. The file is a lightweight
//! bootstrap and recovery aid shared with the streaming service: it records the
//! latest network-hosted catalog addresses plus decoded catalog snapshots, so a
//! restarted stack can resume catalog reads before or without a database query.
//! SQLite remains the source for mutable admin state and durable jobs, while
//! Autonomi remains the durable playback source for ready manifests, segments,
//! and published catalog documents.

use std::{
    fs,
    path::{Path as FsPath, PathBuf},
};

use axum::http::StatusCode;
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use tracing::warn;

use crate::{config::Config, errors::ApiError, CATALOG_CONTENT_TYPE, CATALOG_SCHEMA_VERSION};

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

fn string_value(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .map(ToOwned::to_owned)
}

fn published_catalog_address_from_state(value: &Value) -> Option<String> {
    string_value(value, "published_catalog_address")
        .or_else(|| string_value(value, "catalog_address"))
}

fn all_catalog_address_from_state(value: &Value) -> Option<String> {
    string_value(value, "all_catalog_address")
}

pub(crate) fn read_catalog_address(config: &Config) -> Option<String> {
    read_catalog_state_value(config)
        .as_ref()
        .and_then(published_catalog_address_from_state)
        .or_else(|| config.catalog_bootstrap_address.clone())
}

pub(crate) fn read_all_catalog_address(config: &Config) -> Option<String> {
    read_catalog_state_value(config)
        .as_ref()
        .and_then(all_catalog_address_from_state)
        .or_else(|| config.all_catalog_bootstrap_address.clone())
}

fn normalize_catalog_value(mut catalog: Value) -> Option<Value> {
    if !catalog.is_object() {
        return None;
    }
    if !catalog.get("videos").is_some_and(Value::is_array) {
        catalog["videos"] = json!([]);
    }
    Some(catalog)
}

pub(super) fn read_catalog_snapshot(config: &Config) -> Option<(Value, Option<String>)> {
    let value = read_catalog_state_value(config)?;
    let catalog = value
        .get("published_catalog")
        .or_else(|| value.get("catalog"))?
        .clone();
    let catalog = normalize_catalog_value(catalog)?;
    Some((
        catalog,
        published_catalog_address_from_state(&value)
            .or_else(|| config.catalog_bootstrap_address.clone()),
    ))
}

pub(crate) fn read_catalog_documents(config: &Config) -> (Option<Value>, Option<Value>) {
    let Some(value) = read_catalog_state_value(config) else {
        return (None, None);
    };
    let published = value
        .get("published_catalog")
        .or_else(|| value.get("catalog"))
        .cloned()
        .and_then(normalize_catalog_value);
    let all = value
        .get("all_catalog")
        .cloned()
        .and_then(normalize_catalog_value);
    (published, all)
}

pub(super) fn empty_catalog() -> Value {
    empty_catalog_kind("published")
}

pub(super) fn empty_catalog_kind(kind: &str) -> Value {
    json!({
        "schema_version": CATALOG_SCHEMA_VERSION,
        "content_type": CATALOG_CONTENT_TYPE,
        "catalog_kind": kind,
        "generated_at": Utc::now().to_rfc3339(),
        "updated_at": Utc::now().to_rfc3339(),
        "videos": [],
    })
}

pub(crate) fn write_catalog_state<T: Serialize + ?Sized, U: Serialize + ?Sized>(
    config: &Config,
    published_address: Option<&str>,
    all_address: Option<&str>,
    published_catalog: Option<&T>,
    all_catalog: Option<&U>,
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
        "catalog_address": published_address.unwrap_or(""),
        "published_catalog_address": published_address.unwrap_or(""),
        "all_catalog_address": all_address.unwrap_or(""),
        "updated_at": Utc::now().to_rfc3339(),
        "publish_pending": publish_pending,
        "note": "Local catalog snapshots plus the latest network-hosted catalog addresses.",
    });
    if let Some(catalog) = published_catalog {
        let value = serde_json::to_value(catalog).map_err(|err| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not encode catalog state: {err}"),
            )
        })?;
        payload["catalog"] = value.clone();
        payload["published_catalog"] = value;
    }
    if let Some(catalog) = all_catalog {
        payload["all_catalog"] = serde_json::to_value(catalog).map_err(|err| {
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
