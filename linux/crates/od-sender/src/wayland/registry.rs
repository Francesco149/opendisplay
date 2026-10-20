//! One roundtrip over the Wayland registry: which globals the compositor
//! advertises and which outputs exist. Used by backend detection, adopt mode
//! and `doctor`; capture and input keep their own connections because they
//! run on their own threads.

use std::collections::HashMap;

use anyhow::{Context, Result};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, QueueHandle};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputSnapshot {
    pub name: String,
    /// Current mode, physical pixels.
    pub width: i32,
    pub height: i32,
    pub refresh_mhz: i32,
    /// `wl_output.scale`: an integer, the ceiling of a fractional scale.
    pub scale: i32,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// `(interface, version)` in advertised order.
    pub globals: Vec<(String, u32)>,
    pub outputs: Vec<OutputSnapshot>,
}

impl Snapshot {
    pub fn has(&self, interface: &str) -> bool {
        self.globals.iter().any(|(i, _)| i == interface)
    }

    pub fn output(&self, name: &str) -> Option<&OutputSnapshot> {
        self.outputs.iter().find(|o| o.name == name)
    }
}

/// Globals that matter to the sender, for `doctor` and detection.
pub const INTERESTING: &[&str] = &[
    "ext_image_copy_capture_manager_v1",
    "ext_output_image_capture_source_manager_v1",
    "zwlr_screencopy_manager_v1",
    "zwp_linux_dmabuf_v1",
    "zwlr_virtual_pointer_manager_v1",
    "zwp_virtual_keyboard_manager_v1",
    "zwlr_output_manager_v1",
    "zxdg_output_manager_v1",
    "zkde_screencast_unstable_v1",
    "wp_presentation",
];

struct State {
    globals: Vec<(String, u32)>,
    outputs: HashMap<u32, OutputSnapshot>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            // Output names need wl_output v4.
            if interface == "wl_output" && version >= 4 {
                let _: wl_output::WlOutput = registry.bind(name, 4, qh, name);
                state.outputs.insert(name, OutputSnapshot::default());
            }
            state.globals.push((interface, version));
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(o) = state.outputs.get_mut(global) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => o.name = name,
            wl_output::Event::Geometry { x, y, .. } => (o.x, o.y) = (x, y),
            wl_output::Event::Scale { factor } => o.scale = factor,
            wl_output::Event::Mode {
                flags,
                width,
                height,
                refresh,
            } if flags
                .into_result()
                .is_ok_and(|f| f.contains(wl_output::Mode::Current)) =>
            {
                (o.width, o.height, o.refresh_mhz) = (width, height, refresh);
            }
            _ => {}
        }
    }
}

/// Connect to `$WAYLAND_DISPLAY` and read the registry and every output.
pub fn snapshot() -> Result<Snapshot> {
    let conn = Connection::connect_to_env().context("connecting to Wayland")?;
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut st = State {
        globals: Vec::new(),
        outputs: HashMap::new(),
    };
    queue.roundtrip(&mut st)?;
    queue.roundtrip(&mut st)?;
    let mut outputs: Vec<_> = st.outputs.into_values().collect();
    outputs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Snapshot {
        globals: st.globals,
        outputs,
    })
}
