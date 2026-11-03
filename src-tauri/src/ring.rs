// 右下角三段圓環圖示：綠 Codex／橘 Claude／紫 Antigravity。
// 每段亮起的長度＝該平台最緊額度的剩餘 %；沒有資料的段只畫灰色底。
pub const SIZE: u32 = 32;

const COLORS: [[u8; 3]; 3] = [[0x16, 0xA3, 0x4A], [0xEA, 0x8A, 0x0C], [0x7C, 0x3A, 0xED]];
const TRACK: [u8; 3] = [0x9A, 0xA3, 0xAE];
const OUTER: f64 = 15.0;
const INNER: f64 = 9.5;
const GAP_DEG: f64 = 14.0;
const SS: u32 = 4; // 超取樣倍數（反鋸齒）

/// lefts：三個平台的剩餘 %（0～100），None＝無資料。回傳 RGBA。
pub fn draw(lefts: [Option<f64>; 3]) -> Vec<u8> {
    let mut buf = vec![0u8; (SIZE * SIZE * 4) as usize];
    let c = SIZE as f64 / 2.0;
    let seg_span = 120.0 - GAP_DEG;
    for py in 0..SIZE {
        for px in 0..SIZE {
            let (mut r, mut g, mut b, mut hits) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = px as f64 + (sx as f64 + 0.5) / SS as f64 - c;
                    let y = py as f64 + (sy as f64 + 0.5) / SS as f64 - c;
                    let d = (x * x + y * y).sqrt();
                    if !(INNER..=OUTER).contains(&d) {
                        continue;
                    }
                    // 角度：從正上方開始順時針 0～360
                    let ang = (x.atan2(-y).to_degrees() + 360.0) % 360.0;
                    let seg = (ang / 120.0) as usize % 3;
                    let within = ang - seg as f64 * 120.0 - GAP_DEG / 2.0;
                    if within < 0.0 || within > seg_span {
                        continue;
                    }
                    let lit = lefts[seg].is_some_and(|left| within <= seg_span * left.clamp(0.0, 100.0) / 100.0);
                    let col = if lit { COLORS[seg] } else { TRACK };
                    r += col[0] as u32;
                    g += col[1] as u32;
                    b += col[2] as u32;
                    hits += 1;
                }
            }
            if hits > 0 {
                let i = ((py * SIZE + px) * 4) as usize;
                buf[i] = (r / hits) as u8;
                buf[i + 1] = (g / hits) as u8;
                buf[i + 2] = (b / hits) as u8;
                buf[i + 3] = (hits * 255 / (SS * SS)) as u8;
            }
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    /// 手動檢查用：cargo test dump_preview -- --ignored，輸出原始 RGBA 到 AITOKEN_RING_DUMP 指定的資料夾
    #[test]
    #[ignore]
    fn dump_preview() {
        let dir = std::env::var("AITOKEN_RING_DUMP").expect("AITOKEN_RING_DUMP");
        let cases: [(&str, [Option<f64>; 3]); 3] = [
            ("now", [Some(7.0), None, None]),
            ("full", [Some(100.0), Some(100.0), Some(100.0)]),
            ("mixed", [Some(94.0), Some(18.0), Some(42.0)]),
        ];
        for (name, l) in cases {
            std::fs::write(format!("{dir}/ring-{name}.rgba"), draw(l)).unwrap();
        }
    }

    #[test]
    fn size_and_transparent_center() {
        let b = draw([Some(50.0), None, None]);
        assert_eq!(b.len(), (SIZE * SIZE * 4) as usize);
        assert_eq!(pixel(&b, 16, 16)[3], 0);
    }

    #[test]
    fn full_codex_segment_is_green_empty_is_grey() {
        // 右上方（Codex 段中間附近）
        let full = draw([Some(100.0), None, None]);
        let p = pixel(&full, 26, 9);
        assert!(p[1] > p[0] && p[1] > p[2], "應為綠色 {p:?}");
        let empty = draw([Some(0.0), None, None]);
        let q = pixel(&empty, 26, 9);
        assert_eq!([q[0], q[1], q[2]], TRACK);
    }
}
