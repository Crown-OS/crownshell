use smithay_client_toolkit::{
    delegate_keyboard,
    seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
    shell::WaylandSurface,
};
use wayland_client::{
    Connection, QueueHandle,
    protocol::{wl_keyboard::WlKeyboard, wl_surface::WlSurface},
};

use crate::app::App;

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        surface: &WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        // A compositor can hand focus to a surface we do not own — another
        // client's, or one of ours that has just been torn down. Leaving the
        // focus unset then keeps every later key event from being delivered to
        // the wrong window.
        let ours = self.windows.iter().any(|w| w.layer.wl_surface() == surface);
        if !ours {
            log::debug!("keyboard focus entered a surface we do not own");
            self.keyboard_focus = None;
            return;
        }
        self.keyboard_focus = Some(surface.clone());

        let App {
            compositor_state,
            qh,
            text_cx,
            windows,
            ..
        } = self;
        if let Some(window) = windows.iter_mut().find(|w| w.layer.wl_surface() == surface) {
            window.on_keyboard_enter(compositor_state, qh, text_cx);
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        surface: &WlSurface,
        _: u32,
    ) {
        if self.keyboard_focus.as_ref() != Some(surface) {
            return;
        }
        self.keyboard_focus = None;
        // Modifiers are only meaningful while focused; a stale set would leak
        // into the next surface's first keypress.
        self.modifiers = Modifiers::default();

        let App {
            compositor_state,
            qh,
            text_cx,
            windows,
            ..
        } = self;
        if let Some(window) = windows.iter_mut().find(|w| w.layer.wl_surface() == surface) {
            window.on_keyboard_leave(compositor_state, qh, text_cx);
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.dispatch_key(&event, true, false);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.dispatch_key(&event, false, false);
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;

        let App {
            compositor_state,
            qh,
            text_cx,
            windows,
            keyboard_focus,
            ..
        } = self;
        let Some(surface) = keyboard_focus.as_ref() else {
            return;
        };
        if let Some(window) = windows.iter_mut().find(|w| w.layer.wl_surface() == surface) {
            window.on_modifiers(modifiers, compositor_state, qh, text_cx);
        }
    }
}

delegate_keyboard!(App);
