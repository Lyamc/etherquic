struct Params {
    width: u32,
    height: u32,
    block: u32,
    gw: u32,
    gh: u32,
    mode: u32,
    map_w: u32,
    map_h: u32,
    map_levels: u32,
    _pad0: u32,
    spread: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> src: array<u32>;
@group(0) @binding(2) var<storage, read_write> dst: array<u32>;
@group(0) @binding(3) var<storage, read> lut: array<u32>;
@group(0) @binding(4) var<storage, read> thresh: array<u32>;

fn pack_rgb(r: u32, g: u32, b: u32) -> u32 {
    return (r & 0xFFu) | ((g & 0xFFu) << 8u) | ((b & 0xFFu) << 16u);
}

@compute @workgroup_size(8, 8, 1)
fn look_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let gx = gid.x;
    let gy = gid.y;
    if (gx >= params.gw || gy >= params.gh) {
        return;
    }

    let x0 = gx * params.block;
    let y0 = gy * params.block;
    let bw = min(params.block, params.width - x0);
    let bh = min(params.block, params.height - y0);

    var sr = 0u;
    var sg = 0u;
    var sb = 0u;
    for (var row = 0u; row < bh; row++) {
        let base = (y0 + row) * params.width + x0;
        for (var col = 0u; col < bw; col++) {
            let p = src[base + col];
            sr += p & 0xFFu;
            sg += (p >> 8u) & 0xFFu;
            sb += (p >> 16u) & 0xFFu;
        }
    }
    let n = max(bw * bh, 1u);
    var r = (sr + n / 2u) / n;
    var g = (sg + n / 2u) / n;
    var b = (sb + n / 2u) / n;

    if (params.mode == 1u) {
        r = min((r + 16u) / 32u * 32u, 255u);
        g = min((g + 16u) / 32u * 32u, 255u);
        b = min((b + 16u) / 32u * 32u, 255u);
    } else if (params.mode == 2u) {
        var rf = f32(r);
        var gf = f32(g);
        var bf = f32(b);
        if (params.map_levels > 0u && params.spread > 0.0) {
            let mx = gx % max(params.map_w, 1u);
            let my = gy % max(params.map_h, 1u);
            let t = (f32(thresh[my * params.map_w + mx]) + 0.5)
                / f32(params.map_levels)
                - 0.5;
            let amt = t * params.spread * 255.0;
            rf = clamp(rf + amt, 0.0, 255.0);
            gf = clamp(gf + amt, 0.0, 255.0);
            bf = clamp(bf + amt, 0.0, 255.0);
        }
        let li = (u32(rf) >> 3u) * 1024u + (u32(gf) >> 3u) * 32u + (u32(bf) >> 3u);
        let q = lut[li];
        r = q & 0xFFu;
        g = (q >> 8u) & 0xFFu;
        b = (q >> 16u) & 0xFFu;
    }

    let colour = pack_rgb(r, g, b);
    for (var row = 0u; row < bh; row++) {
        let base = (y0 + row) * params.width + x0;
        for (var col = 0u; col < bw; col++) {
            dst[base + col] = colour;
        }
    }
}
