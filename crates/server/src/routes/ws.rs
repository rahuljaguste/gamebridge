// SPDX-License-Identifier: AGPL-3.0-only
//! Browser WebSocket: server events in, controller state and commands out.

use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use serde::Deserialize;

use chiaki_core::controller::{ControllerState, ControllerTouch};
use chiaki_core::session::SessionCommand;

use crate::state::{AppState, ServerEvent};

pub async fn ws_handler(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Keeps the session manager's browser-presence count accurate on every exit
/// path, including a failed send or a panic while unwinding.
struct WsClientGuard(Arc<AppState>);

impl Drop for WsClientGuard {
    fn drop(&mut self) {
        self.0.ws_client_disconnected();
    }
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let (input, commands, session_id) = {
        let active = state.active.lock().await;
        match active.as_ref() {
            Some(active) => (
                active.input.clone(),
                active.commands.clone(),
                active.id.to_string(),
            ),
            None => {
                let _ = socket
                    .send(Message::Text(
                        serde_json::json!({"type":"error","message":"no active session"})
                            .to_string()
                            .into(),
                    ))
                    .await;
                return;
            }
        }
    };
    let (mut events, replay) = state.subscribe_with_replay();
    state.ws_client_connected();
    let _client = WsClientGuard(state.clone());
    let _ = socket
        .send(Message::Text(
            serde_json::to_string(&ServerEvent::SessionStarted { session_id })
                .unwrap_or_default()
                .into(),
        ))
        .await;
    // Replay what happened while the browser was gathering ICE candidates,
    // notably a PIN request or the current phase.
    for event in replay {
        let Ok(text) = serde_json::to_string(&event) else {
            continue;
        };
        if socket.send(Message::Text(text.into())).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        if let Ok(text) = serde_json::to_string(&event)
                            && socket.send(Message::Text(text.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!("browser event stream lagged by {skipped} events");
                        continue;
                    }
                    Err(_) => break,
                }
            }
            message = socket.recv() => {
                let Some(Ok(message)) = message else { break };
                match message {
                    Message::Text(text) => {
                        if let Err(message) = handle_client_message(text.as_str(), &input, &commands) {
                            let _ = socket
                                .send(Message::Text(
                                    serde_json::json!({"type":"error","message":message})
                                        .to_string()
                                        .into(),
                                ))
                                .await;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Input {
        #[serde(default)]
        buttons: u32,
        #[serde(default)]
        l2: u8,
        #[serde(default)]
        r2: u8,
        #[serde(default)]
        lx: i16,
        #[serde(default)]
        ly: i16,
        #[serde(default)]
        rx: i16,
        #[serde(default)]
        ry: i16,
        #[serde(default)]
        touches: Vec<TouchMessage>,
    },
    Pin {
        pin: String,
    },
    Keyframe,
    GotoBed,
    Stop,
}

#[derive(Debug, Deserialize)]
struct TouchMessage {
    #[serde(default = "touch_up")]
    id: i8,
    #[serde(default)]
    x: u16,
    #[serde(default)]
    y: u16,
}

fn touch_up() -> i8 {
    -1
}

/// Apply one browser message. The error text is shown to the user, so a
/// malformed message is distinguished from a command the session can't take.
fn handle_client_message(
    text: &str,
    input: &tokio::sync::watch::Sender<ControllerState>,
    commands: &tokio::sync::mpsc::Sender<SessionCommand>,
) -> Result<(), &'static str> {
    let Ok(message) = serde_json::from_str::<ClientMessage>(text) else {
        return Err("invalid message");
    };
    let send = |command: SessionCommand| {
        commands
            .try_send(command)
            .map_err(|_| "session is not accepting commands")
    };
    match message {
        ClientMessage::Input {
            buttons,
            l2,
            r2,
            lx,
            ly,
            rx,
            ry,
            touches,
        } => {
            let mut state = ControllerState::idle();
            state.buttons = buttons;
            state.l2_state = l2;
            state.r2_state = r2;
            state.left_x = lx;
            state.left_y = ly;
            state.right_x = rx;
            state.right_y = ry;
            for (slot, touch) in state.touches.iter_mut().zip(touches.iter().take(2)) {
                *slot = ControllerTouch {
                    id: touch.id,
                    x: touch.x,
                    y: touch.y,
                };
            }
            let _ = input.send(state);
            Ok(())
        }
        ClientMessage::Pin { pin } => send(SessionCommand::SubmitPin(pin.into_bytes())),
        ClientMessage::Keyframe => send(SessionCommand::RequestKeyframe),
        ClientMessage::GotoBed => send(SessionCommand::GotoBed),
        ClientMessage::Stop => send(SessionCommand::Stop),
    }
}
