use calloop::{LoopHandle, RegistrationToken};
use smithay_client_toolkit::{
    compositor::CompositorState,
    data_device_manager::{data_device::DataDevice, data_offer::DragOffer, DataDeviceManagerState},
    output::OutputState,
    registry::RegistryState,
    seat::SeatState,
    shell::{wlr_layer::LayerShell, WaylandSurface},
};
use wayland_client::{
    globals::registry_queue_init, protocol::wl_pointer::WlPointer, protocol::wl_surface::WlSurface,
    Connection, EventQueue, QueueHandle,
};

use crate::{
    handler::SurfaceHandler,
    wayland::background_effect::BackgroundEffect,
    window::{Window, WindowConfig},
};

pub(crate) struct ActiveDrag {
    pub surface: WlSurface,
    pub accepted_mime: Option<String>,
    pub pos: (f64, f64),
}

pub(crate) struct PendingRead {
    pub offer: DragOffer,
    pub surface: WlSurface,
    pub mime_type: String,
    pub data: Vec<u8>,
    pub pos: (f64, f64),
    pub token: Option<RegistrationToken>,
}

#[derive(Default)]
pub(crate) struct DndState {
    pub accept_counter: u32,
    pub active: Option<ActiveDrag>,
    pub pending_reads: Vec<PendingRead>,
}

pub struct App {
    pub registry_state: RegistryState,
    pub output_state: OutputState,
    pub seat_state: SeatState,
    pub compositor_state: CompositorState,
    pub layer_shell: LayerShell,
    pub background_effect: Option<BackgroundEffect>,
    pub data_device_manager: Option<DataDeviceManagerState>,
    pub data_devices: Vec<DataDevice>,
    pub pointer: Option<WlPointer>,
    pub qh: QueueHandle<App>,
    pub loop_handle: LoopHandle<'static, App>,
    pub tick_timer: Option<RegistrationToken>,
    pub windows: Vec<Window>,
    pub(crate) dnd: DndState,
    pub exit: bool,
}

impl App {
    pub fn try_new(
        loop_handle: LoopHandle<'static, App>,
    ) -> anyhow::Result<(Connection, EventQueue<App>, Self)> {
        let connection = Connection::connect_to_env()?;
        let (globals, event_queue) = registry_queue_init(&connection)?;
        let qh: QueueHandle<App> = event_queue.handle();

        let compositor_state = CompositorState::bind(&globals, &qh)?;
        let layer_shell = LayerShell::bind(&globals, &qh)?;
        let seat_state = SeatState::new(&globals, &qh);
        let background_effect = BackgroundEffect::bind(&globals, &qh);
        let data_device_manager = DataDeviceManagerState::bind(&globals, &qh).ok();

        let app = Self {
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),
            seat_state,
            compositor_state,
            layer_shell,
            background_effect,
            data_device_manager,
            data_devices: Vec::new(),
            pointer: None,
            qh,
            loop_handle,
            tick_timer: None,
            windows: Vec::new(),
            dnd: DndState::default(),
            exit: false,
        };

        Ok((connection, event_queue, app))
    }

    pub fn create_window<H: SurfaceHandler + 'static>(&mut self, config: WindowConfig, handler: H) {
        let window = Window::new(
            config,
            Box::new(handler),
            &self.compositor_state,
            &self.layer_shell,
            self.background_effect.as_ref(),
            &self.qh,
            None,
        );
        self.windows.push(window);
    }

    pub fn all_configured(&self) -> bool {
        !self.windows.is_empty() && self.windows.iter().all(|w| !w.first_configure)
    }

    pub fn apply_blur_regions(&self) {
        for window in &self.windows {
            if window.wants_blur() {
                window.apply_blur_region(&self.compositor_state, self.background_effect.as_ref());
            }
        }
    }

    pub fn paint_all(&mut self) {
        let App {
            compositor_state,
            qh,
            windows,
            ..
        } = self;
        for window in windows.iter_mut() {
            window.paint(compositor_state, qh);
        }
    }

    pub fn window_by_surface_mut(&mut self, surface: &WlSurface) -> Option<&mut Window> {
        self.windows
            .iter_mut()
            .find(|w| w.layer.wl_surface() == surface)
    }

    pub fn arm_tick_timer(&mut self) {
        if self.tick_timer.is_some() || self.windows.is_empty() {
            return;
        }
        let timer = self.windows[0].build_tick_timer();
        let interval = self.windows[0].tick_interval();
        let token = self
            .loop_handle
            .insert_source(timer, move |_deadline, _, app: &mut App| {
                let App {
                    compositor_state,
                    qh,
                    windows,
                    ..
                } = app;
                for window in windows.iter_mut() {
                    window.on_tick(compositor_state, qh);
                }
                calloop::timer::TimeoutAction::ToDuration(interval)
            });
        match token {
            Ok(t) => self.tick_timer = Some(t),
            Err(e) => log::warn!("failed to install tick timer: {e}"),
        }
    }
}
