//! picoserve HTTP server and WebSocket transport.
//!
//! Serves the [`ezal_core::dashboard`] asset and speaks the
//! [`ezal_core::protocol`] wire format over picoserve and Embassy TCP sockets.
//! The shared state these routes read and write — control ownership, the
//! command bus, and the latest telemetry — lives in [`crate::state`].

use core::cell::RefCell;
use core::fmt;

use embassy_executor::Spawner;
use embassy_futures::yield_now;
use embassy_net::Stack;
use embassy_time::{Duration, Instant, Timer};
use heapless::String;
use picoserve::io::{Read, Write as PicoserveWrite};
use picoserve::response::ws::{Message, SocketRx, SocketTx, WebSocketCallback, WebSocketUpgrade};
use picoserve::response::{Content, Response};
use picoserve::routing::{get, PathRouter};

use ezal_core::dashboard::{ClientConfig, CONFIG_PATH, INDEX_HTML};
use ezal_core::drive::Command;
use ezal_core::mailbox::CommandEnvelope;
use ezal_core::passes::Report;
use ezal_core::protocol::{ClientMessage, TelemetryMessage, TELEMETRY_CAPACITY, WS_PATH};
use ezal_core::session::{CommandFreshness, TELEMETRY_PERIOD_MS};

use crate::state::{SharedState, STATE};

#[path = "ws_receive.rs"]
mod ws_receive;
use ws_receive::{ReceiveError, ReceiveEvent};

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
/// `StackResources<14>` in `main`, background network services and the web pool have
/// enough headroom for multiple dashboards plus browser preconnect sockets.
const WEB_SERVER_TASKS: usize = 8;

/// Per-connection HTTP request parsing buffer.
const HTTP_BUFFER_SIZE: usize = 1536;

/// Per-connection TCP buffers.
const TCP_RX_BUFFER_SIZE: usize = 2048;
const TCP_TX_BUFFER_SIZE: usize = 4096;

/// Build the dashboard app. `report` is the owning acceptor task's slot for
/// the `/api/passes` reply it is currently sending.
pub fn app<'a>(
    state: &'static SharedState,
    report: &'a RefCell<Option<Report>>,
) -> picoserve::Router<impl PathRouter + 'a> {
    picoserve::Router::new()
        .route(
            "/",
            get(async || (("Content-Type", "text/html; charset=utf-8"), INDEX_HTML)),
        )
        .route(
            CONFIG_PATH,
            get(async || Response::ok(DashboardConfig).with_header("Cache-Control", "no-store")),
        )
        .route(
            "/api/passes",
            get(async move || {
                report.replace(Some(crate::prediction::report()));
                Response::ok(ForecastJson(report)).with_header("Cache-Control", "no-store")
            }),
        )
        .route(
            WS_PATH,
            get(async move |upgrade: WebSocketUpgrade| {
                upgrade.on_upgrade(DashboardSocket { state })
            }),
        )
}

struct DashboardConfig;

impl Content for DashboardConfig {
    fn content_type(&self) -> &'static str {
        "text/javascript; charset=utf-8"
    }

    fn content_length(&self) -> usize {
        format_args!("{}", ClientConfig).content_length()
    }

    async fn write_content<W: PicoserveWrite>(self, mut writer: W) -> Result<(), W::Error> {
        writer.write_fmt(format_args!("{}", ClientConfig)).await
    }
}

// A request copies the dashboard `Report` (about 1.5 KiB) into its acceptor
// task's slot, then formats JSON from the slot straight into the TCP transmit
// buffer. Response futures hold only a reference: carrying the report by value
// would let each nested future keep its own copy in every task. No buffer or
// lock is shared between requests, so a client that stops acknowledging
// delays only its own reply. A task serves one request at a time, so the slot
// cannot change while it is borrowed here.
struct ForecastJson<'a>(&'a RefCell<Option<Report>>);

impl fmt::Display for ForecastJson<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &*self.0.borrow() {
            Some(report) => report.fmt(out),
            None => Ok(()),
        }
    }
}

impl Content for ForecastJson<'_> {
    fn content_type(&self) -> &'static str {
        "application/json"
    }

    fn content_length(&self) -> usize {
        self.0.borrow().as_ref().map_or(0, Report::json_len)
    }

    async fn write_content<W: PicoserveWrite>(self, mut writer: W) -> Result<(), W::Error> {
        // Formatting is deterministic, so a retry after a full transmit
        // buffer reproduces exactly the bytes `content_length` counted.
        writer.write_fmt(format_args!("{self}")).await
    }
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
    // This acceptor's `/api/passes` reply, borrowed by the router.
    let report = RefCell::new(None);
    let app = app(&STATE, &report);
    let mut http_buffer = [0; HTTP_BUFFER_SIZE];
    let mut tcp_rx_buffer = [0; TCP_RX_BUFFER_SIZE];
    let mut tcp_tx_buffer = [0; TCP_TX_BUFFER_SIZE];

    match picoserve::Server::new(&app, &WEB_CONFIG, &mut http_buffer)
        .listen_and_serve(task_id, stack, 80, &mut tcp_rx_buffer, &mut tcp_tx_buffer)
        .await {}
}

/// A failed frame cannot be silently skipped while leaving its source alive.
enum SocketError<E> {
    Io(E),
    Encoding,
}

impl<E> From<E> for SocketError<E> {
    fn from(error: E) -> Self {
        Self::Io(error)
    }
}

async fn send_control_status<W: PicoserveWrite>(
    tx: &mut SocketTx<W>,
    state: &'static SharedState,
    client_id: u32,
) -> Result<(), SocketError<W::Error>> {
    let mut payload: String<256> = String::new();
    state
        .control_status(client_id)
        .write_json(&mut payload)
        .map_err(|_| SocketError::Encoding)?;
    tx.send_text(&payload).await?;
    Ok(())
}

/// Send one observation with its proof. Acknowledgements do not issue proofs.
async fn send_telemetry<W: PicoserveWrite>(
    tx: &mut SocketTx<W>,
    state: &'static SharedState,
    client_id: u32,
    freshness: &mut CommandFreshness,
) -> Result<(), SocketError<W::Error>> {
    // Stamp before the state read and socket write. Slow delivery consumes the
    // original token lifetime instead of granting a fresh lease on completion.
    let now_ms = Instant::now().as_millis();
    let snapshot = state.telemetry(client_id, now_ms);
    freshness.observe_network(
        snapshot.network.generation(),
        snapshot.control.network_ready,
    );
    let message = TelemetryMessage {
        snapshot,
        token: freshness.issue(now_ms),
    };
    let mut payload: String<TELEMETRY_CAPACITY> = String::new();
    message
        .write_json(&mut payload)
        .map_err(|_| SocketError::Encoding)?;
    tx.send_text(&payload).await?;
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
        rx: SocketRx<R>,
        tx: SocketTx<W>,
    ) -> Result<(), W::Error> {
        match self.run_session(rx, tx).await {
            Ok(()) => Ok(()),
            Err(SocketError::Io(error)) => Err(error),
            Err(SocketError::Encoding) => {
                // run_session has dropped its ownership guard before returning.
                defmt::error!("dashboard telemetry exceeded its bounded buffer; closing session");
                Ok(())
            }
        }
    }
}

impl DashboardSocket {
    async fn run_session<R: Read, W: PicoserveWrite<Error = R::Error>>(
        self,
        mut rx: SocketRx<R>,
        mut tx: SocketTx<W>,
    ) -> Result<(), SocketError<W::Error>> {
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
                Ok(event) => event,
                Err(ReceiveError::Io(error)) => return Err(SocketError::Io(error)),
                Err(ReceiveError::Protocol(error)) => {
                    drop(session);
                    return tx
                        .close(Some((error.code(), "invalid message")))
                        .await
                        .map_err(SocketError::Io);
                }
                // A partial frame cannot be retried after cancellation. End the
                // connection before picoserve's bounded TCP shutdown; do not
                // wait for a WebSocket close handshake from this peer.
                Err(ReceiveError::TimedOut) => return Ok(()),
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
                ReceiveEvent::Message(Message::Text(message)) => {
                    let parsed = ClientMessage::parse(message);
                    let drive_signaled = match parsed {
                        Some(ClientMessage::TakeControl) => {
                            freshness.reject();
                            state.take_control(client_id);
                            tx.send_text("{\"type\":\"input_required\"}").await?;
                            true
                        }
                        Some(ClientMessage::Stop) => {
                            freshness.disarm();
                            state.accept_command(client_id, CommandEnvelope::INHIBIT)
                        }
                        Some(ClientMessage::Movement(request)) => {
                            let now_ms = Instant::now().as_millis();
                            if let Some(deadline) = freshness.admit(request, now_ms) {
                                state.accept_command(
                                    client_id,
                                    CommandEnvelope {
                                        command: Command::Jog(request.drive),
                                        valid_until_ms: deadline,
                                    },
                                )
                            } else {
                                let signaled =
                                    state.accept_command(client_id, CommandEnvelope::INHIBIT);
                                // Clear the browser hold too; a newly issued
                                // token must not turn a timer refresh into a
                                // new operator press after this rejection.
                                tx.send_text("{\"type\":\"input_required\"}").await?;
                                signaled
                            }
                        }
                        None => {
                            freshness.reject();
                            let signaled =
                                state.accept_command(client_id, CommandEnvelope::INHIBIT);
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
                ReceiveEvent::Message(Message::Binary(_)) => {
                    freshness.reject();
                    if state.accept_command(client_id, CommandEnvelope::INHIBIT) {
                        yield_now().await;
                    }
                }
                ReceiveEvent::Message(Message::Pong(_)) => {}
                ReceiveEvent::Message(Message::Ping(data)) => tx.send_pong(data).await?,
                ReceiveEvent::Message(Message::Close(reason)) => {
                    // Revoke motion before awaiting the close response: a peer
                    // that stopped reading must not keep ownership or motion
                    // alive for the socket's write timeout.
                    drop(session);
                    return tx.close(reason).await.map_err(SocketError::Io);
                }
                ReceiveEvent::Telemetry => {
                    send_telemetry(&mut tx, state, client_id, &mut freshness).await?;

                    while next_telemetry <= Instant::now() {
                        next_telemetry += telemetry_period;
                    }
                }
            }
        }
    }
}
