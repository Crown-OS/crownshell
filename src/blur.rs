//! Separable Gaussian post-process for the finished frame.
//!
//! Vello renders into an offscreen target and the swapchain is normally reached
//! with a plain blit. When a handler asks for blur that blit is replaced by two
//! full-screen passes: horizontal into a scratch texture, vertical from the
//! scratch straight into the swapchain view. Two passes of `2r+1` taps instead
//! of one pass of `(2r+1)²`.
//!
//! Everything here is created on the first blurred frame and kept afterwards,
//! so an app that never blurs pays nothing and an app that blurs for 300 ms
//! pays the pipeline compile once.

use vello::wgpu::{
    self, AddressMode, BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout,
    BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingResource, BindingType, BlendState,
    Buffer, BufferBindingType, BufferUsages, Color, ColorTargetState, ColorWrites, CommandEncoder,
    Device, Extent3d, FilterMode, FragmentState, LoadOp, MipmapFilterMode, MultisampleState,
    Operations, PipelineCompilationOptions, PipelineLayoutDescriptor, PrimitiveState, Queue,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline, RenderPipelineDescriptor,
    Sampler, SamplerBindingType, SamplerDescriptor, ShaderModuleDescriptor, ShaderSource,
    ShaderStages, StoreOp, TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType,
    TextureUsages, TextureView, TextureViewDescriptor, TextureViewDimension, VertexState,
    util::DeviceExt,
};

/// Sigmas at or below this are indistinguishable from no blur at all, and are
/// the resting value of an animation that has settled — the frame takes the
/// plain blit path instead.
pub(crate) const MIN_SIGMA: f32 = 0.05;

/// Largest kernel half-width, in texels. Bounds the per-pixel tap count so a
/// runaway sigma cannot stall the GPU; [`effective_sigma`] narrows the Gaussian
/// to fit rather than truncating it into a box.
pub(crate) const MAX_RADIUS: u32 = 64;

/// Whether `sigma` is worth a blur pass.
pub(crate) fn is_enabled(sigma: f32) -> bool {
    // Written as a positive test so a NaN sigma disables the pass.
    sigma > MIN_SIGMA
}

/// The sigma actually rendered with: capped so `ceil(3σ)` stays within
/// [`MAX_RADIUS`], because a Gaussian cut off at three sigma has lost only
/// ~1% of its mass but one cut off sooner shows its edge.
pub(crate) fn effective_sigma(sigma: f32) -> f32 {
    sigma.min(MAX_RADIUS as f32 / 3.0)
}

/// Kernel half-width in texels for `sigma`, zero when blur is off.
pub(crate) fn kernel_radius(sigma: f32) -> u32 {
    if !is_enabled(sigma) {
        return 0;
    }
    ((3.0 * effective_sigma(sigma)).ceil() as u32).clamp(1, MAX_RADIUS)
}

/// Per-pass uniform. Weights are evaluated in the shader from `sigma`, so
/// nothing scales with the radius here.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    /// One texel step along the axis being blurred, in UV space.
    step: [f32; 2],
    sigma: f32,
    radius: f32,
    /// 0 = horizontal (premultiply on read), 1 = vertical (unpremultiply on write).
    vertical: f32,
    _pad: [f32; 3],
}

/// The scratch texture and bind groups for one surface size. Rebuilt on resize,
/// which is also when Vello hands out a new `target_view`.
struct Targets {
    size: (u32, u32),
    scratch_view: TextureView,
    horizontal: Pass,
    vertical: Pass,
}

struct Pass {
    bind_group: BindGroup,
    params: Buffer,
}

pub(crate) struct Blur {
    pipeline: RenderPipeline,
    layout: BindGroupLayout,
    sampler: Sampler,
    format: TextureFormat,
    targets: Option<Targets>,
}

impl Blur {
    pub(crate) fn new(device: &Device, format: TextureFormat) -> Self {
        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("crownshell::blur::shader"),
            source: ShaderSource::Wgsl(SHADER.into()),
        });

        let layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("crownshell::blur::bind_group_layout"),
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: true },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 2,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Buffer {
                        ty: BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("crownshell::blur::pipeline_layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("crownshell::blur::pipeline"),
            layout: Some(&pipeline_layout),
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                targets: &[Some(ColorTargetState {
                    format,
                    // Both passes cover every pixel of their target, so there is
                    // nothing underneath to blend with.
                    blend: Some(BlendState::REPLACE),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("crownshell::blur::sampler"),
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            address_mode_w: AddressMode::ClampToEdge,
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            mipmap_filter: MipmapFilterMode::Nearest,
            ..Default::default()
        });

        Self {
            pipeline,
            layout,
            sampler,
            format,
            targets: None,
        }
    }

    /// Drops the size-dependent resources. Called when the surface resizes,
    /// because the source view the horizontal pass is bound to is replaced then.
    pub(crate) fn invalidate(&mut self) {
        self.targets = None;
    }

    /// Records both passes: `source` blurred horizontally into the scratch
    /// texture, then the scratch blurred vertically into `dest`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record(
        &mut self,
        device: &Device,
        queue: &Queue,
        encoder: &mut CommandEncoder,
        source: &TextureView,
        dest: &TextureView,
        size: (u32, u32),
        sigma: f32,
    ) {
        let (width, height) = (size.0.max(1), size.1.max(1));
        let targets = match self.targets.take() {
            Some(targets) if targets.size == (width, height) => targets,
            _ => self.build_targets(device, source, (width, height)),
        };

        let sigma = effective_sigma(sigma);
        let radius = kernel_radius(sigma) as f32;
        queue.write_buffer(
            &targets.horizontal.params,
            0,
            bytemuck::bytes_of(&Params {
                step: [1.0 / width as f32, 0.0],
                sigma,
                radius,
                vertical: 0.0,
                _pad: [0.0; 3],
            }),
        );
        queue.write_buffer(
            &targets.vertical.params,
            0,
            bytemuck::bytes_of(&Params {
                step: [0.0, 1.0 / height as f32],
                sigma,
                radius,
                vertical: 1.0,
                _pad: [0.0; 3],
            }),
        );

        draw(
            encoder,
            &self.pipeline,
            &targets.horizontal.bind_group,
            &targets.scratch_view,
            "crownshell::blur::horizontal",
        );
        draw(
            encoder,
            &self.pipeline,
            &targets.vertical.bind_group,
            dest,
            "crownshell::blur::vertical",
        );

        self.targets = Some(targets);
    }

    fn build_targets(&self, device: &Device, source: &TextureView, size: (u32, u32)) -> Targets {
        let scratch = device.create_texture(&TextureDescriptor {
            label: Some("crownshell::blur::scratch"),
            size: Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: self.format,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let scratch_view = scratch.create_view(&TextureViewDescriptor::default());

        Targets {
            horizontal: self.build_pass(device, source, "horizontal"),
            vertical: self.build_pass(device, &scratch_view, "vertical"),
            scratch_view,
            size,
        }
    }

    fn build_pass(&self, device: &Device, source: &TextureView, name: &str) -> Pass {
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("crownshell::blur::params"),
            contents: bytemuck::bytes_of(&Params {
                step: [0.0, 0.0],
                sigma: 1.0,
                radius: 0.0,
                vertical: 0.0,
                _pad: [0.0; 3],
            }),
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        });

        let bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some(name),
            layout: &self.layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(source),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::Sampler(&self.sampler),
                },
                BindGroupEntry {
                    binding: 2,
                    resource: params.as_entire_binding(),
                },
            ],
        });

        Pass { bind_group, params }
    }
}

fn draw(
    encoder: &mut CommandEncoder,
    pipeline: &RenderPipeline,
    bind_group: &BindGroup,
    target: &TextureView,
    label: &str,
) {
    let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: Operations {
                // The full-screen triangle writes every pixel, so there is
                // nothing to preserve — clearing skips the load on tilers.
                load: LoadOp::Clear(Color::TRANSPARENT),
                store: StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.draw(0..3, 0..1);
}

const SHADER: &str = r#"
struct Params {
    step: vec2<f32>,
    sigma: f32,
    radius: f32,
    vertical: f32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One oversized triangle covering the whole target; no vertex buffer.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let u = f32((index << 1u) & 2u);
    let v = f32(index & 2u);
    var out: VertexOutput;
    out.uv = vec2<f32>(u, v);
    out.position = vec4<f32>(u * 2.0 - 1.0, 1.0 - v * 2.0, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let radius = i32(params.radius);
    let falloff = -1.0 / (2.0 * params.sigma * params.sigma);
    let horizontal = params.vertical < 0.5;

    var acc = vec4<f32>(0.0);
    var weight_sum = 0.0;
    for (var i = -radius; i <= radius; i = i + 1) {
        let offset = f32(i);
        let weight = exp(offset * offset * falloff);
        var texel = textureSampleLevel(src, src_sampler, in.uv + params.step * offset, 0.0);
        // Vello writes straight alpha. Averaging that directly drags the colour
        // of fully transparent texels into the result, so the horizontal pass
        // premultiplies on the way in and the vertical pass undoes it on the
        // way out; the scratch texture holds premultiplied values in between.
        if (horizontal) {
            texel = vec4<f32>(texel.rgb * texel.a, texel.a);
        }
        acc = acc + texel * weight;
        weight_sum = weight_sum + weight;
    }

    var color = acc / weight_sum;
    if (!horizontal) {
        if (color.a > 0.0) {
            color = vec4<f32>(color.rgb / color.a, color.a);
        } else {
            color = vec4<f32>(0.0);
        }
    }
    return color;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_compiles() {
        // No GPU is needed to catch a typo in the WGSL: naga is the same front
        // end `create_shader_module` would hand it to, and a shader that fails
        // to validate would otherwise only show up as a blank frame.
        use vello::wgpu::naga;

        let module = naga::front::wgsl::parse_str(SHADER).expect("blur shader failed to parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("blur shader failed to validate");
    }

    #[test]
    fn the_uniform_matches_what_the_shader_declares() {
        // std140-ish layout: vec2 at 0, three scalars behind it, padded to a
        // multiple of the vec2's alignment.
        assert_eq!(size_of::<Params>(), 32);
    }

    #[test]
    fn a_settled_animation_bypasses_the_pass() {
        assert!(!is_enabled(0.0));
        assert!(!is_enabled(MIN_SIGMA));
        assert!(!is_enabled(f32::NAN));
        assert!(!is_enabled(-4.0));
        assert!(is_enabled(MIN_SIGMA + 0.001));
        assert_eq!(kernel_radius(0.0), 0);
        assert_eq!(kernel_radius(MIN_SIGMA), 0);
    }

    #[test]
    fn radius_covers_three_sigma() {
        assert_eq!(kernel_radius(1.0), 3);
        assert_eq!(kernel_radius(2.0), 6);
        assert_eq!(kernel_radius(18.0), 54);
        // ceil, not round: 3 * 4.5 is already 13.5 texels of support.
        assert_eq!(kernel_radius(4.5), 14);
    }

    #[test]
    fn a_barely_enabled_sigma_still_gets_a_tap_either_side() {
        assert_eq!(kernel_radius(0.06), 1);
    }

    #[test]
    fn a_runaway_sigma_is_bounded() {
        assert_eq!(kernel_radius(1_000.0), MAX_RADIUS);
        assert_eq!(kernel_radius(f32::MAX), MAX_RADIUS);
        assert_eq!(kernel_radius(f32::INFINITY), MAX_RADIUS);
    }

    #[test]
    fn clamping_narrows_the_gaussian_instead_of_truncating_it() {
        // Whatever sigma the handler asks for, the kernel it is rendered with
        // still reaches three sigma — otherwise the cut-off edge shows.
        for sigma in [0.5, 6.0, 21.3, 400.0] {
            let used = effective_sigma(sigma);
            assert!(used <= sigma);
            assert!(3.0 * used <= kernel_radius(sigma) as f32);
        }
        assert_eq!(effective_sigma(6.0), 6.0);
        assert_eq!(effective_sigma(400.0), MAX_RADIUS as f32 / 3.0);
    }
}
