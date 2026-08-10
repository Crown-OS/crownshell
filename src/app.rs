use calloop::{LoopHandle, RegistrationToken};
use smithay_client_toolkit::{
    compositor::CompositorState,
    data_device_manager::{DataDeviceManagerState, data_device::DataDevice, data_offer::DragOffer},
    output::{OutputInfo, OutputState},
    registry::RegistryState,
    seat::{
        SeatState,
        keyboard::{KeyEvent, Modifiers},
    },
    shell::{WaylandSurface, wlr_layer::LayerShell},
};
use wayland_client::{
    Connection, EventQueue, QueueHandle,
    globals::registry_queue_init,
    protocol::{
        wl_keyboard::WlKeyboard, wl_output::WlOutput, wl_pointer::WlPointer, wl_surface::WlSurface,
    },
};

use crate::{
    handler::{KeyPress, RawSurfaceHandler, SurfaceHandler},
    renderer::SharedGpu,
    text::TextContext,
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

type OutputHook = Box<dyn FnMut(&mut App, &WlOutput)>;

#[derive(Default)]
pub(crate) struct OutputHooks {
    added: Vec<OutputHook>,
    removed: Vec<OutputHook>,
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
    pub keyboard: Option<WlKeyboard>,
    /// The surface the compositor last gave keyboard focus to, if it is one of
    /// ours. Every key event is routed to the window owning it.
    pub(crate) keyboard_focus: Option<WlSurface>,
    /// Latest modifier state, folded into each [`KeyPress`] — `wl_keyboard.key`
    /// does not carry it.
    pub(crate) modifiers: Modifiers,
    pub qh: QueueHandle<App>,
    pub loop_handle: LoopHandle<'static, App>,
    pub tick_timer: Option<RegistrationToken>,
    /// Font database and layout caches shared by every window.
    pub text_cx: TextContext,
    pub windows: Vec<Window>,
    pub(crate) dnd: DndState,
    pub(crate) connection: Connection,
    /// The wgpu instance and device pool shared by raw windows; created
    /// lazily when the first raw window is configured.
    pub(crate) raw_gpu: Option<SharedGpu>,
    /// First fatal GPU-init error; makes [`run`](crate::run) return `Err`.
    pub(crate) init_error: Option<anyhow::Error>,
    pub(crate) output_hooks: OutputHooks,
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
            keyboard: None,
            keyboard_focus: None,
            modifiers: Modifiers::default(),
            qh,
            loop_handle,
            tick_timer: None,
            text_cx: TextContext::new(),
            windows: Vec::new(),
            dnd: DndState::default(),
            connection: connection.clone(),
            raw_gpu: None,
            init_error: None,
            output_hooks: OutputHooks::default(),
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

    /// Like [`create_window`](Self::create_window), but pins the surface to a
    /// specific output instead of letting the compositor pick one.
    ///
    /// The window is destroyed automatically when the output disappears; pair
    /// this with [`on_output_added`](Self::on_output_added) to recreate it on
    /// hotplug.
    pub fn create_window_on_output<H: SurfaceHandler + 'static>(
        &mut self,
        config: WindowConfig,
        handler: H,
        output: &WlOutput,
    ) {
        let window = Window::new(
            config,
            Box::new(handler),
            &self.compositor_state,
            &self.layer_shell,
            self.background_effect.as_ref(),
            &self.qh,
            Some(output),
        );
        self.windows.push(window);
    }

    /// Creates a window whose handler renders with its own wgpu pipeline
    /// instead of a Vello [`Scene`](crate::Scene).
    pub fn create_raw_window<H: RawSurfaceHandler + 'static>(
        &mut self,
        config: WindowConfig,
        handler: H,
    ) {
        let window = Window::new_raw(
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

    /// [`create_raw_window`](Self::create_raw_window) pinned to a specific
    /// output — the building block of a per-output daemon such as a wallpaper.
    ///
    /// The window is destroyed automatically when the output disappears.
    pub fn create_raw_window_on_output<H: RawSurfaceHandler + 'static>(
        &mut self,
        config: WindowConfig,
        handler: H,
        output: &WlOutput,
    ) {
        let window = Window::new_raw(
            config,
            Box::new(handler),
            &self.compositor_state,
            &self.layer_shell,
            self.background_effect.as_ref(),
            &self.qh,
            Some(output),
        );
        self.windows.push(window);
    }

    /// The outputs currently known to the compositor.
    ///
    /// During [`run`](crate::run)'s setup phase this already lists the outputs
    /// present at startup, though their [`output_info`](Self::output_info) may
    /// still be incomplete until the first events are dispatched — hooks
    /// registered with [`on_output_added`](Self::on_output_added) fire once
    /// each output's information is complete, including for these initial
    /// outputs.
    pub fn outputs(&self) -> impl Iterator<Item = WlOutput> + '_ {
        self.output_state.outputs()
    }

    /// Name, mode, position, scale and the rest of what the compositor
    /// advertised for `output`.
    pub fn output_info(&self, output: &WlOutput) -> Option<OutputInfo> {
        self.output_state.info(output)
    }

    /// Registers a hook that runs whenever an output's information is
    /// complete: once per output present at startup and again on every
    /// hotplug. Check [`window_on_output`](Self::window_on_output) inside the
    /// hook to avoid creating a second window for an output handled in setup.
    pub fn on_output_added(&mut self, hook: impl FnMut(&mut App, &WlOutput) + 'static) {
        self.output_hooks.added.push(Box::new(hook));
    }

    /// Registers a hook that runs when an output disappears. It runs before
    /// crownshell destroys the windows bound to that output, so the hook can
    /// still inspect them.
    pub fn on_output_removed(&mut self, hook: impl FnMut(&mut App, &WlOutput) + 'static) {
        self.output_hooks.removed.push(Box::new(hook));
    }

    pub fn window_on_output(&self, output: &WlOutput) -> Option<&Window> {
        self.windows.iter().find(|w| w.output() == Some(output))
    }

    pub fn window_on_output_mut(&mut self, output: &WlOutput) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.output() == Some(output))
    }

    /// A handle to the calloop event loop `run` drives, for registering
    /// custom event sources — IPC sockets, worker-thread channels, timers.
    /// Callbacks get `&mut App` back, so they can reach every window.
    pub fn loop_handle(&self) -> LoopHandle<'static, App> {
        self.loop_handle.clone()
    }

    pub(crate) fn has_output_hooks(&self) -> bool {
        !self.output_hooks.added.is_empty() || !self.output_hooks.removed.is_empty()
    }

    // The hooks are moved out for the duration of the call so they can borrow
    // `self` mutably; hooks registered while they run are preserved.
    pub(crate) fn emit_output_added(&mut self, output: &WlOutput) {
        let mut hooks = std::mem::take(&mut self.output_hooks.added);
        for hook in hooks.iter_mut() {
            hook(self, output);
        }
        let mut registered_meanwhile = std::mem::replace(&mut self.output_hooks.added, hooks);
        self.output_hooks.added.append(&mut registered_meanwhile);
    }

    pub(crate) fn emit_output_removed(&mut self, output: &WlOutput) {
        let mut hooks = std::mem::take(&mut self.output_hooks.removed);
        for hook in hooks.iter_mut() {
            hook(self, output);
        }
        let mut registered_meanwhile = std::mem::replace(&mut self.output_hooks.removed, hooks);
        self.output_hooks.removed.append(&mut registered_meanwhile);
    }

    pub fn all_configured(&self) -> bool {
        !self.windows.is_empty() && self.windows.iter().all(|w| !w.first_configure)
    }

    pub fn apply_blur_regions(&self) {
        for window in &self.windows {
            if window.wants_auto_blur_region() {
                window.apply_blur_region(&self.compositor_state);
            }
        }
    }

    /// Repaints every surface whose handler reports [`needs_redraw`].
    ///
    /// Called once per event-loop iteration, after the events of that iteration
    /// have been dispatched, so that a handler can repaint a surface other than
    /// the one an event landed on.
    ///
    /// [`needs_redraw`]: crate::SurfaceHandler::needs_redraw
    pub fn flush_redraws(&mut self) {
        let App {
            compositor_state,
            qh,
            text_cx,
            windows,
            ..
        } = self;
        for window in windows.iter_mut() {
            if window.needs_redraw() {
                window.request_frame(compositor_state, qh, text_cx);
            }
        }
    }

    pub fn paint_all(&mut self) {
        let App {
            compositor_state,
            qh,
            text_cx,
            windows,
            ..
        } = self;
        for window in windows.iter_mut() {
            window.paint(compositor_state, qh, text_cx);
        }
    }

    pub fn window_by_surface_mut(&mut self, surface: &WlSurface) -> Option<&mut Window> {
        self.windows
            .iter_mut()
            .find(|w| w.layer.wl_surface() == surface)
    }

    /// Routes a key event to the focused window. `repeat` distinguishes the
    /// events synthesised by the repeat timer from real presses.
    pub(crate) fn dispatch_key(&mut self, event: &KeyEvent, pressed: bool, repeat: bool) {
        let App {
            compositor_state,
            qh,
            text_cx,
            windows,
            keyboard_focus,
            modifiers,
            ..
        } = self;
        let Some(surface) = keyboard_focus.as_ref() else {
            return;
        };
        let Some(window) = windows.iter_mut().find(|w| w.layer.wl_surface() == surface) else {
            return;
        };
        let key = KeyPress {
            keysym: event.keysym,
            raw_code: event.raw_code,
            text: event.utf8.as_deref(),
            modifiers: *modifiers,
            repeat,
        };
        window.on_key(key, pressed, compositor_state, qh, text_cx);
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
                    text_cx,
                    windows,
                    ..
                } = app;
                for window in windows.iter_mut() {
                    window.on_tick(compositor_state, qh, text_cx);
                }
                calloop::timer::TimeoutAction::ToDuration(interval)
            });
        match token {
            Ok(t) => self.tick_timer = Some(t),
            Err(e) => log::warn!("failed to install tick timer: {e}"),
        }
    }
}
