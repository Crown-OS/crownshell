use smithay_client_toolkit::{
    delegate_layer,
    shell::wlr_layer::{LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
};
use wayland_client::{Connection, QueueHandle};

use crate::app::App;

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(idx) = self.windows.iter().position(|w| &w.layer == layer) {
            // A surface pinned to an output closes when that output goes away;
            // drop just the window and let the app live on for the others.
            if self.windows[idx].output().is_some() {
                self.windows.remove(idx);
                return;
            }
        }
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        // Windows can appear at any time (output hotplug), so the tick timer
        // is (re-)armed here rather than only once in `run`.
        self.arm_tick_timer();
        let App {
            compositor_state,
            qh,
            text_cx,
            windows,
            connection,
            raw_gpu,
            init_error,
            ..
        } = self;
        let Some(window) = windows.iter_mut().find(|w| &w.layer == layer) else {
            return;
        };
        let new_w = if configure.new_size.0 != 0 {
            configure.new_size.0
        } else {
            window.width
        };
        let new_h = if configure.new_size.1 != 0 {
            configure.new_size.1
        } else {
            window.height
        };
        window.resize(new_w, new_h);
        window.first_configure = false;
        // The renderer is created on first configure — only now is the
        // surface's real size known.
        if let Err(e) = window.ensure_renderer(connection, raw_gpu, qh) {
            log::error!("renderer init failed: {e}");
            if init_error.is_none() {
                *init_error = Some(e);
            }
            return;
        }
        if window.wants_auto_blur_region() {
            window.apply_blur_region(compositor_state);
        }
        window.request_frame(compositor_state, qh, text_cx);
    }
}

delegate_layer!(App);
