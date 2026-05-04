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

#[derive(Clone, Deserialize)]
pub(crate) struct VideoVariant {
    pub(crate) resolution: String,
    pub(crate) segment_duration: f64,
    pub(crate) segments: Vec<VideoSegment>,
}

#[derive(Clone, Deserialize)]
pub(crate) struct VideoSegment {
    pub(crate) segment_index: i32,
    pub(crate) autonomi_address: String,
    pub(crate) duration: f64,
}
