use std::{
    fs,
    path::{Path as FsPath, PathBuf},
};

use axum::http::StatusCode;

use crate::errors::ApiError;

pub(crate) fn collect_segment_files(seg_dir: &FsPath) -> Result<Vec<PathBuf>, ApiError> {
    let mut files = fs::read_dir(seg_dir)
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("ts"))
        .filter(|path| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|stem| stem.starts_with("seg_"))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|path| segment_index_from_path(path).unwrap_or(i32::MAX));
    Ok(files)
}

pub(crate) fn segment_index_from_path(path: &FsPath) -> Option<i32> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .and_then(|stem| stem.strip_prefix("seg_"))
        .and_then(|value| value.parse::<i32>().ok())
}
