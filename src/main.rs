use clap::Parser;
use smithay_client_toolkit::{
  compositor::{CompositorHandler, CompositorState},
  delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
  delegate_simple,
  output::{OutputHandler, OutputState},
  reexports::{
    client::{
      globals::{registry_queue_init, GlobalList},
      protocol::{
        wl_buffer::{self, WlBuffer},
        wl_output::WlOutput,
        wl_region::WlRegion,
        wl_shm::Format,
      },
      Connection, Dispatch, QueueHandle,
    },
    protocols::wp::viewporter::client::{
      wp_viewport::{self, WpViewport},
      wp_viewporter::{self, WpViewporter},
    },
  },
  registry::{ProvidesRegistryState, RegistryState, SimpleGlobal},
  registry_handlers,
  shell::{
    wlr_layer::{KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface},
    WaylandSurface,
  },
  shm::{raw::RawPool, Shm, ShmHandler},
};

pub const DEFAULT_ALPHA: f32 = 0.5;
pub const DEFAULT_RADIUS: u32 = 0;

#[derive(Debug, Parser)]
#[command(version)]
pub struct DimlandArgs {
  #[arg(
    short,
    long,
    help = format!("0.0 is transparent, 1.0 is opaque, default is {DEFAULT_ALPHA}")
  )]
  pub alpha: Option<f32>,
  #[arg(
    short,
    long,
    help = format!("The radius of the opaque screen corners, default is {DEFAULT_RADIUS}")
  )]
  pub radius: Option<u32>,
}

fn main() {
  let args = DimlandArgs::parse();

  let conn = Connection::connect_to_env().expect("where are you running this");

  let (globals, mut event_queue) = registry_queue_init(&conn).expect("queueless");
  let qh = event_queue.handle();

  let compositor = CompositorState::bind(&globals, &qh).expect("no compositor :sukia:");
  let layer_shell = LayerShell::bind(&globals, &qh).expect("huh?");
  let shm = Shm::bind(&globals, &qh).expect("wl_shm is not available");

  let alpha = args.alpha.unwrap_or(DEFAULT_ALPHA);
  let radius = args.radius.unwrap_or(DEFAULT_RADIUS);
  let mut data = DimlandData::new(compositor, &globals, &qh, layer_shell, alpha, radius, shm);

  while !data.should_exit() {
    event_queue.blocking_dispatch(&mut data).expect("sus");
  }
}

pub struct DimlandData {
  compositor: CompositorState,
  registry_state: RegistryState,
  output_state: OutputState,
  layer_shell: LayerShell,
  viewporter: SimpleGlobal<WpViewporter, 1>,
  alpha: f32,
  radius: u32,
  views: Vec<DimlandView>,
  exit: bool,
  shm: Shm,
}

impl ShmHandler for DimlandData {
  fn shm_state(&mut self) -> &mut Shm {
    &mut self.shm
  }
}

struct DimlandView {
  first_configure: bool,
  width: u32,
  height: u32,
  buffer: WlBuffer,
  viewport: WpViewport,
  layer: LayerSurface,
  output: WlOutput,
}

impl DimlandData {
  pub fn new(
    compositor: CompositorState,
    globals: &GlobalList,
    qh: &QueueHandle<Self>,
    layer_shell: LayerShell,
    alpha: f32,
    radius: u32,
    shm: Shm,
  ) -> Self {
    Self {
      compositor,
      registry_state: RegistryState::new(globals),
      output_state: OutputState::new(globals, qh),
      layer_shell,
      viewporter: SimpleGlobal::<wp_viewporter::WpViewporter, 1>::bind(globals, qh)
        .expect("wp_viewporter not available"),
      radius,
      alpha,
      views: Vec::new(),
      exit: false,
      shm,
    }
  }

  pub fn should_exit(&self) -> bool {
    self.exit
  }

  fn create_view(&self, qh: &QueueHandle<Self>, output: WlOutput) -> DimlandView {
    let layer = self.layer_shell.create_layer_surface(
      qh,
      self.compositor.create_surface(qh),
      Layer::Overlay,
      Some("dimland_layer"),
      Some(&output),
    );

    let (width, height) = if let Some((width, height)) = self
      .output_state
      .info(&output)
      .and_then(|info| info.logical_size)
    {
      (width as u32, height as u32)
    } else {
      (0, 0)
    };

    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    let region = self.compositor.wl_compositor().create_region(qh, ());
    layer.set_input_region(Some(&region));
    layer.set_size(width, height);
    layer.commit();

    let viewport = self
      .viewporter
      .get()
      .expect("wp_viewporter failed")
      .get_viewport(layer.wl_surface(), qh, ());

    let mut pool = RawPool::new(width as usize * height as usize * 4, &self.shm).unwrap();
    let canvas = pool.mmap();

    // TODO: corner calc is kinda wrong?
    // see file:///stuff/screenshots/24-05-02T20-36-18.png
    // can't be bothered right now though for it is good enough

    {
      let corner_radius = self.radius;

      canvas
        .chunks_exact_mut(4)
        .enumerate()
        .for_each(|(index, chunk)| {
          let x = (index as u32) % width;
          let y = (index as u32) / width;

          let mut color = 0x00000000u32;
          let alpha = (self.alpha * 255.0) as u32;
          color |= alpha << 24;

          if (x < corner_radius
            && y < corner_radius
            && (corner_radius - x).pow(2) + (corner_radius - y).pow(2) > corner_radius.pow(2))
            || (x > width - corner_radius
              && y < corner_radius
              && (x - (width - corner_radius)).pow(2) + (corner_radius - y).pow(2)
                > corner_radius.pow(2))
            || (x < corner_radius
              && y > height - corner_radius
              && (corner_radius - x).pow(2) + (y - (height - corner_radius)).pow(2)
                > corner_radius.pow(2))
            || (x > width - corner_radius
              && y > height - corner_radius
              && (x - (width - corner_radius)).pow(2) + (y - (height - corner_radius)).pow(2)
                > corner_radius.pow(2))
          {
            color = 0xFF000000u32;
          }

          let array: &mut [u8; 4] = chunk.try_into().unwrap();
          *array = color.to_le_bytes();
        });
    }

    let buffer = pool.create_buffer(
      0,
      width as i32,
      height as i32,
      width as i32 * 4,
      Format::Argb8888,
      (),
      qh,
    );

    DimlandView::new(qh, buffer, viewport, layer, output)
  }
}

impl DimlandView {
  fn new(
    _qh: &QueueHandle<DimlandData>,
    buffer: WlBuffer,
    viewport: WpViewport,
    layer: LayerSurface,
    output: WlOutput,
  ) -> Self {
    Self {
      first_configure: true,
      width: 0,
      height: 0,
      buffer,
      viewport,
      layer,
      output,
    }
  }

  fn draw(&mut self, _qh: &QueueHandle<DimlandData>) {
    if !self.first_configure {
      return;
    }

    self.layer.wl_surface().attach(Some(&self.buffer), 0, 0);
    self.layer.commit();
  }
}

impl LayerShellHandler for DimlandData {
  fn closed(
    &mut self,
    _conn: &smithay_client_toolkit::reexports::client::Connection,
    _qh: &QueueHandle<Self>,
    _layer: &LayerSurface,
  ) {
    self.exit = true;
  }

  fn configure(
    &mut self,
    _conn: &smithay_client_toolkit::reexports::client::Connection,
    qh: &QueueHandle<Self>,
    layer: &LayerSurface,
    configure: smithay_client_toolkit::shell::wlr_layer::LayerSurfaceConfigure,
    _serial: u32,
  ) {
    let Some(view) = self.views.iter_mut().find(|view| &view.layer == layer) else {
      return;
    };

    (view.width, view.height) = configure.new_size;

    view
      .viewport
      .set_destination(view.width as _, view.height as _);

    if view.first_configure {
      view.draw(qh);
      view.first_configure = false;
    }
  }
}

impl OutputHandler for DimlandData {
  fn output_state(&mut self) -> &mut OutputState {
    &mut self.output_state
  }

  fn new_output(
    &mut self,
    _conn: &smithay_client_toolkit::reexports::client::Connection,
    qh: &QueueHandle<Self>,
    output: smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput,
  ) {
    self.views.push(self.create_view(qh, output));
  }

  fn update_output(
    &mut self,
    _conn: &smithay_client_toolkit::reexports::client::Connection,
    qh: &QueueHandle<Self>,
    output: smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput,
  ) {
    let new_view = self.create_view(qh, output);

    if let Some(view) = self.views.iter_mut().find(|v| v.output == new_view.output) {
      *view = new_view;
    }
  }

  fn output_destroyed(
    &mut self,
    _conn: &smithay_client_toolkit::reexports::client::Connection,
    _qh: &QueueHandle<Self>,
    output: smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput,
  ) {
    self.views.retain(|v| v.output != output);
  }
}

impl CompositorHandler for DimlandData {
  fn scale_factor_changed(
    &mut self,
    _conn: &smithay_client_toolkit::reexports::client::Connection,
    _qh: &QueueHandle<Self>,
    _surface: &smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface,
    _new_factor: i32,
  ) {
  }

  fn transform_changed(
    &mut self,
    _conn: &smithay_client_toolkit::reexports::client::Connection,
    _qh: &QueueHandle<Self>,
    _surface: &smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface,
    _new_transform: smithay_client_toolkit::reexports::client::protocol::wl_output::Transform,
  ) {
  }

  fn frame(
    &mut self,
    _conn: &smithay_client_toolkit::reexports::client::Connection,
    _qh: &QueueHandle<Self>,
    _surface: &smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface,
    _time: u32,
  ) {
  }
}

delegate_layer!(DimlandData);
delegate_output!(DimlandData);
delegate_registry!(DimlandData);
delegate_compositor!(DimlandData);
delegate_simple!(DimlandData, WpViewporter, 1);
delegate_shm!(DimlandData);

impl ProvidesRegistryState for DimlandData {
  fn registry(&mut self) -> &mut RegistryState {
    &mut self.registry_state
  }

  registry_handlers![OutputState];
}

impl Dispatch<WpViewport, ()> for DimlandData {
  fn event(
    _: &mut Self,
    _: &WpViewport,
    _: wp_viewport::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
  }
}

impl Dispatch<WlBuffer, ()> for DimlandData {
  fn event(
    _: &mut Self,
    _: &WlBuffer,
    _: wl_buffer::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
  }
}

impl Dispatch<WlRegion, ()> for DimlandData {
  fn event(
    _: &mut Self,
    _: &WlRegion,
    _: <WlRegion as smithay_client_toolkit::reexports::client::Proxy>::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
  }
}

impl Drop for DimlandView {
  fn drop(&mut self) {
    self.viewport.destroy();
    self.buffer.destroy();
  }
}
