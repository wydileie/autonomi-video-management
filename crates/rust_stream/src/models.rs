use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct CatalogState {
    pub(crate) catalog_address: Option<String>,
    pub(crate) published_catalog_address: Option<String>,
    pub(crate) catalog: Option<Catalog>,
    pub(crate) published_catalog: Option<Catalog>,
}

#[derive(Clone, Deserialize)]
pub(crate) struct Catalog {
    pub(crate) videos: Vec<CatalogVideo>,
}

#[derive(Clone, Deserialize)]
pub(crate) struct CatalogVideo {
    pub(crate) id: String,
    pub(crate) manifest_address: String,
}

#[derive(Clone, Deserialize)]
pub(crate) struct VideoManifest {
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) variants: Vec<VideoVariant>,
}

impl VideoManifest {
    pub(crate) fn index_segments(&mut self) -> Result<(), String> {
        if self.id.len() > 128 || self.variants.len() > 16 {
            return Err("manifest exceeds structural limits".into());
        }
        let mut count = 0usize;
        let mut resolutions = std::collections::HashSet::new();
        for variant in &mut self.variants {
            count = count.saturating_add(variant.segments.len());
            if count > 65_536
                || !resolutions.insert(variant.resolution.clone())
                || variant.resolution.is_empty()
                || variant.resolution.len() > 32
                || !variant
                    .resolution
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric())
                || !valid_duration(variant.segment_duration)
            {
                return Err("invalid manifest variant".into());
            }
            variant
                .segments
                .sort_by_key(|segment| segment.segment_index);
            let mut previous = None;
            for segment in &variant.segments {
                if segment.segment_index < 0
                    || segment.segment_index >= 65_536
                    || previous == Some(segment.segment_index)
                    || !valid_duration(segment.duration)
                    || segment.autonomi_address.is_empty()
                    || segment.autonomi_address.len() > 128
                    || !segment
                        .autonomi_address
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    return Err("invalid manifest segment".into());
                }
                previous = Some(segment.segment_index);
            }
        }
        Ok(())
    }
}

fn valid_duration(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value <= 3600.0
}

#[derive(Clone, Deserialize)]
pub(crate) struct VideoVariant {
    pub(crate) resolution: String,
    pub(crate) segment_duration: f64,
    pub(crate) segments: Vec<VideoSegment>,
}

impl VideoVariant {
    pub(crate) fn segment_address(&self, segment_index: i32) -> Option<&str> {
        self.segments
            .binary_search_by_key(&segment_index, |s| s.segment_index)
            .ok()
            .map(|index| self.segments[index].autonomi_address.as_str())
    }
}

#[derive(Clone, Deserialize)]
pub(crate) struct VideoSegment {
    pub(crate) segment_index: i32,
    pub(crate) autonomi_address: String,
    pub(crate) duration: f64,
}
