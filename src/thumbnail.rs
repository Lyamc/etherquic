//! The discovery picture: box-filter downscale, and JPEG XL through libjxl's
//! `cjxl` tool (BSD-3-Clause), searched to fit a byte budget.
//!
//! The tool is found through `ETHERQUIC_CJXL`, next to this program, or on
//! `PATH`. Without it an offer carries no picture. Shared by the desktop
//! player's host and by the Windows service host.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Widest thumbnail tried first. Smaller sizes follow when it cannot fit.
pub const THUMB_WIDTHS: [u32; 6] = [192, 160, 128, 96, 72, 48];
/// Butteraugli distance range for the search. 25 is the modular maximum.
pub const DISTANCE_MIN: f32 = 0.8;
pub const DISTANCE_MAX: f32 = 25.0;
const SEARCH_STEPS: u32 = 7;

/// RGB8 picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Box-filter `rgba` down to `width` (aspect kept). Never upscales.
pub fn downscale(rgba: &[u8], src_w: u32, src_h: u32, width: u32) -> Rgb {
    let width = width.clamp(1, src_w.max(1));
    let height = ((src_h as u64 * width as u64 + src_w as u64 / 2) / src_w.max(1) as u64)
        .max(1) as u32;
    let mut data = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        let y0 = (y as u64 * src_h as u64 / height as u64) as u32;
        let y1 = (((y + 1) as u64 * src_h as u64 / height as u64) as u32).max(y0 + 1);
        for x in 0..width {
            let x0 = (x as u64 * src_w as u64 / width as u64) as u32;
            let x1 = (((x + 1) as u64 * src_w as u64 / width as u64) as u32).max(x0 + 1);
            let mut sum = [0u64; 3];
            let mut n = 0u64;
            for sy in y0..y1.min(src_h) {
                for sx in x0..x1.min(src_w) {
                    let at = ((sy * src_w + sx) * 4) as usize;
                    if let Some(px) = rgba.get(at..at + 3) {
                        sum[0] += px[0] as u64;
                        sum[1] += px[1] as u64;
                        sum[2] += px[2] as u64;
                        n += 1;
                    }
                }
            }
            let n = n.max(1);
            data.extend([(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]);
        }
    }
    Rgb {
        width,
        height,
        data,
    }
}

/// The best picture no longer than `budget`: the widest size that fits,
/// at the lowest distance that still fits. `encode(width, distance)` returns
/// the codestream for the picture scaled to `width`.
pub fn fit_to_budget(
    budget: usize,
    widths: &[u32],
    mut encode: impl FnMut(u32, f32) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    for &width in widths {
        let Some(worst) = encode(width, DISTANCE_MAX) else {
            continue;
        };
        if worst.len() > budget {
            continue;
        }
        let mut best = worst;
        let (mut lo, mut hi) = (DISTANCE_MIN, DISTANCE_MAX);
        for _ in 0..SEARCH_STEPS {
            let mid = (lo + hi) / 2.0;
            match encode(width, mid) {
                Some(bytes) if bytes.len() <= budget => {
                    best = bytes;
                    hi = mid;
                }
                _ => lo = mid,
            }
        }
        return Some(best);
    }
    None
}

/// `cjxl` to use: `ETHERQUIC_CJXL`, then next to this program, then `PATH`.
pub fn cjxl_path() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "cjxl.exe" } else { "cjxl" };
    if let Some(path) = std::env::var_os("ETHERQUIC_CJXL") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf))
    {
        let path = dir.join(exe);
        if path.is_file() {
            return Some(path);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(exe))
            .find(|path| path.is_file())
    })
}

static SCRATCH: AtomicU64 = AtomicU64::new(0);

/// One modular, progressive encode at `distance`, through a binary PPM.
pub fn encode_cjxl(cjxl: &Path, picture: &Rgb, distance: f32) -> Option<Vec<u8>> {
    let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("etherquic-thumb-{}-{n}", std::process::id()));
    let input = base.with_extension("ppm");
    let output = base.with_extension("jxl");
    let mut ppm = format!("P6\n{} {}\n255\n", picture.width, picture.height).into_bytes();
    ppm.extend(&picture.data);
    std::fs::write(&input, ppm).ok()?;
    let status = std::process::Command::new(cjxl)
        .arg(&input)
        .arg(&output)
        .args(["--modular=1", "--progressive", "--container=0", "--effort=7", "--quiet"])
        .arg(format!("--distance={distance:.3}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let bytes = match status {
        Ok(status) if status.success() => std::fs::read(&output).ok(),
        _ => None,
    };
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    bytes
}

