use smithay_client_toolkit::{compositor::CompositorState, shell::wlr_layer::LayerSurface};
use vello::Scene;
use wayland_client::QueueHandle;
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;

use crate::app::App;

pub struct SurfaceCtx<'a> {
    pub size: (u32, u32),
    pub compositor_state: &'a CompositorState,
    pub layer: &'a LayerSurface,
    pub bg_effect_surface: Option<&'a ExtBackgroundEffectSurfaceV1>,
    pub qh: &'a QueueHandle<App>,
}

pub struct DragOffer<'a> {
    pub mime_types: &'a [String],
    pub x: f64,
    pub y: f64,
}

pub struct DropPayload {
    pub mime_type: String,
    pub data: Vec<u8>,
    pub x: f64,
    pub y: f64,
}

pub trait SurfaceHandler {
    fn paint(&mut self, scene: &mut Scene, ctx: SurfaceCtx<'_>);

    fn on_pointer_enter(&mut self, _x: f64, _y: f64, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_pointer_leave(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_pointer_motion(&mut self, _x: f64, _y: f64, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_pointer_press(&mut self, _x: f64, _y: f64, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_pointer_release(&mut self, _x: f64, _y: f64, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }

    /// Return the mime type to accept, or None to reject the drag.
    fn on_drag_enter(&mut self, _offer: DragOffer<'_>, _ctx: SurfaceCtx<'_>) -> Option<String> {
        None
    }
    fn on_drag_motion(&mut self, _x: f64, _y: f64, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_drag_leave(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    /// Called after the drop payload has been fully read from the source pipe.
    fn on_drop(&mut self, _drop: DropPayload, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }

    fn on_tick(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_frame(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
}
