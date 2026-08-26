//! Premultiplying blit from Vello's target into the swapchain.
//!
//! Vello's fine shader stores straight (un-premultiplied) alpha, but a Wayland
//! buffer is always premultiplied. Copying the target verbatim therefore shows
//! every partially covered pixel at full RGB intensity, which flattens the
//! coverage ramp that antialiasing is made of.

use vello::wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor,
    BindGroupLayoutEntry, BindingResource, BindingType, BlendState, Color, ColorTargetState,
    ColorWrites, CommandEncoder, Device, FragmentState, LoadOp, MultisampleState, Operations,
    PipelineCompilationOptions, PipelineLayoutDescriptor, PrimitiveState, RenderPassColorAttachment,
    RenderPassDescriptor, RenderPipeline, RenderPipelineDescriptor, ShaderModuleDescriptor,
    ShaderSource, ShaderStages, StoreOp, TextureFormat, TextureSampleType, TextureView,
    TextureViewDimension, VertexState,
};

pub(crate) struct PremulBlit {
    pipeline: RenderPipeline,
    layout: BindGroupLayout,
    bind_group: Option<BindGroup>,
}

impl PremulBlit {
    pub(crate) fn new(device: &Device, format: TextureFormat) -> Self {
        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("crownshell::blit::shader"),
            source: ShaderSource::Wgsl(SHADER.into()),
        });

        let layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("crownshell::blit::bind_group_layout"),
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Texture {
                    // Sampled with textureLoad, so no filtering sampler is bound.
                    sample_type: TextureSampleType::Float { filterable: false },
                    view_dimension: TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("crownshell::blit::pipeline_layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("crownshell::blit::pipeline"),
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
                    blend: Some(BlendState::REPLACE),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            layout,
            bind_group: None,
        }
    }

    /// Drops the bind group. Called on resize, when Vello hands out a new target view.
    pub(crate) fn invalidate(&mut self) {
        self.bind_group = None;
    }

    pub(crate) fn record(
        &mut self,
        device: &Device,
        encoder: &mut CommandEncoder,
        source: &TextureView,
        dest: &TextureView,
    ) {
        let bind_group = self.bind_group.get_or_insert_with(|| {
            device.create_bind_group(&BindGroupDescriptor {
                label: Some("crownshell::blit::bind_group"),
                layout: &self.layout,
                entries: &[BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(source),
                }],
            })
        });

        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("crownshell::blit::premultiply"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: dest,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Clear(Color::TRANSPARENT),
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &*bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

const SHADER: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let u = f32((index << 1u) & 2u);
    let v = f32(index & 2u);
    var out: VertexOutput;
    out.position = vec4<f32>(u * 2.0 - 1.0, 1.0 - v * 2.0, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureLoad(src, vec2<i32>(in.position.xy), 0);
    return vec4<f32>(texel.rgb * texel.a, texel.a);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_compiles() {
        use vello::wgpu::naga;

        let module = naga::front::wgsl::parse_str(SHADER).expect("blit shader failed to parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("blit shader failed to validate");
    }
}
