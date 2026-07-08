use std::{
    env,
    fs::{self},
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(crate) struct ToolPaths {
    pub(crate) ffmpeg: Option<PathBuf>,
    pub(crate) ffprobe: Option<PathBuf>,
}

impl ToolPaths {
    pub(crate) fn resolve(binary_dir: Option<&Path>) -> Self {
        Self {
            ffmpeg: resolve_tool("FFMPEG_BIN", "ffmpeg", binary_dir),
            ffprobe: resolve_tool("FFPROBE_BIN", "ffprobe", binary_dir),
        }
    }
}

pub(crate) fn resolve_tool(
    env_name: &str,
    fallback: &str,
    binary_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Ok(path) = env::var(env_name) {
        return Some(PathBuf::from(path));
    }
    binary_dir.and_then(|dir| binary_candidate(dir, fallback))
}

pub(crate) fn binary_candidate(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    let prefix = format!("{name}-");
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .is_some_and(|file_name| file_name.starts_with(&prefix))
        })
}
