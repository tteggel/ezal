//! picoserve web UI and WebSocket transport.
//!
//! `ezal-core::web` defines the static dashboard and wire protocol. This
//! module wires those pure pieces to picoserve, Embassy TCP sockets, and the
//! firmware task graph.

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU8, Ordering};

use embassy_executor::Spawner;
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use heapless::String;
use picoserve::futures::Either;
use picoserve::io::{Read, Write as PicoserveWrite};
use picoserve::response::ws::{Message, SocketRx, SocketTx, WebSocketCallback, WebSocketUpgrade};
use picoserve::routing::{get, PathRouter};

use ezal_core::web::{
    AzimuthDirection, ClientMessage, Command, ControlStatus, DriveCommand, ElevationDirection,
    PositionTelemetry, INDEX_HTML, TELEMETRY_PERIOD_MS, WS_PATH,
};

/// Shared state used by the dashboard routes and firmware tasks.
pub static STATE: WebState = WebState::new();

/// picoserve server configuration.
///
/// Mobile browsers often open speculative TCP connections and leave them idle.
/// Keep that accepted-but-unread window short so those sockets do not starve
/// real dashboard requests. Writes get a little more room than picoserve's
/// default because the CYW43 can be bursty under load.
static WEB_CONFIG: picoserve::Config = picoserve::Config::new(picoserve::Timeouts {
    start_read_request: Duration::from_millis(750),
    persistent_start_read_request: Duration::from_millis(250),
    read_request: Duration::from_secs(2),
    write: Duration::from_secs(4),
});

/// Concurrent picoserve acceptors. Each acceptor owns one TCP socket; with
/// `StackResources<10>` in `main`, DHCP uses one slot and the web pool has
/// enough headroom for multiple dashboards plus browser preconnect sockets.
const WEB_SERVER_TASKS: usize = 8;

/// Per-connection HTTP request parsing buffer.
const HTTP_BUFFER_SIZE: usize = 1536;

/// Per-connection TCP buffers.
const TCP_RX_BUFFER_SIZE: usize = 2048;
const TCP_TX_BUFFER_SIZE: usize = 4096;

/// Dashboard state that can be shared safely between Embassy tasks.
pub struct WebState {
    positions: SharedPositions,
    commands: Signal<CriticalSectionRawMutex, Command>,
    next_client_id: AtomicU32,
    controller_id: AtomicU32,
    drive: AtomicU8,
}

impl WebState {
    /// Create empty dashboard state.
    pub const fn new() -> Self {
        Self {
            positions: SharedPositions::new(),
            commands: Signal::new(),
            next_client_id: AtomicU32::new(1),
            controller_id: AtomicU32::new(0),
            drive: AtomicU8::new(encode_drive(DriveCommand::IDLE)),
        }
    }

    /// Store the latest raw ADC millivolt readings.
    pub fn store_position(&self, position: PositionTelemetry) {
        self.positions.store(position);
    }

    /// Read the latest raw ADC millivolt readings.
    pub fn position(&self) -> PositionTelemetry {
        self.positions.load()
    }

    /// Allocate a WebSocket connection id and assign initial control when no
    /// controller is active.
    fn register_client(&self) -> u32 {
        let id = self.next_client_id.fetch_add(1, Ordering::Relaxed);
        let id = if id == 0 {
            self.next_client_id.fetch_add(1, Ordering::Relaxed)
        } else {
            id
        };
        let _ = self
            .controller_id
            .compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire);
        id
    }

    /// Whether a WebSocket connection currently owns drive control.
    fn is_controller(&self, client_id: u32) -> bool {
        self.controller_id.load(Ordering::Acquire) == client_id
    }

    /// Return the control status as seen by one connection.
    fn control_status(&self, client_id: u32) -> ControlStatus {
        ControlStatus {
            controller: self.is_controller(client_id),
            drive: decode_drive(self.drive.load(Ordering::Acquire)),
        }
    }

    /// Make a connection the controller and stop any stale drive lease.
    fn take_control(&self, client_id: u32) {
        self.controller_id.store(client_id, Ordering::Release);
        self.apply_command(Command::Stop);
    }

    /// Release control if the disconnecting connection still owns it.
    fn release_client(&self, client_id: u32) {
        if self
            .controller_id
            .compare_exchange(client_id, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.apply_command(Command::Stop);
        }
    }

    /// Accept a client command only from the current controller.
    fn accept_command(&self, client_id: u32, command: Command) -> bool {
        if !self.is_controller(client_id) {
            return false;
        }

        self.apply_command(command);
        true
    }

    /// Signal a requested drive-state command to the task that owns the GPIOs.
    pub fn apply_command(&self, command: Command) {
        self.commands.signal(command);
    }

    /// Record the drive state currently applied to the output GPIOs.
    pub fn record_applied_drive(&self, drive: DriveCommand) {
        self.drive.store(encode_drive(drive), Ordering::Release);
    }

    /// Wait for the next drive-state command.
    pub async fn wait_command(&self) -> Command {
        self.commands.wait().await
    }
}

const fn encode_drive(drive: DriveCommand) -> u8 {
    let azimuth = match drive.azimuth {
        Some(AzimuthDirection::Clockwise) => 1,
        Some(AzimuthDirection::CounterClockwise) => 2,
        None => 0,
    };
    let elevation = match drive.elevation {
        Some(ElevationDirection::Up) => 1,
        Some(ElevationDirection::Down) => 2,
        None => 0,
    };

    azimuth | (elevation << 2)
}

const fn decode_drive(bits: u8) -> DriveCommand {
    DriveCommand {
        azimuth: match bits & 0b11 {
            1 => Some(AzimuthDirection::Clockwise),
            2 => Some(AzimuthDirection::CounterClockwise),
            _ => None,
        },
        elevation: match (bits >> 2) & 0b11 {
            1 => Some(ElevationDirection::Up),
            2 => Some(ElevationDirection::Down),
            _ => None,
        },
    }
}

/// Atomic position snapshot shared between the ADC task and WebSocket clients.
struct SharedPositions {
    a0_mv: AtomicI32,
    a1_mv: AtomicI32,
}

impl SharedPositions {
    const fn new() -> Self {
        Self {
            a0_mv: AtomicI32::new(PositionTelemetry::ZERO.a0_mv),
            a1_mv: AtomicI32::new(PositionTelemetry::ZERO.a1_mv),
        }
    }

    fn store(&self, position: PositionTelemetry) {
        self.a0_mv.store(position.a0_mv, Ordering::Relaxed);
        self.a1_mv.store(position.a1_mv, Ordering::Relaxed);
    }

    fn load(&self) -> PositionTelemetry {
        PositionTelemetry {
            a0_mv: self.a0_mv.load(Ordering::Relaxed),
            a1_mv: self.a1_mv.load(Ordering::Relaxed),
        }
    }
}

/// Build the dashboard app.
pub fn app(state: &'static WebState) -> picoserve::Router<impl PathRouter> {
    picoserve::Router::new()
        .route(
            "/",
            get(async || (("Content-Type", "text/html; charset=utf-8"), INDEX_HTML)),
        )
        .route(
            WS_PATH,
            get(async move |upgrade: WebSocketUpgrade| {
                upgrade.on_upgrade(DashboardSocket { state })
            }),
        )
}

/// Run the HTTP/WebSocket server forever.
pub async fn serve(spawner: Spawner, stack: Stack<'static>) -> ! {
    for task_id in 0..WEB_SERVER_TASKS {
        spawner.must_spawn(web_task(task_id as u8, stack));
    }

    defmt::info!("ezal web: started {=usize} acceptors", WEB_SERVER_TASKS);

    loop {
        Timer::after(Duration::from_secs(3600)).await;
    }
}

#[embassy_executor::task(pool_size = WEB_SERVER_TASKS)]
async fn web_task(task_id: u8, stack: Stack<'static>) -> ! {
    let app = app(&STATE);
    let mut http_buffer = [0; HTTP_BUFFER_SIZE];
    let mut tcp_rx_buffer = [0; TCP_RX_BUFFER_SIZE];
    let mut tcp_tx_buffer = [0; TCP_TX_BUFFER_SIZE];

    match picoserve::Server::new(&app, &WEB_CONFIG, &mut http_buffer)
        .listen_and_serve(task_id, stack, 80, &mut tcp_rx_buffer, &mut tcp_tx_buffer)
        .await {}
}

async fn send_control_status<W: PicoserveWrite>(
    tx: &mut SocketTx<W>,
    state: &'static WebState,
    client_id: u32,
) -> Result<(), W::Error> {
    let mut payload: String<128> = String::new();
    if state
        .control_status(client_id)
        .write_json(&mut payload)
        .is_ok()
    {
        tx.send_text(&payload).await?;
    }
    Ok(())
}

async fn send_position<W: PicoserveWrite>(
    tx: &mut SocketTx<W>,
    state: &'static WebState,
) -> Result<(), W::Error> {
    let mut payload: String<96> = String::new();
    if state.position().write_json(&mut payload).is_ok() {
        tx.send_text(&payload).await?;
    }
    Ok(())
}

struct DashboardSocket {
    state: &'static WebState,
}

impl WebSocketCallback for DashboardSocket {
    async fn run<R: Read, W: PicoserveWrite<Error = R::Error>>(
        self,
        mut rx: SocketRx<R>,
        mut tx: SocketTx<W>,
    ) -> Result<(), W::Error> {
        let state = self.state;
        let client_id = state.register_client();
        let mut read_buffer = [0; 64];
        let telemetry_period = Duration::from_millis(TELEMETRY_PERIOD_MS);
        let mut next_telemetry = Instant::now() + telemetry_period;

        if let Err(error) = send_control_status(&mut tx, state, client_id).await {
            state.release_client(client_id);
            return Err(error);
        }

        loop {
            let event = match rx
                .next_message(&mut read_buffer, Timer::at(next_telemetry))
                .await
            {
                Ok(event) => event,
                Err(error) => {
                    state.release_client(client_id);
                    return Err(error);
                }
            };

            match event {
                Either::First(Ok(Message::Text(message))) => {
                    match ClientMessage::parse(message) {
                        Some(ClientMessage::TakeControl) => state.take_control(client_id),
                        Some(ClientMessage::Command(command)) => {
                            let _ = state.accept_command(client_id, command);
                        }
                        None if message.starts_with("drive:") && state.is_controller(client_id) => {
                            state.apply_command(Command::Stop);
                        }
                        None => {}
                    }

                    if let Err(error) = send_control_status(&mut tx, state, client_id).await {
                        state.release_client(client_id);
                        return Err(error);
                    }
                }
                Either::First(Ok(Message::Binary(_))) | Either::First(Ok(Message::Pong(_))) => {}
                Either::First(Ok(Message::Ping(data))) => {
                    if let Err(error) = tx.send_pong(data).await {
                        state.release_client(client_id);
                        return Err(error);
                    }
                }
                Either::First(Ok(Message::Close(reason))) => {
                    state.release_client(client_id);
                    return tx.close(reason).await;
                }
                Either::First(Err(error)) => {
                    state.release_client(client_id);
                    return tx.close(Some((error.code(), "invalid message"))).await;
                }
                Either::Second(()) => {
                    if let Err(error) = send_position(&mut tx, state).await {
                        state.release_client(client_id);
                        return Err(error);
                    }
                    if let Err(error) = send_control_status(&mut tx, state, client_id).await {
                        state.release_client(client_id);
                        return Err(error);
                    }

                    while next_telemetry <= Instant::now() {
                        next_telemetry += telemetry_period;
                    }
                }
            }
        }
    }
}
