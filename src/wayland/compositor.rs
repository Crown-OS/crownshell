use smithay_client_toolkit::{
    compositor::CompositorHandler, delegate_compositor, shell::WaylandSurface,
};
use wayland_client::{
    protocol::{wl_output, wl_surface},
    Connection, QueueHandle,
};

use crate::app::App;

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        let App {
            compositor_state,
            qh,
            text_cx,
            windows,
            ..
        } = self;
        let Some(window) = windows.iter_mut().find(|w| w.layer.wl_surface() == surface) else {
            return;
        };
        // The buffer scale only reaches the compositor on the next commit, so
        // repaint at the new density right away.
        if window.set_scale(new_factor) {
            window.request_frame(compositor_state, qh, text_cx);
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        let App {
            compositor_state,
            qh,
            text_cx,
            windows,
            ..
        } = self;
        if let Some(window) = windows.iter_mut().find(|w| w.layer.wl_surface() == surface) {
            window.on_frame(compositor_state, qh, text_cx);
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

delegate_compositor!(App);
