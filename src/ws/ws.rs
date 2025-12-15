use actix::prelude::*;
use actix_web::{web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_web_actors::ws;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};

use voice_agent::messages::{AudioCaptureMessage, AudioPlaybackMessage};
use voice_agent::voice_agent::{VoiceAgent, Config};

/// How often heartbeat pings are sent
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// How long before lack of client response causes a timeout
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum WebSocketMessage {
    #[serde(rename = "audio")]
    Audio { data: String }, // base64 encoded PCM audio (i16 samples, little-endian)
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "pong")]
    Pong,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum WebSocketResponse {
    #[serde(rename = "audio")]
    Audio { data: String }, // base64 encoded PCM audio (i16 samples, little-endian)
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "pong")]
    Pong,
    #[serde(rename = "reset")]
    Reset,
}

/// WebSocket connection handler
struct WebSocketSession {
    /// Client must send ping at least once per 10 seconds (CLIENT_TIMEOUT),
    /// otherwise we drop connection.
    hb: Instant,
    /// Channel to send audio input to VoiceAgent
    capture_tx: UnboundedSender<AudioCaptureMessage>,
    /// Channel to receive playback messages from VoiceAgent
    playback_rx: Option<UnboundedReceiver<AudioPlaybackMessage>>,
    /// Signal to stop the VoiceAgent when WebSocket ends
    stop_tx: Option<oneshot::Sender<()>>,
}

impl Actor for WebSocketSession {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        info!("WebSocket session started");
        self.hb(ctx);
        
        // Spawn task to forward playback audio to WebSocket client
        if let Some(mut playback_rx) = self.playback_rx.take() {
            let addr = ctx.address();
            // Cache reset message string to avoid repeated allocation
            let reset_msg = serde_json::to_string(&WebSocketResponse::Reset).unwrap();
            tokio::spawn(async move {
                // Batch audio chunks to reduce WebSocket overhead
                let mut audio_buffer = Vec::new();
                let mut last_send = Instant::now();
                const BATCH_INTERVAL_MS: u64 = 20; // Send every 20ms or when buffer gets large
                const MAX_BATCH_SAMPLES: usize = 4800; // ~100ms at 48kHz
                
                while let Some(message) = playback_rx.recv().await {
                    match message {
                        AudioPlaybackMessage::Play(samples) => {
                            audio_buffer.extend_from_slice(&samples);
                            
                            // Send if buffer is large enough or enough time has passed
                            let should_send = audio_buffer.len() >= MAX_BATCH_SAMPLES 
                                || last_send.elapsed().as_millis() >= BATCH_INTERVAL_MS as u128;
                            
                            if should_send && !audio_buffer.is_empty() {
                                // Convert samples to bytes (little-endian) - pre-allocate capacity
                                let bytes: Vec<u8> = audio_buffer
                                    .iter()
                                    .flat_map(|s| s.to_le_bytes())
                                    .collect();

                                // Encode to base64
                                let data = BASE64.encode(&bytes);

                                // Send to WebSocket client
                                let msg = serde_json::to_string(&WebSocketResponse::Audio { data })
                                    .unwrap();
                                addr.do_send(PlaybackMessage(msg));
                                
                                audio_buffer.clear();
                                last_send = Instant::now();
                            }
                        }
                        AudioPlaybackMessage::Reset => {
                            debug!("Playback reset requested");
                            // Clear buffered audio - don't send it, as it's from the previous response
                            // This prevents old audio from playing after reset
                            audio_buffer.clear();
                            
                            // Send reset message FIRST to ensure client processes it before any new audio
                            addr.do_send(PlaybackMessage(reset_msg.clone()));
                            
                            // Small delay to ensure reset message is sent before any new audio chunks
                            // This helps with message ordering over WebSocket
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                    }
                }
                info!("Playback task finished");
            });
        }
    }

    fn stopping(&mut self, _: &mut Self::Context) -> Running {
        info!("WebSocket session stopping");
        self.request_stop();
        Running::Stop
    }
}

impl WebSocketSession {
    fn new(
        capture_tx: UnboundedSender<AudioCaptureMessage>,
        playback_rx: UnboundedReceiver<AudioPlaybackMessage>,
        stop_tx: oneshot::Sender<()>,
    ) -> Self {
        Self {
            hb: Instant::now(),
            capture_tx,
            playback_rx: Some(playback_rx),
            stop_tx: Some(stop_tx),
        }
    }

    /// Sends ping to client every HEARTBEAT_INTERVAL seconds
    fn hb(&self, ctx: &mut ws::WebsocketContext<Self>) {
        ctx.run_interval(HEARTBEAT_INTERVAL, |act, ctx| {
            // Check client heartbeats
            if Instant::now().duration_since(act.hb) > CLIENT_TIMEOUT {
                warn!("Client heartbeat timeout, disconnecting");
                act.request_stop();
                ctx.stop();
                return;
            }

            // Send ping (cache the pong message string)
            static PONG_MSG: Lazy<String> = Lazy::new(|| {
                serde_json::to_string(&WebSocketResponse::Pong).unwrap()
            });
            ctx.ping(b"");
            ctx.text(PONG_MSG.as_str());
        });
    }

    /// Handle incoming WebSocket messages
    fn handle_message(&mut self, msg: String, ctx: &mut ws::WebsocketContext<Self>) {
        match serde_json::from_str::<WebSocketMessage>(&msg) {
            Ok(WebSocketMessage::Audio { data }) => {
                // Decode base64 audio data
                match BASE64.decode(&data) {
                    Ok(bytes) => {
                        // Convert bytes to Vec<i16> (little-endian)
                        if bytes.len() % 2 != 0 {
                            warn!("Audio data length is not even, dropping");
                            return;
                        }
                        let samples: Vec<i16> = bytes
                            .chunks_exact(2)
                            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                            .collect();

                        debug!("Received audio: {} samples", samples.len());

                        // Send to VoiceAgent
                        if let Err(e) = self.capture_tx.send(AudioCaptureMessage::Chunk(samples)) {
                            error!("Failed to send audio to VoiceAgent: {e}");
                            let error_msg = serde_json::to_string(&WebSocketResponse::Error {
                                message: "Failed to process audio".to_string(),
                            })
                            .unwrap();
                            ctx.text(error_msg);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to decode base64 audio: {e}");
                        let error_msg = serde_json::to_string(&WebSocketResponse::Error {
                            message: format!("Invalid audio data: {e}"),
                        })
                        .unwrap();
                        ctx.text(error_msg);
                    }
                }
            }
            Ok(WebSocketMessage::Ping) => {
                // Respond to ping (use cached message)
                static PONG_MSG: Lazy<String> = Lazy::new(|| {
                    serde_json::to_string(&WebSocketResponse::Pong).unwrap()
                });
                ctx.text(PONG_MSG.as_str());
                self.hb = Instant::now();
            }
            Ok(WebSocketMessage::Pong) => {
                self.hb = Instant::now();
            }
            Err(e) => {
                warn!("Failed to parse WebSocket message: {e}");
                let error_msg = serde_json::to_string(&WebSocketResponse::Error {
                    message: format!("Invalid message format: {e}"),
                })
                .unwrap();
                ctx.text(error_msg);
            }
        }
    }

    fn request_stop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
    }

}

/// Handler for `ws::Message`
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WebSocketSession {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Ping(msg)) => {
                self.hb = Instant::now();
                ctx.pong(&msg);
            }
            Ok(ws::Message::Pong(_)) => {
                self.hb = Instant::now();
            }
            Ok(ws::Message::Text(text)) => {
                self.handle_message(text.to_string(), ctx);
            }
            Ok(ws::Message::Binary(bin)) => {
                // Handle binary audio data directly
                if bin.len() % 2 != 0 {
                    warn!("Binary audio data length is not even, dropping");
                    return;
                }
                let samples: Vec<i16> = bin
                    .chunks_exact(2)
                    .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect();

                debug!("Received binary audio: {} samples", samples.len());

                if let Err(e) = self.capture_tx.send(AudioCaptureMessage::Chunk(samples)) {
                    error!("Failed to send audio to VoiceAgent: {e}");
                }
            }
            Ok(ws::Message::Close(reason)) => {
                info!("WebSocket closing: {:?}", reason);
                self.request_stop();
                ctx.stop();
            }
            _ => {
                self.request_stop();
                ctx.stop();
            }
        }
    }
}

/// Message to send playback audio to WebSocket client
#[derive(Message)]
#[rtype(result = "()")]
struct PlaybackMessage(String);

impl Handler<PlaybackMessage> for WebSocketSession {
    type Result = ();

    fn handle(&mut self, msg: PlaybackMessage, ctx: &mut Self::Context) {
        ctx.text(msg.0);
    }
}

async fn websocket_handler(
    req: HttpRequest,
    stream: web::Payload,
    config: web::Data<Config>,
) -> Result<HttpResponse, Error> {
    let config = config.get_ref().clone();
    
    // Create channels for this connection
    let (capture_tx, capture_rx) = unbounded_channel::<AudioCaptureMessage>();
    let (playback_tx, playback_rx) = unbounded_channel::<AudioPlaybackMessage>();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    
    // Create VoiceAgent for this connection
    let mut agent = VoiceAgent::new(config);
    if let Err(e) = agent.start(capture_rx, playback_tx).await {
        error!("Failed to start VoiceAgent for WebSocket connection: {e}");
        return Err(actix_web::error::ErrorInternalServerError("Failed to start voice agent"));
    }
    
    // Create WebSocket session with channels
    let session = WebSocketSession::new(capture_tx, playback_rx, stop_tx);
    
    // Spawn task to run the agent (it will run until shutdown)
    tokio::spawn(async move {

        tokio::select! {
            res = agent.run() => {
                if let Err(e) = res {
                    error!("VoiceAgent run error: {e}");
                }
            }
            _ = stop_rx => {
                info!("WebSocket disconnect detected, stopping VoiceAgent");
                agent.set_stopping();
            }
        }

        agent.shutdown().await;
    });
    
    // Start WebSocket
    let resp = ws::start(session, &req, stream)?;
    
    Ok(resp)
}

pub async fn start_server(config: Config, bind_addr: String) -> anyhow::Result<()> {
    let server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(config.clone()))
            .route("/ws", web::get().to(websocket_handler))
    })
    .bind(&bind_addr)?
    .run();

    if let Err(e) = server.await {
        error!("Server error: {e}");
    }

    Ok(())
}
