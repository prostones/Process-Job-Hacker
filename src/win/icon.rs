//! 从可执行文件提取图标，供进程树名称列展示。
//!
//! 图标来源是 Windows Shell 的 `SHGetFileInfoW`，拿到 `HICON` 后通过 GDI
//! 转成自上而下的 RGBA 像素，交给 egui 上传为纹理。所有句柄都在本模块内
//! 成对释放，调用方只需要持有返回的像素数据。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, ReleaseDC,
};
use windows_sys::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

/// 提取出的图标位图，像素顺序为自上而下、RGBA。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessIcon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 按路径提取 exe 图标。
///
/// 文件不存在、没有图标或权限不足时返回 `None`，调用方应缓存失败结果，
/// 避免对同一个路径反复调用 Shell API。
pub fn load_process_icon(path: &Path) -> Option<ProcessIcon> {
    let wide: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut file_info = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut file_info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if ok == 0 || file_info.hIcon.is_null() {
        return None;
    }

    let icon = unsafe { hicon_to_rgba(file_info.hIcon) };
    unsafe { DestroyIcon(file_info.hIcon) };
    icon
}

/// 把 `HICON` 转成 RGBA，并释放 `GetIconInfo` 产生的位图句柄。
unsafe fn hicon_to_rgba(hicon: HICON) -> Option<ProcessIcon> {
    let mut icon_info = ICONINFO::default();
    if unsafe { GetIconInfo(hicon, &mut icon_info) } == 0 {
        return None;
    }

    let result = unsafe { color_bitmap_to_rgba(icon_info.hbmColor) };
    if !icon_info.hbmColor.is_null() {
        unsafe { DeleteObject(icon_info.hbmColor as _) };
    }
    if !icon_info.hbmMask.is_null() {
        unsafe { DeleteObject(icon_info.hbmMask as _) };
    }
    result
}

/// 把图标的颜色位图转成自上而下的 RGBA。
unsafe fn color_bitmap_to_rgba(bitmap_handle: HBITMAP) -> Option<ProcessIcon> {
    if bitmap_handle.is_null() {
        return None;
    }

    let mut bitmap = BITMAP::default();
    let copied = unsafe {
        GetObjectW(
            bitmap_handle as _,
            std::mem::size_of::<BITMAP>() as i32,
            &mut bitmap as *mut _ as _,
        )
    };
    if copied == 0 {
        return None;
    }

    let width = bitmap.bmWidth.max(0) as u32;
    let height = bitmap.bmHeight.max(0) as u32;
    if width == 0 || height == 0 {
        return None;
    }

    let mut info = BITMAPINFO::default();
    info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = width as i32;
    // 负高度表示自上而下，与 egui ColorImage 的行顺序一致。
    info.bmiHeader.biHeight = -(height as i32);
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;

    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let hdc = unsafe { GetDC(std::ptr::null_mut()) };
    if hdc.is_null() {
        return None;
    }
    let lines = unsafe {
        GetDIBits(
            hdc,
            bitmap_handle,
            0,
            height,
            pixels.as_mut_ptr() as _,
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    unsafe { ReleaseDC(std::ptr::null_mut(), hdc) };
    if lines == 0 {
        return None;
    }

    // GDI 返回 BGRA，egui 需要 RGBA。
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    // 旧式无 alpha 图标会整幅 alpha 为 0，兜底为不透明，避免整张图透明消失。
    if pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0) {
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
    }

    Some(ProcessIcon {
        width,
        height,
        rgba: pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_returns_none() {
        assert!(load_process_icon(Path::new(r"C:\definitely-missing-pjh-icon.exe")).is_none());
    }

    #[test]
    fn extracts_icon_from_notepad() {
        let path = Path::new(r"C:\Windows\System32\notepad.exe");
        if !path.exists() {
            return;
        }
        let icon = load_process_icon(path).expect("notepad icon");
        assert!(icon.width > 0);
        assert!(icon.height > 0);
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
        assert!(
            icon.rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] > 0)
        );
    }
}
