use smithay_client_toolkit::{
    delegate_output,
    output::{OutputHandler, OutputState},
};
use wayland_client::{Connection, QueueHandle, protocol::wl_output};

use crate::app::App;

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.emit_output_added(&output);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // Hooks run first, while the output's windows still exist.
        self.emit_output_removed(&output);
        self.windows.retain(|w| w.output() != Some(&output));
    }
}

delegate_output!(App);
