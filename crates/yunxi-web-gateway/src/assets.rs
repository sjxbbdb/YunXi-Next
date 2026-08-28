//! Build-time embedded dsh Web distribution.

pub const MAX_WEB_ASSET_BYTES: usize = 1024 * 1024;

pub(crate) struct EmbeddedWebAsset {
    pub(crate) content_type: &'static str,
    pub(crate) cache_control: &'static str,
    pub(crate) body: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
