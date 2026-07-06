use smithay_client_toolkit::{
    delegate_layer,
    shell::wlr_layer::{LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
};
use wayland_client::{Connection, QueueHandle};

use crate::app::App;

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
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
        let App {
            compositor_state,
            qh,
            windows,
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
        window.request_frame(compositor_state, qh);
    }
}

delegate_layer!(App);
