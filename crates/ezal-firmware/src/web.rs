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
use ezal_core::protocol::{ClientMessage, CommandFreshness, TELEMETRY_PERIOD_MS, WS_PATH};

use crate::state::{SharedState, STATE};

#[path = "ws_receive.rs"]
mod ws_receive;

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
        spawner.spawn(web_task(task_id as u8, stack).expect("web task pool exhausted"));
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
    let mut payload: String<256> = String::new();
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

async fn send_tracking<W: PicoserveWrite>(
    tx: &mut SocketTx<W>,
    state: &'static SharedState,
) -> Result<(), W::Error> {
    let mut payload: String<384> = String::new();
    if state.tracking().write_json(&mut payload).is_ok() {
        tx.send_text(&payload).await?;
    }
    Ok(())
}

/// Complete one application telemetry batch before issuing its freshness
/// proof. Movement acknowledgements deliberately do not issue tokens. A
/// control transfer receives a complete fresh batch, as do periodic ticks.
async fn send_telemetry<W: PicoserveWrite>(
    tx: &mut SocketTx<W>,
    state: &'static SharedState,
    client_id: u32,
    freshness: &mut CommandFreshness,
) -> Result<(), W::Error> {
    // Timestamp before observing or writing the batch. A slow TCP write must
    // not make already delayed telemetry eligible for a brand-new lease.
    let now_ms = Instant::now().as_millis();
    let network = state.network();
    freshness.observe_network(network.generation(), network.ready(now_ms));
    let token = freshness.issue(now_ms);
    send_position(tx, state).await?;
    send_control_status(tx, state, client_id).await?;
    send_tracking(tx, state).await?;
    if let Some(token) = token {
        let mut payload: String<64> = String::new();
        if token.write_json(&mut payload).is_ok() {
            tx.send_text(&payload).await?;
        }
    }
    Ok(())
}

struct DashboardSocket {
    state: &'static SharedState,
}

/// A registered dashboard connection. Dropping it releases manual control,
/// however the callback ends: a returned error, a close, a receive timeout, or
/// the server cancelling the whole future.
struct ClientSession {
    state: &'static SharedState,
    id: u32,
}

impl ClientSession {
    fn register(state: &'static SharedState) -> Self {
        Self {
            state,
            id: state.register_client(),
        }
    }
}

impl Drop for ClientSession {
    fn drop(&mut self) {
        self.state.release_client(self.id);
    }
}

impl WebSocketCallback for DashboardSocket {
    async fn run<R: Read, W: PicoserveWrite<Error = R::Error>>(
        self,
        mut rx: SocketRx<R>,
        mut tx: SocketTx<W>,
    ) -> Result<(), W::Error> {
        let state = self.state;
        let session = ClientSession::register(state);
        let client_id = session.id;
        let mut freshness = CommandFreshness::new(client_id);
        let mut read_buffer = [0; ws_receive::MESSAGE_BUFFER_SIZE];
        let telemetry_period = Duration::from_millis(TELEMETRY_PERIOD_MS);
        let mut next_telemetry = Instant::now() + telemetry_period;

        send_telemetry(&mut tx, state, client_id, &mut freshness).await?;

        loop {
            let event = match ws_receive::next_message(
                &mut rx,
                &mut tx,
                &mut read_buffer,
                &picoserve::time::EmbassyTimer,
                Timer::at(next_telemetry),
            )
            .await
            {
                Ok(Ok(event)) => event,
                Ok(Err(error)) => return Err(error),
                // A partial frame cannot be retried after cancellation. End the
                // connection before picoserve's bounded TCP shutdown; do not
                // wait for a WebSocket close handshake from this peer.
                Err(_) => return Ok(()),
            };

            // Link recovery invalidates even commands already buffered on an
            // established TCP connection. Checking before token issuance too
            // lets only post-recovery observations authorize a new press.
            let network = state.network();
            freshness.observe_network(
                network.generation(),
                network.ready(Instant::now().as_millis()),
            );

            match event {
                Either::First(Ok(Message::Text(message))) => {
                    let parsed = ClientMessage::parse(message);
                    let drive_signaled = match parsed {
                        Some(ClientMessage::TakeControl) => {
                            freshness.reject();
                            state.take_control(client_id);
                            tx.send_text("{\"type\":\"input_required\"}").await?;
                            true
                        }
                        Some(ClientMessage::Command(command)) => {
                            freshness.disarm();
                            state.accept_command(client_id, command)
                        }
                        Some(ClientMessage::Movement(request)) => {
                            let now_ms = Instant::now().as_millis();
                            if let Some(deadline) = freshness.admit(request, now_ms) {
                                state.accept_command_until(
                                    client_id,
                                    Command::Drive(request.drive),
                                    deadline,
                                )
                            } else {
                                let signaled = state.accept_command(client_id, Command::Inhibit);
                                // Clear the browser hold too; a newly issued
                                // token must not turn a timer refresh into a
                                // new operator press after this rejection.
                                tx.send_text("{\"type\":\"input_required\"}").await?;
                                signaled
                            }
                        }
                        None => {
                            freshness.reject();
                            let signaled = state.accept_command(client_id, Command::Inhibit);
                            tx.send_text("{\"type\":\"input_required\"}").await?;
                            signaled
                        }
                    };

                    if drive_signaled {
                        yield_now().await;
                    }

                    if parsed == Some(ClientMessage::TakeControl) {
                        send_telemetry(&mut tx, state, client_id, &mut freshness).await?;
                    } else {
                        send_control_status(&mut tx, state, client_id).await?;
                    }
                }
                Either::First(Ok(Message::Binary(_))) => {
                    freshness.reject();
                    if state.accept_command(client_id, Command::Inhibit) {
                        yield_now().await;
                    }
                }
                Either::First(Ok(Message::Pong(_))) => {}
                Either::First(Ok(Message::Ping(data))) => tx.send_pong(data).await?,
                Either::First(Ok(Message::Close(reason))) => {
                    // Revoke motion before awaiting the close response: a peer
                    // that stopped reading must not keep ownership or motion
                    // alive for the socket's write timeout.
                    drop(session);
                    return tx.close(reason).await;
                }
                Either::First(Err(error)) => {
                    // Framing errors end this command source immediately too.
                    drop(session);
                    return tx.close(Some((error.code(), "invalid message"))).await;
                }
                Either::Second(()) => {
                    send_telemetry(&mut tx, state, client_id, &mut freshness).await?;

                    while next_telemetry <= Instant::now() {
                        next_telemetry += telemetry_period;
                    }
                }
            }
        }
    }
}
