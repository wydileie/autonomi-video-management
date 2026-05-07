use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct CatalogState {
    pub(crate) catalog_address: Option<String>,
    pub(crate) catalog: Option<Catalog>,
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
    pub(crate) fn index_segments(&mut self) {
        for variant in &mut self.variants {
            variant.index_segments();
        }
    }
}

#[derive(Clone, Deserialize)]
pub(crate) struct VideoVariant {
    pub(crate) resolution: String,
    pub(crate) segment_duration: f64,
    pub(crate) segments: Vec<VideoSegment>,
    #[serde(default, skip)]
    pub(crate) segments_by_index: Vec<Option<String>>,
}

impl VideoVariant {
    pub(crate) fn index_segments(&mut self) {
        self.segments.sort_by_key(|segment| segment.segment_index);
        let Some(max_index) = self
            .segments
            .iter()
            .filter_map(|segment| usize::try_from(segment.segment_index).ok())
            .max()
        else {
            self.segments_by_index.clear();
            return;
        };

        let mut segments_by_index = vec![None; max_index.saturating_add(1)];
        for segment in &self.segments {
            if let Ok(index) = usize::try_from(segment.segment_index) {
                if let Some(slot) = segments_by_index.get_mut(index) {
                    *slot = Some(segment.autonomi_address.clone());
                }
            }
        }
        self.segments_by_index = segments_by_index;
    }

    pub(crate) fn segment_address(&self, segment_index: i32) -> Option<&str> {
        let index = usize::try_from(segment_index).ok()?;
        if !self.segments_by_index.is_empty() {
            return self.segments_by_index.get(index).and_then(Option::as_deref);
        }
        self.segments
            .iter()
            .find(|segment| segment.segment_index == segment_index)
            .map(|segment| segment.autonomi_address.as_str())
    }
}

#[derive(Clone, Deserialize)]
pub(crate) struct VideoSegment {
    pub(crate) segment_index: i32,
    pub(crate) autonomi_address: String,
    pub(crate) duration: f64,
}
