struct Params {
    width: u32,
    height: u32,
    t_q8: u32,
    _pad: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> prev_px: array<u32>;
@group(0) @binding(2) var<storage, read> curr_px: array<u32>;
@group(0) @binding(3) var<storage, read_write> flow: array<u32>;
@group(0) @binding(4) var<storage, read_write> out_px: array<u32>;

const BLOCK: i32 = 16;
const RADIUS: i32 = 8;

fn unpack(p: u32) -> vec3<u32> {
    return vec3<u32>((p >> 16u) & 255u, (p >> 8u) & 255u, p & 255u);
}

fn pack(c: vec3<u32>) -> u32 {
    return (c.x << 16u) | (c.y << 8u) | c.z;
}

fn luma_word(p: u32) -> i32 {
    let c = unpack(p);
    return i32((77u * c.x + 150u * c.y + 29u * c.z) >> 8u);
}

fn idx(w: i32, h: i32, x: i32, y: i32) -> u32 {
    let sx = clamp(x, 0, w - 1);
    let sy = clamp(y, 0, h - 1);
    return u32(sy * w + sx);
}

@compute @workgroup_size(8, 8)
fn motion_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = i32(params.width);
    let h = i32(params.height);
    let bw = (w + BLOCK - 1) / BLOCK;
    let bh = (h + BLOCK - 1) / BLOCK;
    let bx = i32(id.x);
    let by = i32(id.y);
    if bx >= bw || by >= bh {
        return;
    }
    let ox = bx * BLOCK;
    let oy = by * BLOCK;
    var best = 0x7fffffff;
    var mx = 0;
    var my = 0;
    for (var dy = -RADIUS; dy <= RADIUS; dy++) {
        for (var dx = -RADIUS; dx <= RADIUS; dx++) {
            var sad = 0;
            var y = oy;
            while y < oy + BLOCK && y < h {
                var x = ox;
                while x < ox + BLOCK && x < w {
                    let a = luma_word(curr_px[idx(w, h, x, y)]);
                    let b = luma_word(prev_px[idx(w, h, x - dx, y - dy)]);
                    sad += abs(a - b);
                    x += 2;
                }
                y += 2;
            }
            if sad < best {
                best = sad;
                mx = dx;
                my = dy;
            }
        }
    }
    flow[u32(by * bw + bx)] = ((u32(my + 128) & 255u) << 8u) | (u32(mx + 128) & 255u);
}

@compute @workgroup_size(8, 8)
fn warp_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = i32(params.width);
    let h = i32(params.height);
    let x = i32(id.x);
    let y = i32(id.y);
    if x >= w || y >= h {
        return;
    }
    let bw = (w + BLOCK - 1) / BLOCK;
    let bh = (h + BLOCK - 1) / BLOCK;
    let bx = min(x / BLOCK, bw - 1);
    let by = min(y / BLOCK, bh - 1);
    let packed = flow[u32(by * bw + bx)];
    let mx = i32(packed & 255u) - 128;
    let my = i32((packed >> 8u) & 255u) - 128;
    let extra = i32(params.t_q8) - 256;
    let sx = x + ((mx * extra) >> 8);
    let sy = y + ((my * extra) >> 8);
    out_px[u32(y * w + x)] = curr_px[idx(w, h, sx, sy)];
}
