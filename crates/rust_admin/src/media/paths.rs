use std::{
    fs, io,
    path::{Path as FsPath, PathBuf},
};

use axum::http::StatusCode;

use crate::errors::ApiError;

pub(crate) fn assert_under(path: &FsPath, root: &FsPath) -> Result<PathBuf, ApiError> {
    let root = fs::canonicalize(root).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not inspect upload temp directory: {err}"),
        )
    })?;

    let target = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Media path has no parent directory",
                )
            })?;
            let parent = fs::canonicalize(parent).map_err(|err| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Could not inspect media path parent: {err}"),
                )
            })?;
            let file_name = path.file_name().ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Media path has no file name",
                )
            })?;
            parent.join(file_name)
        }
        Err(err) => {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not inspect media path: {err}"),
            ));
        }
    };

    if !target.starts_with(&root) {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Media path is outside the configured upload workspace",
        ));
    }

    Ok(target)
}
