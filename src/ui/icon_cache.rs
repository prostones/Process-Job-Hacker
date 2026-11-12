//! 进程图标纹理缓存。
//!
//! 图标提取要走 Shell API，首次调用有磁盘和 Shell 初始化开销，因此这里按
//! exe 路径缓存纹理，并限制每帧最多加载的图标数量，避免首次滚动时卡顿。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, TextureHandle};

use crate::win::icon::load_process_icon;

/// 名称列图标的逻辑显示尺寸。
pub const ICON_SIZE: f32 = 16.0;

/// 每帧最多加载的图标数量，避免一次渲染大量新行时阻塞 UI 线程。
const MAX_ICON_LOADS_PER_FRAME: usize = 2;

/// 按 exe 路径缓存图标纹理；失败结果也会缓存，避免重复调用 Shell API。
#[derive(Default)]
pub struct ProcessIconCache {
    textures: HashMap<PathBuf, Option<TextureHandle>>,
    loads_this_frame: usize,
}

impl ProcessIconCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 每帧开始时重置加载预算。
    pub fn begin_frame(&mut self) {
        self.loads_this_frame = 0;
    }

    /// 返回 exe 路径对应的图标纹理。
    ///
    /// 未命中时按帧预算加载；加载失败会缓存 `None`，后续帧直接返回 `None`。
    pub fn texture_for(
        &mut self,
        ctx: &egui::Context,
        path: Option<&Path>,
    ) -> Option<&TextureHandle> {
        let path = path?;
        if !self.textures.contains_key(path) {
            if self.loads_this_frame >= MAX_ICON_LOADS_PER_FRAME {
                return None;
            }

            self.loads_this_frame += 1;
            let texture = load_process_icon(path).map(|icon| {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [icon.width as usize, icon.height as usize],
                    &icon.rgba,
                );
                ctx.load_texture(
                    format!("process-icon:{}", path.display()),
                    image,
                    egui::TextureOptions::LINEAR,
                )
            });
            self.textures.insert(path.to_path_buf(), texture);
        }
        self.textures.get(path).and_then(Option::as_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_path_is_cached_and_not_retried() {
        let ctx = egui::Context::default();
        let mut cache = ProcessIconCache::new();
        let path = Path::new(r"C:\definitely-missing-pjh-icon.exe");

        cache.begin_frame();
        assert!(cache.texture_for(&ctx, Some(path)).is_none());
        assert_eq!(cache.textures.len(), 1);
        assert_eq!(cache.loads_this_frame, 1);

        // 同一帧第二次：命中缓存的 None，不再消耗预算。
        assert!(cache.texture_for(&ctx, Some(path)).is_none());
        assert_eq!(cache.loads_this_frame, 1);

        // 新的一帧也不会重试失败路径。
        cache.begin_frame();
        assert!(cache.texture_for(&ctx, Some(path)).is_none());
        assert_eq!(cache.textures.len(), 1);
        assert_eq!(cache.loads_this_frame, 0);
    }

    #[test]
    fn begin_frame_resets_load_budget() {
        let mut cache = ProcessIconCache::new();
        cache.loads_this_frame = MAX_ICON_LOADS_PER_FRAME;
        cache.begin_frame();
        assert_eq!(cache.loads_this_frame, 0);
    }

    #[test]
    fn missing_path_returns_none_without_loading() {
        let ctx = egui::Context::default();
        let mut cache = ProcessIconCache::new();
        cache.begin_frame();
        assert!(cache.texture_for(&ctx, None).is_none());
        assert_eq!(cache.loads_this_frame, 0);
    }
}
