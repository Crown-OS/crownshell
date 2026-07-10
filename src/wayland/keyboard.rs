use smithay_client_toolkit::{
    delegate_keyboard,
    seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
    shell::WaylandSurface,
};
use wayland_client::{
    protocol::{wl_keyboard::WlKeyboard, wl_surface::WlSurface},
    Connection, QueueHandle,
};

use crate::{app::App, handler::SurfaceCtx};

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: &WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: &WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        // let App {
        //     compositor_state,
        //     qh,
        //     lock_windows,
        //     windows,
        //     ..
        // } = self;
        // let text = event.utf8.as_deref();
        // let sym = event.keysym.raw();
        // let raw = event.raw_code;
        // for lw in lock_windows.iter_mut() {
        //     let ctx = SurfaceCtx {
        //         size: (lw.width, lw.height),
        //         compositor_state,
        //         wl_surface: &lw.wl_surface,
        //         layer: None,
        //         bg_effect_surface: None,
        //         qh,
        //     };
        //     if lw.handler.on_key(raw, sym, text, ctx) {
        //         lw.request_frame(compositor_state, qh);
        //     }
        // }
        // for w in windows.iter_mut() {
        //     let ctx = SurfaceCtx {
        //         size: (w.width, w.height),
        //         compositor_state,
        //         wl_surface: w.layer.wl_surface(),
        //         layer: Some(&w.layer),
        //         bg_effect_surface: w.bg_effect_surface.as_ref(),
        //         qh,
        //     };
        //     if w.handler.on_key(raw, sym, text, ctx) {
        //         w.request_frame(compositor_state, qh);
        //     }
        // }
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        _: Modifiers,
        _: u32,
    ) {
    }
}

delegate_keyboard!(App);
