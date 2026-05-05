use std::fs;
use std::time::Instant;

use axum::http::{header, HeaderMap, HeaderValue};
use tokio::sync::watch;

use crate::cache::CachedValue;
use crate::models::{Catalog, CatalogState, VideoManifest};
use crate::state::AppState;

pub(crate) fn playlist_headers(state: &AppState) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.apple.mpegurl"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        cache_control_header(state.cache_config.playlist_max_age_seconds()),
    );
    headers
}

pub(crate) fn segment_headers(state: &AppState) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
    headers.insert(
        header::CACHE_CONTROL,
        cache_control_header(state.cache_config.segment_max_age_seconds()),
    );
    headers
}

fn cache_control_header(max_age_seconds: u64) -> HeaderValue {
    if max_age_seconds == 0 {
        return HeaderValue::from_static("no-store");
    }

    HeaderValue::from_str(&format!("public, max-age={max_age_seconds}"))
        .unwrap_or_else(|_| HeaderValue::from_static("no-store"))
}

pub(crate) async fn build_manifest(
    state: &AppState,
    video_id: &str,
    resolution: &str,
) -> Result<String, String> {
    let manifest = load_video_manifest(state, video_id).await?;
    render_manifest(&manifest, resolution, |segment_index| {
        format!("/stream/{video_id}/{resolution}/{segment_index}.ts")
    })
}

pub(crate) async fn build_manifest_from_address(
    state: &AppState,
    manifest_address: &str,
    resolution: &str,
) -> Result<String, String> {
    let manifest = load_manifest(state, manifest_address).await?;
    render_manifest(&manifest, resolution, |segment_index| {
        format!("/stream/manifest/{manifest_address}/{resolution}/{segment_index}.ts")
    })
}

fn render_manifest<F>(
    manifest: &VideoManifest,
    resolution: &str,
    segment_url: F,
) -> Result<String, String>
where
    F: Fn(i32) -> String,
{
    if manifest.status != "ready" {
        return Err("video not ready".to_string());
    }

    let variant = manifest
        .variants
        .iter()
        .find(|variant| variant.resolution == resolution)
        .ok_or_else(|| "variant not found".to_string())?;

    if variant.segments.is_empty() {
        return Err("no segments found".to_string());
    }

    let target_duration = variant
        .segments
        .iter()
        .map(|segment| segment.duration)
        .fold(variant.segment_duration, f64::max)
        .ceil() as u64;
    let mut m3u8 = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target_duration}\n#EXT-X-MEDIA-SEQUENCE:0\n"
    );

    for seg in &variant.segments {
        m3u8.push_str(&format!(
            "#EXTINF:{:.3},\n{}\n",
            seg.duration,
            segment_url(seg.segment_index),
        ));
    }
    m3u8.push_str("#EXT-X-ENDLIST\n");

    Ok(m3u8)
}

pub(crate) async fn fetch_segment(
    state: &AppState,
    video_id: &str,
    resolution: &str,
    seg_index: i32,
) -> Result<Vec<u8>, String> {
    let manifest = load_video_manifest(state, video_id).await?;
    let segment_address = manifest
        .variants
        .iter()
        .find(|variant| variant.resolution == resolution)
        .and_then(|variant| {
            variant
                .segments
                .iter()
                .find(|segment| segment.segment_index == seg_index)
        })
        .map(|segment| segment.autonomi_address.clone())
        .ok_or_else(|| "segment not found".to_string())?;

    fetch_segment_data(state, &segment_address).await
}

pub(crate) async fn fetch_segment_from_address(
    state: &AppState,
    manifest_address: &str,
    resolution: &str,
    seg_index: i32,
) -> Result<Vec<u8>, String> {
    let manifest = load_manifest(state, manifest_address).await?;
    let segment_address = manifest
        .variants
        .iter()
        .find(|variant| variant.resolution == resolution)
        .and_then(|variant| {
            variant
                .segments
                .iter()
                .find(|segment| segment.segment_index == seg_index)
        })
        .map(|segment| segment.autonomi_address.clone())
        .ok_or_else(|| "segment not found".to_string())?;

    fetch_segment_data(state, &segment_address).await
}

fn read_catalog_address(state: &AppState) -> Option<String> {
    if let Ok(raw) = fs::read_to_string(&state.catalog_state_path) {
        if let Ok(catalog_state) = serde_json::from_str::<CatalogState>(&raw) {
            if let Some(address) = catalog_state
                .catalog_address
                .map(|address| address.trim().to_string())
                .filter(|address| !address.is_empty())
            {
                return Some(address);
            }
        }
    }

    state.catalog_bootstrap_address.clone()
}

fn read_catalog_snapshot(state: &AppState) -> Option<Catalog> {
    fs::read_to_string(&state.catalog_state_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<CatalogState>(&raw).ok())
        .and_then(|catalog_state| catalog_state.catalog)
}

async fn load_video_manifest(state: &AppState, video_id: &str) -> Result<VideoManifest, String> {
    let catalog = if let Some(catalog) = read_catalog_snapshot(state) {
        catalog
    } else {
        let catalog_address = read_catalog_address(state)
            .ok_or_else(|| "catalog address not configured".to_string())?;
        load_catalog(state, &catalog_address).await?
    };

    let manifest_address = catalog
        .videos
        .iter()
        .find(|video| video.id == video_id)
        .map(|video| video.manifest_address.clone())
        .ok_or_else(|| "video not found in catalog".to_string())?;

    let manifest = load_manifest(state, &manifest_address).await?;

    if manifest.id != video_id {
        return Err("video manifest ID mismatch".to_string());
    }

    Ok(manifest)
}

async fn load_catalog(state: &AppState, catalog_address: &str) -> Result<Catalog, String> {
    if !state.cache_config.catalog_ttl.is_zero() {
        let now = Instant::now();
        let mut catalogs = state.cache.catalogs.lock().await;
        match catalogs.get(catalog_address) {
            Some(cached) if cached.expires_at > now => return Ok(cached.value.clone()),
            Some(_) => {
                catalogs.remove(catalog_address);
            }
            None => {}
        }
    }

    let catalog_bytes = state
        .antd
        .data_get_public(catalog_address)
        .await
        .map_err(|e| format!("Autonomi catalog fetch failed: {e}"))?;
    let catalog: Catalog =
        serde_json::from_slice(&catalog_bytes).map_err(|e| format!("invalid catalog JSON: {e}"))?;

    if !state.cache_config.catalog_ttl.is_zero() {
        let mut catalogs = state.cache.catalogs.lock().await;
        catalogs.insert(
            catalog_address.to_string(),
            CachedValue {
                value: catalog.clone(),
                expires_at: Instant::now() + state.cache_config.catalog_ttl,
            },
        );
    }

    Ok(catalog)
}

async fn load_manifest(state: &AppState, manifest_address: &str) -> Result<VideoManifest, String> {
    if !state.cache_config.manifest_ttl.is_zero() {
        let now = Instant::now();
        let mut manifests = state.cache.manifests.lock().await;
        match manifests.get(manifest_address) {
            Some(cached) if cached.expires_at > now => return Ok(cached.value.clone()),
            Some(_) => {
                manifests.remove(manifest_address);
            }
            None => {}
        }
    }

    let manifest_bytes = state
        .antd
        .data_get_public(manifest_address)
        .await
        .map_err(|e| format!("Autonomi manifest fetch failed: {e}"))?;
    let manifest: VideoManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("invalid video manifest JSON: {e}"))?;

    if !state.cache_config.manifest_ttl.is_zero() {
        let mut manifests = state.cache.manifests.lock().await;
        manifests.insert(
            manifest_address.to_string(),
            CachedValue {
                value: manifest.clone(),
                expires_at: Instant::now() + state.cache_config.manifest_ttl,
            },
        );
    }

    Ok(manifest)
}

async fn fetch_segment_data(state: &AppState, segment_address: &str) -> Result<Vec<u8>, String> {
    loop {
        {
            let mut segments = state.cache.segments.lock().await;
            if let Some(data) = segments.get(segment_address) {
                return Ok(data);
            }
        }

        let maybe_receiver = {
            let mut fetches = state.cache.segment_fetches.lock().await;
            if let Some(receiver) = fetches.get(segment_address) {
                Some(receiver.clone())
            } else {
                let (sender, receiver) = watch::channel(None);
                fetches.insert(segment_address.to_string(), receiver);
                drop(fetches);

                let result = fetch_segment_data_uncached(state, segment_address).await;
                let _ = sender.send(Some(result.clone()));
                state
                    .cache
                    .segment_fetches
                    .lock()
                    .await
                    .remove(segment_address);
                return result;
            }
        };

        let Some(mut receiver) = maybe_receiver else {
            continue;
        };
        loop {
            let result = receiver.borrow().clone();
            if let Some(result) = result {
                return result;
            }
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }
}

async fn fetch_segment_data_uncached(
    state: &AppState,
    segment_address: &str,
) -> Result<Vec<u8>, String> {
    let data = state
        .antd
        .data_get_public(segment_address)
        .await
        .map_err(|e| format!("Autonomi fetch failed: {e}"))?;

    let mut segments = state.cache.segments.lock().await;
    segments.insert(segment_address.to_string(), data.clone());

    Ok(data)
}
