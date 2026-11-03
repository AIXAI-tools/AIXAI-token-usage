// 擷取「本程式面板視窗自己」的畫面（以 HWND 呼叫 PrintWindow）。
// 絕不擷取螢幕：只對傳進來的視窗控制代碼取圖，其他視窗與桌面都不會被讀到。
// 手寫 Win32 宣告以避免多引入大型依賴。

/// 把 RGBA 像素編成 PNG
pub fn encode_png(rgba: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(rgba).ok()?;
    }
    Some(out)
}

/// 畫面是否整張同一個顏色（擷取失敗的空白圖）
pub fn is_blank(rgba: &[u8]) -> bool {
    rgba.len() < 8 || rgba.chunks_exact(4).all(|p| p == &rgba[0..4])
}

#[cfg(windows)]
pub fn capture_window_png(hwnd: isize) -> Option<Vec<u8>> {
    use std::ffi::c_void;
    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[repr(C)]
    struct BitmapInfoHeader {
        size: u32,
        width: i32,
        height: i32,
        planes: u16,
        bit_count: u16,
        compression: u32,
        size_image: u32,
        xppm: i32,
        yppm: i32,
        clr_used: u32,
        clr_important: u32,
    }
    #[repr(C)]
    struct BitmapInfo {
        header: BitmapInfoHeader,
        colors: [u32; 1],
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetWindowRect(h: isize, r: *mut Rect) -> i32;
        fn GetDC(h: isize) -> isize;
        fn ReleaseDC(h: isize, dc: isize) -> i32;
        fn PrintWindow(h: isize, dc: isize, flags: u32) -> i32;
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn CreateCompatibleDC(dc: isize) -> isize;
        fn CreateCompatibleBitmap(dc: isize, w: i32, h: i32) -> isize;
        fn SelectObject(dc: isize, obj: isize) -> isize;
        fn GetDIBits(dc: isize, bmp: isize, start: u32, lines: u32, bits: *mut c_void, bi: *mut BitmapInfo, usage: u32) -> i32;
        fn DeleteObject(obj: isize) -> i32;
        fn DeleteDC(dc: isize) -> i32;
    }
    unsafe {
        let mut r = Rect { left: 0, top: 0, right: 0, bottom: 0 };
        if GetWindowRect(hwnd, &mut r) == 0 {
            return None;
        }
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w <= 0 || h <= 0 || w > 4000 || h > 4000 {
            return None;
        }
        let wdc = GetDC(hwnd);
        if wdc == 0 {
            return None;
        }
        let mdc = CreateCompatibleDC(wdc);
        let bmp = CreateCompatibleBitmap(wdc, w, h);
        let mut result = None;
        if mdc != 0 && bmp != 0 {
            let old = SelectObject(mdc, bmp);
            // PW_RENDERFULLCONTENT = 2：讓 WebView2 的內容也被畫進來
            if PrintWindow(hwnd, mdc, 2) != 0 {
                let mut bi = BitmapInfo {
                    header: BitmapInfoHeader { size: std::mem::size_of::<BitmapInfoHeader>() as u32, width: w, height: -h, planes: 1, bit_count: 32, compression: 0, size_image: 0, xppm: 0, yppm: 0, clr_used: 0, clr_important: 0 },
                    colors: [0],
                };
                let mut buf = vec![0u8; (w * h * 4) as usize];
                // 先把 bmp 選出 DC 再取像素
                SelectObject(mdc, old);
                let got = GetDIBits(mdc, bmp, 0, h as u32, buf.as_mut_ptr() as *mut c_void, &mut bi, 0);
                if got > 0 {
                    // BGRA → RGBA，alpha 一律不透明
                    for p in buf.chunks_exact_mut(4) {
                        p.swap(0, 2);
                        p[3] = 255;
                    }
                    if !is_blank(&buf) {
                        result = encode_png(&buf, w as u32, h as u32);
                    }
                }
            }
        }
        if bmp != 0 {
            DeleteObject(bmp);
        }
        if mdc != 0 {
            DeleteDC(mdc);
        }
        ReleaseDC(hwnd, wdc);
        result
    }
}

#[cfg(not(windows))]
pub fn capture_window_png(_hwnd: isize) -> Option<Vec<u8>> {
    None
}

/// Windows 版本字串（例如 "Windows 11 (10.0.26300)"）
#[cfg(windows)]
pub fn windows_version() -> String {
    #[repr(C)]
    struct OsVersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
    }
    let mut v = OsVersionInfo { size: std::mem::size_of::<OsVersionInfo>() as u32, major: 0, minor: 0, build: 0, platform: 0, csd: [0; 128] };
    if unsafe { RtlGetVersion(&mut v) } != 0 {
        return "Windows (unknown)".into();
    }
    let name = if v.major == 10 && v.build >= 22000 { "Windows 11" } else if v.major == 10 { "Windows 10" } else { "Windows" };
    format!("{name} ({}.{}.{})", v.major, v.minor, v.build)
}

#[cfg(not(windows))]
pub fn windows_version() -> String {
    "non-Windows".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_detection() {
        assert!(is_blank(&[0, 0, 0, 255].repeat(100)));
        let mut img = [10, 20, 30, 255].repeat(100);
        img[40] = 99;
        assert!(!is_blank(&img));
    }

    #[test]
    fn png_has_signature() {
        let img = [1u8, 2, 3, 255].repeat(4);
        let p = encode_png(&img, 2, 2).unwrap();
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
    }
}
