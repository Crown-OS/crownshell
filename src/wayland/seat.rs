use smithay_client_toolkit::{
    delegate_seat,
    seat::{Capability, SeatHandler, SeatState},
};
use wayland_client::{Connection, QueueHandle, protocol::wl_seat};

use crate::app::App;

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if let Some(ddm) = self.data_device_manager.as_ref() {
            let dd = ddm.get_data_device(qh, &seat);
            self.data_devices.push(dd);
        }
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Pointer if self.pointer.is_none() => {
                match self.seat_state.get_pointer(qh, &seat) {
                    Ok(pointer) => self.pointer = Some(pointer),
                    Err(e) => log::warn!("failed to get pointer: {e}"),
                }
            }
            Capability::Keyboard if self.keyboard.is_none() => {
                // Repeat is driven by a calloop timer inside SCTK, which is why
                // it needs our loop handle: a held key then keeps producing
                // events without the compositor resending them, which is what
                // a search box or a text field expects.
                let loop_handle = self.loop_handle.clone();
                let keyboard = self.seat_state.get_keyboard_with_repeat(
                    qh,
                    &seat,
                    None,
                    loop_handle,
                    Box::new(|app: &mut App, _kbd, event| app.dispatch_key(&event, true, true)),
                );
                match keyboard {
                    Ok(keyboard) => self.keyboard = Some(keyboard),
                    Err(e) => log::warn!("failed to get keyboard: {e}"),
                }
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Pointer => {
                if let Some(pointer) = self.pointer.take() {
                    pointer.release();
                }
            }
            Capability::Keyboard => {
                if let Some(keyboard) = self.keyboard.take() {
                    keyboard.release();
                }
                self.keyboard_focus = None;
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

delegate_seat!(App);
