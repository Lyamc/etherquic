//! Vulkan / Metal / DX12 compute for client frame generation.

use super::BLOCK;

const MIN_PIXELS: u32 = 160 * 90;
const PARAMS_SIZE: u64 = 16;

enum State {
    Off,
    Starting,
    Ready(Gpu),
    Failed,
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    motion: wgpu::ComputePipeline,
    warp: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    prev: wgpu::Buffer,
    curr: wgpu::Buffer,
    flow: wgpu::Buffer,
    dst: wgpu::Buffer,
    staging: wgpu::Buffer,
    pixels: u32,
    blocks: u32,
    packed: Vec<u32>,
    upload: Vec<u8>,
    slow: bool,
}

fn state() -> &'static std::sync::Mutex<State> {
    use std::sync::{Mutex, OnceLock};
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::Off))
}

pub fn warm() {
    let Ok(mut guard) = state().lock() else {
        return;
    };
    if !matches!(*guard, State::Off) {
        return;
    }
    *guard = State::Starting;
    drop(guard);
    std::thread::Builder::new()
        .name("etherquic-fruc-gpu".into())
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

pub fn try_interpolate(
    prev: &[u8],
    curr: &[u8],
    width: u32,
    height: u32,
    t_q8: u32,
    out: &mut [u8],
) -> bool {
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
        State::Ready(gpu) => gpu.apply(prev, curr, width, height, t_q8, out),
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
                label: Some("etherquic-fruc"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .ok()?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("etherquic-fruc-wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("etherquic-fruc-bgl"),
            entries: &[
                bind(0, wgpu::BufferBindingType::Uniform),
                bind(1, wgpu::BufferBindingType::Storage { read_only: true }),
                bind(2, wgpu::BufferBindingType::Storage { read_only: true }),
                bind(3, wgpu::BufferBindingType::Storage { read_only: false }),
                bind(4, wgpu::BufferBindingType::Storage { read_only: false }),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("etherquic-fruc-pl"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let motion = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("etherquic-fruc-motion"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("motion_main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let warp = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("etherquic-fruc-warp"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("warp_main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let params = buffer(
            &device,
            PARAMS_SIZE,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let pixels = 64 * 64;
        let blocks = 16;
        let (prev, curr, flow, dst, staging) = buffers(&device, pixels, blocks);
        Some(Self {
            device,
            queue,
            motion,
            warp,
            layout,
            params,
            prev,
            curr,
            flow,
            dst,
            staging,
            pixels,
            blocks,
            packed: Vec::new(),
            upload: Vec::new(),
            slow: false,
        })
    }

    fn apply(
        &mut self,
        prev: &[u8],
        curr: &[u8],
        width: u32,
        height: u32,
        t_q8: u32,
        out: &mut [u8],
    ) -> bool {
        if self.slow {
            return false;
        }
        let pixels = width.saturating_mul(height);
        if pixels == 0 {
            return false;
        }
        let started = std::time::Instant::now();
        let bw = width.div_ceil(BLOCK as u32);
        let bh = height.div_ceil(BLOCK as u32);
        let blocks = bw.saturating_mul(bh).max(1);
        if pixels > self.pixels || blocks > self.blocks {
            let (prev_b, curr_b, flow, dst, staging) = buffers(&self.device, pixels, blocks);
            self.prev = prev_b;
            self.curr = curr_b;
            self.flow = flow;
            self.dst = dst;
            self.staging = staging;
            self.pixels = pixels;
            self.blocks = blocks;
        }
        let mut params = [0u8; PARAMS_SIZE as usize];
        put_u32(&mut params, 0, width);
        put_u32(&mut params, 4, height);
        put_u32(&mut params, 8, t_q8);
        self.queue.write_buffer(&self.params, 0, &params);

        pack_rgb(prev, pixels, &mut self.packed, &mut self.upload);
        self.queue.write_buffer(&self.prev, 0, &self.upload);
        pack_rgb(curr, pixels, &mut self.packed, &mut self.upload);
        self.queue.write_buffer(&self.curr, 0, &self.upload);

        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("etherquic-fruc-bg"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.prev.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.curr.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.flow.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.dst.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("etherquic-fruc-enc"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("etherquic-fruc-motion"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.motion);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(bw.div_ceil(8), bh.div_ceil(8), 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("etherquic-fruc-warp"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.warp);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
        }
        encoder.copy_buffer_to_buffer(&self.dst, 0, &self.staging, 0, u64::from(pixels) * 4);
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
            for (i, colour) in out.chunks_exact_mut(3).take(pixels as usize).enumerate() {
                colour[0] = data[i * 4 + 2];
                colour[1] = data[i * 4 + 1];
                colour[2] = data[i * 4];
            }
        }
        self.staging.unmap();
        if started.elapsed() >= std::time::Duration::from_millis(8) {
            self.slow = true;
        }
        true
    }
}

fn pack_rgb(rgb: &[u8], pixels: u32, packed: &mut Vec<u32>, upload: &mut Vec<u8>) {
    packed.resize(pixels as usize, 0);
    for (i, colour) in rgb.chunks_exact(3).take(pixels as usize).enumerate() {
        packed[i] = (u32::from(colour[0]) << 16) | (u32::from(colour[1]) << 8) | u32::from(colour[2]);
    }
    upload.clear();
    upload.reserve(packed.len() * 4);
    for word in packed.iter() {
        upload.extend_from_slice(&word.to_le_bytes());
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

fn buffers(
    device: &wgpu::Device,
    pixels: u32,
    blocks: u32,
) -> (
    wgpu::Buffer,
    wgpu::Buffer,
    wgpu::Buffer,
    wgpu::Buffer,
    wgpu::Buffer,
) {
    let bytes = u64::from(pixels.max(1)) * 4;
    let prev = buffer(
        device,
        bytes,
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    );
    let curr = buffer(
        device,
        bytes,
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    );
    let flow = buffer(
        device,
        u64::from(blocks.max(1)) * 4,
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
    (prev, curr, flow, dst, staging)
}

fn put_u32(buf: &mut [u8], off: usize, value: u32) {
    buf[off..off + 4].copy_from_slice(&value.to_le_bytes());
}
