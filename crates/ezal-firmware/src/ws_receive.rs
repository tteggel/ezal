//! A deadline for the complete WebSocket message, including continuation frames.

use core::future::Future;

use picoserve::futures::Either;
use picoserve::io::{Read, Write};
use picoserve::response::ws::{
    Control, Data, Message, Opcode, ReadFrameError, ReadMessageError, SocketRx, SocketTx,
};
use picoserve::time::{Duration, TimeoutError, Timer};

use ezal_core::protocol::TELEMETRY_PERIOD_MS;

/// Maximum time spent in one receive operation. Idle connections still return
/// their telemetry signal normally; once a message starts, this deadline also
/// covers the remaining frame header, payload, and every continuation frame.
pub const MESSAGE_TIMEOUT: Duration = Duration::from_secs(2);

/// Per-connection message capacity. Commands are shorter, but RFC 6455 §5.5
/// permits 125-byte Ping, Pong, and Close payloads. The receive buffer must
/// also accommodate those transport frames or a legal keepalive can evict a
/// controller. Keep this shared with the parser integration tests.
pub const MESSAGE_BUFFER_SIZE: usize = 128;

// An idle connection must reach its telemetry tick inside this deadline.
// Otherwise every dashboard would drop and reconnect on the telemetry period,
// handing manual control to whichever client registers first.
const _: () = assert!(TELEMETRY_PERIOD_MS < MESSAGE_TIMEOUT.as_millis());

pub type ReceiveEvent<'a> = Either<Result<Message<'a>, ReadMessageError>, ()>;

/// Read one message or an idle telemetry tick.
///
/// picoserve's frame `signal` is only considered before the first message byte.
/// Its receive future is not cancel-safe after that byte: on `TimeoutError`, the
/// caller must end the connection, never resume parsing on this `SocketRx`.
/// `EmbassyTimer` implements the outer deadline with `embassy_time::with_timeout`.
///
/// picoserve 0.20.1's message assembler rejects control frames inside fragmented
/// messages. RFC 6455 §5.4 permits those frames, so use its public frame decoder
/// and assemble the bounded application message here. Interleaved Ping/Pong
/// frames use separate scratch storage and never reset the complete-message
/// deadline, including time spent replying to Ping.
pub async fn next_message<'a, Runtime, R: Read, W: Write<Error = R::Error>, T: Timer<Runtime>>(
    rx: &mut SocketRx<R>,
    tx: &mut SocketTx<W>,
    buffer: &'a mut [u8],
    timer: &T,
    telemetry: impl Future<Output = ()>,
) -> Result<Result<ReceiveEvent<'a>, R::Error>, TimeoutError> {
    timer
        .run_with_timeout(MESSAGE_TIMEOUT, assemble(rx, tx, buffer, telemetry))
        .await
}

async fn assemble<'a, R: Read, W: Write<Error = R::Error>>(
    rx: &mut SocketRx<R>,
    tx: &mut SocketTx<W>,
    buffer: &'a mut [u8],
    telemetry: impl Future<Output = ()>,
) -> Result<ReceiveEvent<'a>, R::Error> {
    let mut frame_buffer = [0; MESSAGE_BUFFER_SIZE];
    let mut incoming = rx.next_frame(&mut frame_buffer, telemetry).await?;
    let mut text = None;
    let mut length = 0;

    loop {
        let frame = match incoming {
            Either::Second(()) => return Ok(Either::Second(())),
            Either::First(Err(error)) => {
                return Ok(Either::First(Err(ReadMessageError::ReadFrameError(error))));
            }
            Either::First(Ok(frame)) => frame,
        };
        let payload = &frame_buffer[..frame.length];
        match frame.opcode {
            Opcode::Control(control) => {
                // A control frame is always final and at most 125 bytes,
                // regardless of the surrounding application's message size.
                if !frame.is_final || frame.length > 125 {
                    return Ok(Either::First(Err(ReadMessageError::UnexpectedMessageStart)));
                }
                match control {
                    Control::Reserved(opcode) => {
                        return Ok(Either::First(Err(ReadMessageError::ReservedOpcode(opcode))));
                    }
                    Control::Ping if text.is_some() => tx.send_pong(payload).await?,
                    Control::Pong if text.is_some() => {}
                    control => {
                        let Some(output) = buffer.get_mut(..frame.length) else {
                            return Ok(Either::First(Err(ReadMessageError::ReadFrameError(
                                ReadFrameError::OutOfSpace,
                            ))));
                        };
                        output.copy_from_slice(payload);
                        let message = match control {
                            Control::Ping => Ok(Message::Ping(output)),
                            Control::Pong => Ok(Message::Pong(output)),
                            Control::Close => match output {
                                [] => Ok(Message::Close(None)),
                                [_] => Err(ReadMessageError::UnexpectedMessageStart),
                                [high, low, reason @ ..] => core::str::from_utf8(reason)
                                    .map(|reason| {
                                        Message::Close(Some((
                                            u16::from_be_bytes([*high, *low]),
                                            reason,
                                        )))
                                    })
                                    .map_err(|_| ReadMessageError::TextIsNotUtf8),
                            },
                            Control::Reserved(_) => unreachable!(),
                        };
                        return Ok(Either::First(message));
                    }
                }
            }
            Opcode::Data(data) => {
                match (data, text) {
                    (Data::Text, None) => text = Some(true),
                    (Data::Binary, None) => text = Some(false),
                    (Data::Continue, Some(_)) => {}
                    (Data::Continue, None) => {
                        return Ok(Either::First(Err(
                            ReadMessageError::MessageStartsWithContinuation,
                        )));
                    }
                    (Data::Reserved(opcode), _) => {
                        return Ok(Either::First(Err(ReadMessageError::ReservedOpcode(opcode))));
                    }
                    (Data::Text | Data::Binary, Some(_)) => {
                        return Ok(Either::First(Err(ReadMessageError::UnexpectedMessageStart)));
                    }
                }
                let end = length + frame.length;
                let Some(output) = buffer.get_mut(length..end) else {
                    return Ok(Either::First(Err(ReadMessageError::ReadFrameError(
                        ReadFrameError::OutOfSpace,
                    ))));
                };
                output.copy_from_slice(payload);
                length = end;
                if frame.is_final {
                    let message = if text == Some(true) {
                        core::str::from_utf8(&buffer[..length])
                            .map(Message::Text)
                            .map_err(|_| ReadMessageError::TextIsNotUtf8)
                    } else {
                        Ok(Message::Binary(&buffer[..length]))
                    };
                    return Ok(Either::First(message));
                }
            }
        }
        // Once the message begins, telemetry cannot cancel a partially read
        // frame. The one outer timeout remains armed across all fragments and
        // any number of control frames; a keepalive cannot extend it.
        incoming = rx
            .next_frame(&mut frame_buffer, core::future::pending())
            .await?;
    }
}
