use smithay_client_toolkit::{
    delegate_pointer,
    seat::pointer::{PointerEvent, PointerEventKind, PointerHandler},
    shell::WaylandSurface,
};
use wayland_client::{protocol::wl_pointer, Connection, QueueHandle};

use crate::app::App;

const BTN_LEFT: u32 = 0x110;

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let App {
            compositor_state,
            qh,
            windows,
            ..
        } = self;
        for event in events {
            let Some(window) = windows
                .iter_mut()
                .find(|w| w.layer.wl_surface() == &event.surface)
            else {
                continue;
            };
            let (x, y) = event.position;
            match event.kind {
                PointerEventKind::Enter { .. } => {
                    window.on_pointer_enter(x, y, compositor_state, qh)
                }
                PointerEventKind::Leave { .. } => window.on_pointer_leave(compositor_state, qh),
                PointerEventKind::Motion { .. } => {
                    window.on_pointer_motion(x, y, compositor_state, qh)
                }
                PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                    window.on_pointer_press(x, y, compositor_state, qh)
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    window.on_pointer_release(x, y, compositor_state, qh)
                }
                _ => {}
            }
        }
    }
}

delegate_pointer!(App);
