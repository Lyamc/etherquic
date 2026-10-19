//! Rasterize `assets/etherquic.svg` into the Windows icon resource.
//!
//! The SVG is the artwork. On a Windows target, windres embeds the sizes
//! Explorer, the taskbar, and the notification area load from the exe.
//! The same resource file carries `resources/etherquic.manifest.xml`.
//! GPUI imports `TaskDialogIndirect` from comctl32. System32's comctl32 is
//! 5.82 and does not export it; the manifest binds the side-by-side 6.0 copy.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let svg_path = manifest_dir.join("assets").join("etherquic.svg");
    let manifest_path = manifest_dir
        .join("resources")
        .join("etherquic.manifest.xml");
    println!("cargo:rerun-if-changed={}", svg_path.display());
    println!("cargo:rerun-if-changed={}", manifest_path.display());

    let svg = std::fs::read(&svg_path).expect("read etherquic.svg");
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico_path = out_dir.join("etherquic.ico");
    let ico = raster_ico(&svg);
    std::fs::write(&ico_path, &ico).expect("write ico");
    let preview = std::env::temp_dir().join("etherquic-icon-preview.png");
    std::fs::write(&preview, raster_png(&svg, 512)).expect("write preview");
    std::fs::write(
        std::env::temp_dir().join("etherquic-icon-32.png"),
        raster_png(&svg, 32),
    )
    .expect("write 32 preview");
    embed_icon(&ico_path, &manifest_path, &out_dir);
}

fn raster_png(svg: &[u8], px: u32) -> Vec<u8> {
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default())
        .expect("parse etherquic.svg");
    let size = tree.size();
    let scale_x = px as f32 / size.width();
    let scale_y = px as f32 / size.height();
    let mut pixmap = tiny_skia::Pixmap::new(px, px).expect("pixmap");
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale_x, scale_y),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().expect("encode png")
}

fn raster_ico(svg: &[u8]) -> Vec<u8> {
    let sizes = [16u32, 20, 24, 32, 48, 64, 256];
    let images: Vec<(u32, Vec<u8>)> = sizes.iter().map(|px| (*px, raster_png(svg, *px))).collect();
    let count = images.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    let mut offset = 6u32 + u32::from(count) * 16;
    for (size, png) in &images {
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        out.push(dim);
        out.push(dim);
        out.push(0);
        out.push(0);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in &images {
        out.extend_from_slice(png);
    }
    out
}

fn embed_icon(ico: &Path, manifest: &Path, out_dir: &Path) {
    let rc_path = out_dir.join("etherquic.rc");
    let res_path = out_dir.join("etherquic.res");
    let obj_path = out_dir.join("etherquic-icon.o");
    let ico_rc = ico.display().to_string().replace('\\', "/");
    let manifest_rc = manifest.display().to_string().replace('\\', "/");
    // 24 is RT_MANIFEST. Resource id 1 is the one CreateProcess reads.
    std::fs::write(
        &rc_path,
        format!("1 ICON \"{ico_rc}\"\n1 24 \"{manifest_rc}\"\n"),
    )
    .expect("write rc");

    // MinGW windres preprocesses with `gcc -E -xc`, and this gcc drops `-xc`
    // and then fails the `.rc`. The Windows SDK resource compiler writes a
    // `.res`, which windres converts without a preprocessor.
    let rc_exe = find_rc();
    let rc_status = Command::new(&rc_exe)
        .args([
            "/nologo",
            "/fo",
            res_path.to_str().expect("res path"),
            rc_path.to_str().expect("rc path"),
        ])
        .status()
        .unwrap_or_else(|err| panic!("run {}: {err}", rc_exe.display()));
    if !rc_status.success() {
        panic!("rc.exe failed to compile {}", rc_path.display());
    }

    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows-gnu") {
        let windres = find_windres();
        let status = Command::new(&windres)
            .args([
                "-J",
                "res",
                "-O",
                "coff",
                "-i",
                res_path.to_str().expect("res path"),
                "-o",
                obj_path.to_str().expect("obj path"),
            ])
            .status()
            .unwrap_or_else(|err| panic!("run {}: {err}", windres.display()));
        if !status.success() {
            panic!("windres failed to embed {}", ico.display());
        }
        println!("cargo:rustc-link-arg-bins={}", obj_path.display());
    } else {
        // MSVC link.exe takes the .res directly. The installed etherquic.exe is MSVC.
        // The linker's own manifest has no comctl32 6.0 dependency and would
        // replace this one, so TaskDialogIndirect would stay unresolved.
        println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
        println!("cargo:rustc-link-arg-bins={}", res_path.display());
    }
}

fn find_windres() -> PathBuf {
    if let Ok(path) = std::env::var("WINDRES") {
        return PathBuf::from(path);
    }
    let known = PathBuf::from(r"C:\msys64\mingw64\bin\windres.exe");
    if known.exists() {
        return known;
    }
    PathBuf::from("windres")
}

fn find_rc() -> PathBuf {
    if let Ok(path) = std::env::var("RC") {
        return PathBuf::from(path);
    }
    let root = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path().join("x64").join("rc.exe");
            if path.exists() {
                found.push(path);
            }
        }
    }
    found.sort();
    found.pop().unwrap_or_else(|| PathBuf::from("rc.exe"))
}
