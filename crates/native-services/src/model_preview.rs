//! 3D model previews and thumbnails.
//!
//! Importing needs assimp, which builds from C++ source with CMake, so the
//! renderer lives behind the `preview-3d` cargo feature. Without it these types
//! stay available, rendering fails with an Unsupported error, and model files
//! keep their file icon instead of a thumbnail.

#[cfg(feature = "preview-3d")]
mod backend;

use std::sync::Arc;

#[cfg(feature = "preview-3d")]
pub use backend::ModelPreviewCache;
#[cfg(feature = "preview-3d")]
pub(crate) use backend::{GEOMETRY_HELPER, serve_geometry_request};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelCamera {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for ModelCamera {
    fn default() -> Self {
        Self {
            yaw: 0.65,
            pitch: -0.35,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ModelFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub struct ModelPreview {
    pub frame: ModelFrame,
    pub format: String,
    pub mesh_count: usize,
    pub vertex_count: usize,
    pub triangle_count: usize,
    pub sampled: bool,
}

/// Stands in for the renderer in builds without the `preview-3d` feature.
#[cfg(not(feature = "preview-3d"))]
#[derive(Clone, Default)]
pub struct ModelPreviewCache {
    _private: (),
}

#[cfg(not(feature = "preview-3d"))]
impl ModelPreviewCache {
    pub fn render(
        &self,
        _path: &std::path::Path,
        _camera: ModelCamera,
        _width: u32,
        _height: u32,
    ) -> crate::ServiceResult<ModelPreview> {
        Err(crate::ServiceError::new(
            crate::ErrorCode::Unsupported,
            "3D previews aren't included in this build",
        ))
    }
}
