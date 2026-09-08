use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::io::AsyncReadExt;

use axum::http::{header, HeaderMap, HeaderValue};
use bytes::Bytes;
use tokio::sync::watch;
use tracing::{debug, instrument};

use crate::cache::{insert_metadata, CachedValue};
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
        segment_cache_control_header(state.cache_config.segment_max_age_seconds()),
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

fn segment_cache_control_header(max_age_seconds: u64) -> HeaderValue {
    if max_age_seconds == 0 {
        return HeaderValue::from_static("no-store");
    }

    HeaderValue::from_str(&format!("public, max-age={max_age_seconds}, immutable"))
        .unwrap_or_else(|_| HeaderValue::from_static("no-store"))
}

#[instrument(skip(state), fields(video_id = %video_id, resolution = %resolution))]
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

#[instrument(skip(state), fields(manifest_address = %manifest_address, resolution = %resolution))]
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
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-TARGETDURATION:{target_duration}\n#EXT-X-MEDIA-SEQUENCE:0\n"
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

#[instrument(skip(state), fields(video_id = %video_id, resolution = %resolution, segment_index = seg_index))]
pub(crate) async fn fetch_segment(
    state: &AppState,
    video_id: &str,
    resolution: &str,
    seg_index: i32,
) -> Result<Bytes, String> {
    let manifest = load_video_manifest(state, video_id).await?;
    let segment_address = manifest
        .variants
        .iter()
        .find(|variant| variant.resolution == resolution)
        .and_then(|variant| variant.segment_address(seg_index))
        .map(str::to_string)
        .ok_or_else(|| "segment not found".to_string())?;

    fetch_segment_data(state, &segment_address).await
}

#[instrument(skip(state), fields(manifest_address = %manifest_address, resolution = %resolution, segment_index = seg_index))]
pub(crate) async fn fetch_segment_from_address(
    state: &AppState,
    manifest_address: &str,
    resolution: &str,
    seg_index: i32,
) -> Result<Bytes, String> {
    let manifest = load_manifest(state, manifest_address).await?;
    let segment_address = manifest
        .variants
        .iter()
        .find(|variant| variant.resolution == resolution)
        .and_then(|variant| variant.segment_address(seg_index))
        .map(str::to_string)
        .ok_or_else(|| "segment not found".to_string())?;

    fetch_segment_data(state, &segment_address).await
}

async fn local_catalog(state: &AppState) -> Option<Arc<CatalogState>> {
    let mut cached = state.cache.local_catalog.lock().await;
    if let Some(entry) = cached.as_ref().filter(|e| e.expires_at > Instant::now()) {
        return Some(entry.value.clone());
    }
    let file = tokio::fs::File::open(&state.catalog_state_path)
        .await
        .ok()?;
    let mut raw = Vec::new();
    file.take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .await
        .ok()?;
    if raw.len() > 4 * 1024 * 1024 {
        return None;
    }
    let value = Arc::new(serde_json::from_slice::<CatalogState>(&raw).ok()?);
    *cached = Some(CachedValue {
        value: value.clone(),
        expires_at: Instant::now() + state.cache_config.catalog_ttl,
        size_bytes: raw.len(),
    });
    Some(value)
}

pub(crate) async fn read_catalog_address(state: &AppState) -> Option<String> {
    local_catalog(state)
        .await
        .and_then(|c| {
            c.published_catalog_address
                .clone()
                .or_else(|| c.catalog_address.clone())
        })
        .filter(|v| !v.trim().is_empty())
        .or_else(|| state.catalog_bootstrap_address.clone())
}

async fn load_video_manifest(
    state: &AppState,
    video_id: &str,
) -> Result<Arc<VideoManifest>, String> {
    let catalog = if let Some(snapshot) = local_catalog(state)
        .await
        .filter(|c| c.published_catalog.is_some() || c.catalog.is_some())
    {
        snapshot
            .published_catalog
            .as_ref()
            .or(snapshot.catalog.as_ref())
            .cloned()
            .ok_or("catalog missing")?
    } else {
        let catalog_address = read_catalog_address(state)
            .await
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

#[instrument(skip(state), fields(catalog_address = %catalog_address))]
async fn load_catalog(state: &AppState, catalog_address: &str) -> Result<Arc<Catalog>, String> {
    if !state.cache_config.catalog_ttl.is_zero() {
        let now = Instant::now();
        let mut catalogs = state.cache.catalogs.lock().await;
        match catalogs.get(catalog_address) {
            Some(cached) if cached.expires_at > now => {
                debug!(cache = "catalog", hit = true, "catalog cache hit");
                return Ok(cached.value.clone());
            }
            Some(_) => {
                debug!(
                    cache = "catalog",
                    hit = false,
                    expired = true,
                    "catalog cache expired"
                );
                catalogs.remove(catalog_address);
            }
            None => {
                debug!(cache = "catalog", hit = false, "catalog cache miss");
            }
        }
    }

    let _permit = state
        .cache
        .fetch_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| "network fetch capacity exhausted")?;
    let catalog_bytes = state
        .antd
        .data_get_public(catalog_address)
        .await
        .map_err(|e| format!("Autonomi catalog fetch failed: {e}"))?;
    if catalog_bytes.len() > 4 * 1024 * 1024 {
        return Err("catalog exceeds byte limit".into());
    }
    let catalog: Catalog =
        serde_json::from_slice(&catalog_bytes).map_err(|e| format!("invalid catalog JSON: {e}"))?;

    let catalog = Arc::new(catalog);
    if !state.cache_config.catalog_ttl.is_zero() {
        let mut catalogs = state.cache.catalogs.lock().await;
        insert_metadata(
            &mut catalogs,
            catalog_address.to_string(),
            CachedValue {
                value: catalog.clone(),
                expires_at: Instant::now() + state.cache_config.catalog_ttl,
                size_bytes: catalog_bytes.len().saturating_mul(4),
            },
        );
    }

    Ok(catalog)
}

#[instrument(skip(state), fields(manifest_address = %manifest_address))]
async fn load_manifest(
    state: &AppState,
    manifest_address: &str,
) -> Result<Arc<VideoManifest>, String> {
    if !state.cache_config.manifest_ttl.is_zero() {
        let now = Instant::now();
        let mut manifests = state.cache.manifests.lock().await;
        match manifests.get(manifest_address) {
            Some(cached) if cached.expires_at > now => {
                debug!(cache = "manifest", hit = true, "manifest cache hit");
                return Ok(cached.value.clone());
            }
            Some(_) => {
                debug!(
                    cache = "manifest",
                    hit = false,
                    expired = true,
                    "manifest cache expired"
                );
                manifests.remove(manifest_address);
            }
            None => {
                debug!(cache = "manifest", hit = false, "manifest cache miss");
            }
        }
    }

    let _permit = state
        .cache
        .fetch_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| "network fetch capacity exhausted")?;
    let manifest_bytes = state
        .antd
        .data_get_public(manifest_address)
        .await
        .map_err(|e| format!("Autonomi manifest fetch failed: {e}"))?;
    if manifest_bytes.len() > 4 * 1024 * 1024 {
        return Err("manifest exceeds byte limit".into());
    }
    let mut manifest: VideoManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("invalid video manifest JSON: {e}"))?;
    manifest.index_segments()?;
    let manifest = Arc::new(manifest);

    if !state.cache_config.manifest_ttl.is_zero() {
        let mut manifests = state.cache.manifests.lock().await;
        insert_metadata(
            &mut manifests,
            manifest_address.to_string(),
            CachedValue {
                value: manifest.clone(),
                expires_at: Instant::now() + state.cache_config.manifest_ttl,
                size_bytes: manifest_bytes.len().saturating_mul(4),
            },
        );
    }

    Ok(manifest)
}

#[instrument(skip(state), fields(segment_address = %segment_address))]
async fn fetch_segment_data(state: &AppState, segment_address: &str) -> Result<Bytes, String> {
    let started = Instant::now();
    if let Some(data) = state.cache.segments.lock().await.get(segment_address) {
        state.metrics.record_segment_cache_hit();
        state
            .metrics
            .record_segment_fetch_latency("cache_hit", started.elapsed());
        return Ok(data);
    }
    let mut fetches = state.cache.segment_fetches.lock().await;
    let mut receiver = if let Some(receiver) = fetches.get(segment_address) {
        state.metrics.record_segment_fetch_coalesced();
        receiver.clone()
    } else {
        let permit = state
            .cache
            .fetch_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "network fetch capacity exhausted")?;
        state.metrics.record_segment_cache_miss();
        let (sender, receiver) = watch::channel(None);
        fetches.insert(segment_address.to_owned(), receiver.clone());
        let state = state.clone();
        let address = segment_address.to_owned();
        // This task owns the fetch, so dropping any HTTP waiter cannot poison the entry.
        tokio::spawn(async move {
            let _permit = permit;
            let result = tokio::time::timeout(
                Duration::from_secs(60),
                fetch_segment_data_uncached(&state, &address),
            )
            .await
            .unwrap_or_else(|_| Err("shared segment fetch timed out".into()));
            sender.send_replace(Some(result));
            let mut fetches = state.cache.segment_fetches.lock().await;
            if fetches
                .get(&address)
                .is_some_and(|r| r.same_channel(&sender.subscribe()))
            {
                fetches.remove(&address);
            }
        });
        receiver
    };
    drop(fetches);
    loop {
        if let Some(result) = receiver.borrow().clone() {
            state
                .metrics
                .record_segment_fetch_latency("cache_miss", started.elapsed());
            return result;
        }
        if receiver.changed().await.is_err() {
            let mut fetches = state.cache.segment_fetches.lock().await;
            if fetches
                .get(segment_address)
                .is_some_and(|r| r.same_channel(&receiver))
            {
                fetches.remove(segment_address);
            }
            return Err("shared segment fetch stopped".into());
        }
    }
}

#[instrument(skip(state), fields(segment_address = %segment_address))]
async fn fetch_segment_data_uncached(
    state: &AppState,
    segment_address: &str,
) -> Result<Bytes, String> {
    let data = state
        .antd
        .data_get_public(segment_address)
        .await
        .map_err(|e| format!("Autonomi fetch failed: {e}"))?;

    let mut segments = state.cache.segments.lock().await;
    segments.insert(segment_address.to_string(), data.clone());

    Ok(data)
}
