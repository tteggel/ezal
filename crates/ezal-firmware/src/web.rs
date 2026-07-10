//! picoserve HTTP server and WebSocket transport.
//!
//! Serves the [`ezal_core::dashboard`] asset and speaks the
//! [`ezal_core::protocol`] wire format over picoserve and Embassy TCP sockets.
//! The shared state these routes read and write — control ownership, the
//! command bus, and the latest telemetry — lives in [`crate::state`].

use embassy_executor::Spawner;
use embassy_futures::yield_now;
use embassy_net::Stack;
use embassy_time::{Duration, Instant, Timer};
use heapless::String;
use picoserve::futures::Either;
use picoserve::io::{Read, Write as PicoserveWrite};
use picoserve::response::ws::{Message, SocketRx, SocketTx, WebSocketCallback, WebSocketUpgrade};
use picoserve::routing::{get, PathRouter};

use ezal_core::dashboard::INDEX_HTML;
use ezal_core::drive::Command;
use ezal_core::protocol::{ClientMessage, TELEMETRY_PERIOD_MS, WS_PATH};

use crate::state::{SharedState, STATE};

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

/// Build the dashboard app.
pub fn app(state: &'static SharedState) -> picoserve::Router<impl PathRouter> {
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
    state: &'static SharedState,
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
    state: &'static SharedState,
) -> Result<(), W::Error> {
    let mut payload: String<96> = String::new();
    if state.position().write_json(&mut payload).is_ok() {
        tx.send_text(&payload).await?;
    }
    Ok(())
}

struct DashboardSocket {
    state: &'static SharedState,
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
                    let drive_signaled = match ClientMessage::parse(message) {
                        Some(ClientMessage::TakeControl) => {
                            state.take_control(client_id);
                            true
                        }
                        Some(ClientMessage::Command(command)) => {
                            state.accept_command(client_id, command)
                        }
                        None if message.starts_with("drive:") && state.is_controller(client_id) => {
                            state.apply_command(Command::Stop);
                            true
                        }
                        None => false,
                    };

                    if drive_signaled {
                        yield_now().await;
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
