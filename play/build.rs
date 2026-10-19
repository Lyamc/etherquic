//! Rasterize `ui/assets/etherquic-player.svg` for the picture window icon.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let svg_path = manifest_dir
        .join("..")
        .join("ui")
        .join("assets")
        .join("etherquic-player.svg");
    println!("cargo:rerun-if-changed={}", svg_path.display());
    let svg = std::fs::read(&svg_path).expect("read etherquic-player.svg");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(out_dir.join("player-256.rgba"), raster_rgba(&svg, 256))
        .expect("write player icon");
    std::fs::write(out_dir.join("player-256.png"), raster_png(&svg, 256)).expect("write player preview");
}

fn raster_png(svg: &[u8], px: u32) -> Vec<u8> {
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default())
        .expect("parse etherquic-player.svg");
    let size = tree.size();
    let mut pixmap = tiny_skia::Pixmap::new(px, px).expect("pixmap");
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(px as f32 / size.width(), px as f32 / size.height()),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().expect("encode png")
}

fn raster_rgba(svg: &[u8], px: u32) -> Vec<u8> {
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default())
        .expect("parse etherquic-player.svg");
    let size = tree.size();
    let mut pixmap = tiny_skia::Pixmap::new(px, px).expect("pixmap");
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(px as f32 / size.width(), px as f32 / size.height()),
        &mut pixmap.as_mut(),
    );
    let data = pixmap.data();
    let mut out = Vec::with_capacity(8 + data.len());
    out.extend_from_slice(&px.to_le_bytes());
    out.extend_from_slice(&px.to_le_bytes());
    out.extend_from_slice(data);
    out
}
