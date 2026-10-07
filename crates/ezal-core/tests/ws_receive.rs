//! Run the firmware receive adapter with the pinned picoserve HTTP upgrade and
//! WebSocket parser. Only socket I/O and the clock are faked; parser state,
//! fragmentation, and deadline placement are the production implementation.

#[path = "../../ezal-firmware/src/ws_receive.rs"]
mod ws_receive;

use core::future::{poll_fn, Future};
use core::pin::pin;
use core::task::{Context, Poll, Waker};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::rc::Rc;

use picoserve::futures::Either;
use picoserve::io::{BaseWrite, ErrorType, Read, Socket, Write};
use picoserve::response::ws::{Message, SocketRx, SocketTx, WebSocketCallback, WebSocketUpgrade};
use picoserve::routing::get;
use picoserve::time::{Duration, TimeoutError, Timer};

struct TestRuntime;

#[derive(Clone, Default)]
struct Clock(Rc<Cell<u64>>);

impl Timer<TestRuntime> for Clock {
    async fn delay(&self, duration: Duration) {
        let deadline = self.0.get() + duration.as_millis();
        poll_fn(|_| {
            if self.0.get() >= deadline {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }

    async fn run_with_timeout<F: Future>(
        &self,
        duration: Duration,
        future: F,
    ) -> Result<F::Output, TimeoutError> {
        let mut future = pin!(future);
        let deadline = self.0.get() + duration.as_millis();
        poll_fn(|cx| match future.as_mut().poll(cx) {
            Poll::Ready(output) => Poll::Ready(Ok(output)),
            Poll::Pending if self.0.get() >= deadline => Poll::Ready(Err(TimeoutError)),
            Poll::Pending => Poll::Pending,
        })
        .await
    }
}

struct Reader {
    clock: Clock,
    chunks: VecDeque<(u64, VecDeque<u8>)>,
}

impl ErrorType for Reader {
    type Error = Infallible;
}

impl Read for Reader {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        poll_fn(|_| {
            let Some((at, chunk)) = self.chunks.front_mut() else {
                // A stalled peer keeps TCP open; it does not send EOF.
                return Poll::Pending;
            };
            if *at > self.clock.0.get() {
                return Poll::Pending;
            }
            let count = buffer.len().min(chunk.len());
            for slot in &mut buffer[..count] {
                *slot = chunk.pop_front().unwrap();
            }
            if chunk.is_empty() {
                self.chunks.pop_front();
            }
            Poll::Ready(Ok(count))
        })
        .await
    }
}

struct Writer(Rc<RefCell<Vec<u8>>>);

impl ErrorType for Writer {
    type Error = Infallible;
}

impl BaseWrite for Writer {
    async fn write(&mut self, bytes: &[u8]) -> Result<usize, Self::Error> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Write for Writer {
    async fn write_with<F: FnOnce(picoserve::mem::BorrowedCursor<'_>) -> R, R>(
        &mut self,
        f: F,
    ) -> Result<R, Self::Error> {
        let mut buffer = [0; 512];
        let mut buffer = picoserve::mem::BorrowedBuffer::new(&mut buffer);
        let output = f(buffer.unfilled());
        self.0.borrow_mut().extend_from_slice(buffer.filled());
        Ok(output)
    }
}

struct TestSocket {
    reader: Reader,
    writer: Writer,
    closed: Rc<Cell<bool>>,
}

impl Socket<TestRuntime> for TestSocket {
    type Error = Infallible;
    type ReadHalf<'a> = &'a mut Reader;
    type WriteHalf<'a> = &'a mut Writer;

    fn split(&mut self) -> (Self::ReadHalf<'_>, Self::WriteHalf<'_>) {
        (&mut self.reader, &mut self.writer)
    }

    async fn abort<T: Timer<TestRuntime>>(
        self,
        _: &picoserve::Timeouts,
        _: &T,
    ) -> Result<(), picoserve::Error<Self::Error>> {
        self.closed.set(true);
        Ok(())
    }

    async fn shutdown<T: Timer<TestRuntime>>(
        self,
        _: &picoserve::Timeouts,
        _: &T,
    ) -> Result<(), picoserve::Error<Self::Error>> {
        // The real embassy socket closes, then waits up to the `read_request`
        // timeout for the peer's FIN and up to the `write` timeout to flush,
        // so an acceptor can remain busy for seconds longer than this. That
        // teardown belongs to the transport; this fake only records that the
        // server actually exits and recycles the connection.
        self.closed.set(true);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Event {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Close,
    ProtocolError(u16),
    Telemetry,
    Timeout,
}

struct Receiver {
    clock: Clock,
    events: Rc<RefCell<Vec<(u64, Event)>>>,
    /// Stop taking telemetry ticks once the clock reaches this, so a test can
    /// choose how long the connection stays idle before its message arrives.
    telemetry_until_ms: u64,
}

impl WebSocketCallback for Receiver {
    async fn run<R: Read, W: Write<Error = R::Error>>(
        self,
        mut rx: SocketRx<R>,
        mut tx: SocketTx<W>,
    ) -> Result<(), W::Error> {
        let mut buffer = [0; ws_receive::MESSAGE_BUFFER_SIZE];
        loop {
            let result = ws_receive::next_message(
                &mut rx,
                &mut tx,
                &mut buffer,
                &self.clock,
                self.clock.delay(Duration::from_millis(200)),
            )
            .await;
            let (event, done) = match result {
                Ok(Ok(Either::First(Ok(Message::Text(text))))) => (Event::Text(text.into()), true),
                Ok(Ok(Either::First(Ok(Message::Ping(data))))) => (Event::Ping(data.into()), true),
                Ok(Ok(Either::First(Ok(Message::Binary(data))))) => {
                    (Event::Binary(data.into()), true)
                }
                Ok(Ok(Either::First(Ok(Message::Close(_))))) => (Event::Close, true),
                Ok(Ok(Either::First(Err(error)))) => (Event::ProtocolError(error.code()), true),
                Ok(Ok(Either::Second(()))) => (
                    Event::Telemetry,
                    self.clock.0.get() >= self.telemetry_until_ms,
                ),
                Err(_) => (Event::Timeout, true),
                _ => panic!("unexpected read or protocol error"),
            };
            self.events.borrow_mut().push((self.clock.0.get(), event));
            if done {
                return Ok(());
            }
        }
    }
}

fn run(chunks: Vec<(u64, Vec<u8>)>) -> Vec<(u64, Event)> {
    run_until(chunks, 600, 2_100)
}

fn run_until(
    chunks: Vec<(u64, Vec<u8>)>,
    telemetry_until_ms: u64,
    limit_ms: u64,
) -> Vec<(u64, Event)> {
    run_capture(chunks, telemetry_until_ms, limit_ms).0
}

fn run_capture(
    chunks: Vec<(u64, Vec<u8>)>,
    telemetry_until_ms: u64,
    limit_ms: u64,
) -> (Vec<(u64, Event)>, Vec<u8>) {
    let clock = Clock::default();
    let events = Rc::new(RefCell::new(Vec::new()));
    let wire = Rc::new(RefCell::new(Vec::new()));
    let closed = Rc::new(Cell::new(false));
    let app = picoserve::Router::new().route(
        "/ws",
        get({
            let clock = clock.clone();
            let events = events.clone();
            async move |upgrade: WebSocketUpgrade| {
                upgrade.on_upgrade(Receiver {
                    clock: clock.clone(),
                    events: events.clone(),
                    telemetry_until_ms,
                })
            }
        }),
    );
    let request = b"GET /ws HTTP/1.1\r\nHost: ezal.local\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
    let chunks = std::iter::once((0, request.to_vec()))
        .chain(chunks)
        .map(|(at, bytes)| (at, bytes.into()))
        .collect();
    let socket = TestSocket {
        reader: Reader {
            clock: clock.clone(),
            chunks,
        },
        writer: Writer(wire.clone()),
        closed: closed.clone(),
    };
    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: Duration::from_secs(1),
        persistent_start_read_request: Duration::from_secs(1),
        read_request: Duration::from_secs(2),
        write: Duration::from_secs(4),
    });
    let mut buffer = [0; 512];
    let server = picoserve::Server::custom(&app, clock.clone(), &config, &mut buffer);
    let mut serving = pin!(server.serve(socket));
    let mut cx = Context::from_waker(Waker::noop());
    // Drive virtual milliseconds explicitly; the test has no OS timer or
    // executor dependency, and a missing outer timeout fails at a finite bound.
    let mut finished = false;
    for now in 0..=limit_ms {
        clock.0.set(now);
        if let Poll::Ready(result) = serving.as_mut().poll(&mut cx) {
            assert!(
                result.is_ok(),
                "HTTP/WebSocket server must exit successfully"
            );
            finished = true;
            break;
        }
    }
    assert!(
        finished,
        "a stalled message retained its acceptor past the deadline"
    );
    assert!(closed.get());
    assert!(
        wire.borrow().starts_with(b"HTTP/1.1 101"),
        "WebSocket upgrade failed: {}",
        String::from_utf8_lossy(&wire.borrow())
    );
    let result = events.borrow().clone();
    let wire = wire.borrow().clone();
    (result, wire)
}

fn frame(final_frame: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mask = [0x12, 0x34, 0x56, 0x78];
    let mut bytes = vec![
        if final_frame { 0x80 | opcode } else { opcode },
        0x80 | payload.len() as u8,
    ];
    bytes.extend(mask);
    bytes.extend(
        payload
            .iter()
            .enumerate()
            .map(|(i, byte)| byte ^ mask[i % 4]),
    );
    bytes
}

#[test]
fn maximum_sized_control_frame_fits_the_production_receive_buffer() {
    let payload = vec![0xA5; 125];
    assert_eq!(
        run(vec![(0, frame(true, 9, &payload))]),
        vec![(0, Event::Ping(payload))]
    );
}

#[test]
fn partial_header_and_payload_timeout_after_message_start() {
    let mut payload = frame(true, 1, b"drive:cw");
    payload.truncate(payload.len() - 2);
    for partial in [vec![0x81], payload] {
        assert_eq!(run(vec![(0, partial)]), vec![(2000, Event::Timeout)]);
    }
}

#[test]
fn continuation_trickle_cannot_restart_whole_message_deadline() {
    let chunks = vec![
        (0, frame(false, 1, b"drive:")),
        (1000, frame(false, 0, b"")),
        (1999, frame(false, 0, b"")),
    ];
    assert_eq!(run(chunks), vec![(2000, Event::Timeout)]);
}

#[test]
fn complete_fragmented_message_before_deadline_is_delivered() {
    let chunks = vec![
        (0, frame(false, 1, b"drive:")),
        (1900, frame(true, 0, b"cw")),
    ];
    assert_eq!(run(chunks), vec![(1900, Event::Text("drive:cw".into()))]);
}

#[test]
fn idle_connection_keeps_telemetry_ticks_and_can_receive_later() {
    assert_eq!(
        run(vec![]),
        vec![
            (200, Event::Telemetry),
            (400, Event::Telemetry),
            (600, Event::Telemetry)
        ]
    );
    assert_eq!(
        run(vec![(300, frame(true, 1, b"control:take"))]),
        vec![
            (200, Event::Telemetry),
            (300, Event::Text("control:take".into()))
        ]
    );
}

#[test]
fn an_idle_connection_outlives_the_message_deadline_and_still_receives() {
    // The deadline covers one receive, not the connection: an idle dashboard
    // keeps its telemetry ticks and its manual control well past 2 s.
    let events = run_until(vec![(2_500, frame(true, 1, b"control:take"))], 2_450, 2_600);
    assert_eq!(
        events.last(),
        Some(&(2_500, Event::Text("control:take".into())))
    );
    assert_eq!(
        events
            .iter()
            .filter(|(_, event)| *event == Event::Telemetry)
            .count(),
        12
    );
}

#[test]
fn a_stall_that_starts_late_times_out_from_its_own_receive() {
    // A message that starts at 2500, during the receive begun by the 2400 ms
    // telemetry tick, times out 2 s after that receive started.
    let events = run_until(vec![(2_500, vec![0x81])], 5_000, 4_500);
    assert_eq!(events.last(), Some(&(4_400, Event::Timeout)));
}

#[test]
fn interleaved_ping_and_pong_preserve_fragmented_text_and_reply_immediately() {
    let (events, wire) = run_capture(
        vec![
            (0, frame(false, 1, b"drive:")),
            (100, frame(true, 9, b"keepalive")),
            (200, frame(true, 10, b"other ping reply")),
            (300, frame(true, 0, b"cw")),
        ],
        600,
        2100,
    );
    assert_eq!(events, vec![(300, Event::Text("drive:cw".into()))]);
    assert!(wire.ends_with(b"\x8a\x09keepalive"));
}

#[test]
fn control_frame_capacity_does_not_consume_fragmented_message_capacity() {
    let (events, wire) = run_capture(
        vec![
            (0, frame(false, 2, &[0x41; 120])),
            (100, frame(true, 9, &[0x42; 125])),
            (200, frame(true, 0, &[0x41; 8])),
        ],
        600,
        2100,
    );
    assert_eq!(events, vec![(200, Event::Binary(vec![0x41; 128]))]);
    assert_eq!(&wire[wire.len() - 127..wire.len() - 125], &[0x8a, 125]);
    assert_eq!(&wire[wire.len() - 125..], &[0x42; 125]);
    assert_eq!(
        run(vec![
            (0, frame(false, 2, &[0x41; 120])),
            (100, frame(true, 0, &[0x41; 9])),
        ]),
        vec![(100, Event::ProtocolError(1002))]
    );
}

#[test]
fn interleaved_controls_cannot_restart_whole_message_deadline() {
    let (events, wire) = run_capture(
        vec![
            (0, frame(false, 1, b"drive:")),
            (1000, frame(true, 9, b"a")),
            (1999, frame(true, 9, b"b")),
            (2001, frame(true, 0, b"cw")),
        ],
        600,
        2100,
    );
    assert_eq!(events, vec![(2000, Event::Timeout)]);
    assert!(wire.ends_with(b"\x8a\x01a\x8a\x01b"));
}

#[test]
fn close_can_interrupt_fragmented_data_but_control_frames_cannot_fragment() {
    assert_eq!(
        run(vec![
            (0, frame(false, 1, b"drive:")),
            (10, frame(true, 8, &[0x03, 0xE8])),
        ]),
        vec![(10, Event::Close)]
    );
    assert_eq!(
        run(vec![(0, frame(false, 9, b"fragmented ping"))]),
        vec![(0, Event::ProtocolError(1002))]
    );
    assert_eq!(
        run(vec![(0, frame(true, 8, &[0x03]))]),
        vec![(0, Event::ProtocolError(1002))]
    );
}

#[test]
fn utf8_is_validated_after_all_text_fragments_are_reassembled() {
    assert_eq!(
        run(vec![
            (0, frame(false, 1, &[0xE2])),
            (10, frame(true, 9, b"a")),
            (20, frame(true, 0, &[0x82, 0xAC])),
        ]),
        vec![(20, Event::Text("€".into()))]
    );
}

#[test]
fn delayed_complete_and_partial_commands_do_not_receive_a_new_movement_lease() {
    use ezal_core::protocol::{ClientMessage, CommandFreshness};
    let message = b"press:4294967297:1:drive:cw";
    let full_frame = frame(true, 1, message);
    let delayed_messages = [
        // Buffered intact by the network until after the issued token expired.
        vec![(1100, full_frame.clone())],
        // Reading starts promptly but the remainder arrives after expiry.
        vec![
            (0, full_frame[..1].to_vec()),
            (1100, full_frame[1..].to_vec()),
        ],
        // Legal fragmentation takes less than the transport deadline but
        // longer than the command's independent admission deadline.
        vec![
            (0, frame(false, 1, &message[..12])),
            (1100, frame(true, 0, &message[12..])),
        ],
    ];
    for chunks in delayed_messages {
        let mut freshness = CommandFreshness::new(1);
        assert_eq!(freshness.issue(0).unwrap().token, 4294967297);
        let events = run_until(chunks, 2000, 2100);
        let (received_ms, Event::Text(message)) = events.last().unwrap() else {
            panic!("complete command should reach freshness admission");
        };
        let Some(ClientMessage::Movement(request)) = ClientMessage::parse(message) else {
            panic!("valid movement envelope");
        };
        assert_eq!(freshness.admit(request, *received_ms), None);
    }
}
