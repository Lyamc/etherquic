//! Vulkan / Metal / DX12 compute for ordered looks. CPU adapters are skipped.

use super::dither::{self, Dither};
use super::{lut, Look, Style};

const MIN_PIXELS: u32 = 320 * 180;
const PARAMS_SIZE: u64 = 64;
const LUT_WORDS: u64 = lut::LEN as u64;
const MAP_WORDS: u64 = 64 * 64;

enum State {
    Off,
    Starting,
    Ready(Gpu),
    Failed,
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    lut_buf: wgpu::Buffer,
    map_buf: wgpu::Buffer,
    src: wgpu::Buffer,
    dst: wgpu::Buffer,
    staging: wgpu::Buffer,
    pixels: u32,
    packed: Vec<u32>,
    upload: Vec<u8>,
    lut_look: Option<Look>,
    map_dither: Option<Dither>,
    slow: bool,
}

fn state() -> &'static std::sync::Mutex<State> {
    use std::sync::{Mutex, OnceLock};
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::Off))
}

/// Start adapter + LUT work off the encode path. Safe to call many times.
pub fn warm() {
    lut::warm();
    let Ok(mut guard) = state().lock() else {
        return;
    };
    if !matches!(*guard, State::Off) {
        return;
    }
    *guard = State::Starting;
    drop(guard);
    std::thread::Builder::new()
        .name("etherquic-look-gpu".into())
        .spawn(|| {
            let next = match Gpu::new() {
                Some(gpu) => State::Ready(gpu),
                None => State::Failed,
            };
            if let Ok(mut guard) = state().lock() {
                *guard = next;
            }
        })
        .ok();
}

pub fn try_apply(rgb: &mut [u8], width: u32, height: u32, style: Style) -> bool {
    if cfg!(test) {
        return false;
    }
    if style.dither.is_diffusion() {
        return false;
    }
    if width.saturating_mul(height) < MIN_PIXELS {
        return false;
    }
    let Ok(mut guard) = state().lock() else {
        return false;
    };
    match &mut *guard {
        State::Off => {
            drop(guard);
            warm();
            false
        }
        State::Starting | State::Failed => false,
        State::Ready(gpu) => gpu.apply(rgb, width, height, style),
    }
}

impl Gpu {
    fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN
                | wgpu::Backends::METAL
                | wgpu::Backends::DX12,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return None;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("etherquic-look"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .ok()?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("etherquic-look-wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("etherquic-look-bgl"),
            entries: &[
                bind(0, wgpu::BufferBindingType::Uniform),
                bind(1, wgpu::BufferBindingType::Storage { read_only: true }),
                bind(2, wgpu::BufferBindingType::Storage { read_only: false }),
                bind(3, wgpu::BufferBindingType::Storage { read_only: true }),
                bind(4, wgpu::BufferBindingType::Storage { read_only: true }),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("etherquic-look-pl"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("etherquic-look-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("look_main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let params = buffer(
            &device,
            PARAMS_SIZE,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let lut_buf = buffer(
            &device,
            LUT_WORDS * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let map_buf = buffer(
            &device,
            MAP_WORDS * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let pixels = 64 * 64;
        let (src, dst, staging) = pixel_buffers(&device, pixels);
        Some(Self {
            device,
            queue,
            pipeline,
            layout,
            params,
            lut_buf,
            map_buf,
            src,
            dst,
            staging,
            pixels,
            packed: Vec::new(),
            upload: Vec::new(),
            lut_look: None,
            map_dither: None,
            slow: false,
        })
    }

    fn apply(&mut self, rgb: &mut [u8], width: u32, height: u32, style: Style) -> bool {
        if self.slow {
            return false;
        }
        let pixels = width.saturating_mul(height);
        if pixels == 0 {
            return false;
        }
        let started = std::time::Instant::now();
        if pixels > self.pixels {
            let (src, dst, staging) = pixel_buffers(&self.device, pixels);
            self.src = src;
            self.dst = dst;
            self.staging = staging;
            self.pixels = pixels;
        }
        let block = style.pixel_size();
        let gw = width.div_ceil(block);
        let gh = height.div_ceil(block);
        let mode = match style.look {
            Look::Blocks => 0u32,
            Look::Poster => 1,
            _ => 2,
        };
        if mode == 2 {
            let Some(packed) = lut::packed(style.look) else {
                return false;
            };
            if self.lut_look != Some(style.look) {
                self.queue
                    .write_buffer(&self.lut_buf, 0, u32_bytes(packed).as_slice());
                self.lut_look = Some(style.look);
            }
        } else if self.lut_look.is_none() {
            // Storage buffers cannot be empty; keep a dummy table bound.
            let dummy = vec![0u32; lut::LEN];
            self.queue
                .write_buffer(&self.lut_buf, 0, u32_bytes(&dummy).as_slice());
            self.lut_look = Some(Look::Off);
        }
        let map = dither::threshold_map(style.dither);
        let (map_w, map_h, map_levels) = match &map {
            Some(map) => (map.w as u32, map.h as u32, map.levels),
            None => (1, 1, 0),
        };
        if self.map_dither != Some(style.dither) {
            let mut words = vec![0u32; MAP_WORDS as usize];
            if let Some(map) = &map {
                for (i, value) in map.values.iter().enumerate().take(words.len()) {
                    words[i] = u32::from(*value);
                }
            }
            self.queue
                .write_buffer(&self.map_buf, 0, u32_bytes(&words).as_slice());
            self.map_dither = Some(style.dither);
        }
        let spread = if mode == 2 {
            let palette = style.look.palette();
            super::palette_spread(palette) * style.strength01()
        } else {
            0.0
        };
        let mut params = [0u8; PARAMS_SIZE as usize];
        put_u32(&mut params, 0, width);
        put_u32(&mut params, 4, height);
        put_u32(&mut params, 8, block);
        put_u32(&mut params, 12, gw);
        put_u32(&mut params, 16, gh);
        put_u32(&mut params, 20, mode);
        put_u32(&mut params, 24, map_w);
        put_u32(&mut params, 28, map_h);
        put_u32(&mut params, 32, map_levels);
        put_f32(&mut params, 40, spread);
        self.queue.write_buffer(&self.params, 0, &params);

        self.packed.resize(pixels as usize, 0);
        for (i, colour) in rgb.chunks_exact(3).take(pixels as usize).enumerate() {
            self.packed[i] = lut::pack([colour[0], colour[1], colour[2]]);
        }
        fill_u32_bytes(&self.packed, &mut self.upload);
        self.queue.write_buffer(&self.src, 0, &self.upload);

        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("etherquic-look-bg"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.src.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.dst.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.lut_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.map_buf.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("etherquic-look-enc"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("etherquic-look-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(gw.div_ceil(8), gh.div_ceil(8), 1);
        }
        encoder.copy_buffer_to_buffer(
            &self.dst,
            0,
            &self.staging,
            0,
            u64::from(pixels) * 4,
        );
        self.queue.submit(Some(encoder.finish()));
        let slice = self.staging.slice(..u64::from(pixels) * 4);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device.poll(wgpu::Maintain::Wait);
        if rx.recv().ok().and_then(|r| r.ok()).is_none() {
            self.staging.unmap();
            return false;
        }
        {
            let data = slice.get_mapped_range();
            for (i, colour) in rgb.chunks_exact_mut(3).take(pixels as usize).enumerate() {
                let packed = u32::from_le_bytes([
                    data[i * 4],
                    data[i * 4 + 1],
                    data[i * 4 + 2],
                    data[i * 4 + 3],
                ]);
                let [r, g, b] = lut::unpack(packed);
                colour[0] = r;
                colour[1] = g;
                colour[2] = b;
            }
        }
        self.staging.unmap();
        // Pack + submit + map is extra copies. If that hitch is visible,
        // keep the integer LUT on the CPU for the rest of the session.
        if started.elapsed() >= std::time::Duration::from_millis(8) {
            self.slow = true;
        }
        true
    }
}

fn bind(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn buffer(device: &wgpu::Device, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: size.max(4),
        usage,
        mapped_at_creation: false,
    })
}

fn pixel_buffers(
    device: &wgpu::Device,
    pixels: u32,
) -> (wgpu::Buffer, wgpu::Buffer, wgpu::Buffer) {
    let bytes = u64::from(pixels.max(1)) * 4;
    let src = buffer(
        device,
        bytes,
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    );
    let dst = buffer(
        device,
        bytes,
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    );
    let staging = buffer(
        device,
        bytes,
        wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
    );
    (src, dst, staging)
}

fn put_u32(buf: &mut [u8], off: usize, value: u32) {
    buf[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_f32(buf: &mut [u8], off: usize, value: f32) {
    buf[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

fn u32_bytes(words: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    fill_u32_bytes(words, &mut out);
    out
}

fn fill_u32_bytes(words: &[u32], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(words.len() * 4);
    for word in words {
        out.extend_from_slice(&word.to_le_bytes());
    }
}
