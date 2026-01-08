use actix_web::{web, Error, HttpRequest, HttpResponse};
use actix_ws::Message;
use base64::{Engine as _, engine::general_purpose};
use futures_util::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};

use voice_agent::messages::{AudioCaptureMessage, AudioPlaybackMessage};
use voice_agent::voice_agent::{Config, VoiceAgent, VoiceAgentNoOpEventHandler};

use crate::audio::{pcm24k_to_ulaw8k, AudioResampler};
use crate::twilio::*;

#[derive(Debug, Deserialize)]
pub struct TwilioWebhookForm {
    #[serde(rename = "CallSid")]
    pub call_sid: Option<String>,
    #[serde(rename = "From")]
    pub from: Option<String>,
    #[serde(rename = "To")]
    pub to: Option<String>,
    #[serde(rename = "CallStatus")]
    pub call_status: Option<String>,
}

/// Send audio data to Twilio via WebSocket
async fn send_media_to_twilio(
    session: &mut actix_ws::Session,
    stream_sid: &str,
    data: &[u8],
    frame_counter: &mut u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if stream_sid.is_empty() {
        warn!("Stream SID not set, cannot send audio");
        return Ok(());
    }

    *frame_counter += 1;

    // Log every 50 frames (1 second)
    if *frame_counter % 50 == 0 {
        info!(
            "Sending frame {} to Twilio WebSocket: {} bytes",
            frame_counter,
            data.len()
        );
    }

    let encoded = general_purpose::STANDARD.encode(data);
    let msg = TwilioMediaMessage {
        event: "media".to_string(),
        stream_sid: stream_sid.to_string(),
        media: TwilioMediaMessagePayload { payload: encoded },
    };

    let json = serde_json::to_string(&msg)?;
    session.text(json).await?;

    debug!("Sent audio frame {} to Twilio", frame_counter);
    Ok(())
}

/// Send clear message to Twilio to stop current audio playback
async fn send_clear_to_twilio(
    session: &mut actix_ws::Session,
    stream_sid: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if stream_sid.is_empty() {
        return Ok(());
    }

    let clear_msg = TwilioClearMessage {
        event: "clear".to_string(),
        stream_sid: stream_sid.to_string(),
    };

    let json = serde_json::to_string(&clear_msg)?;
    session.text(json).await?;
    info!("Sent clear command to Twilio WebSocket");
    Ok(())
}

pub async fn call_dispatch(
    req: HttpRequest,
    body: web::Payload,
    config: web::Data<Config>,
) -> Result<HttpResponse, Error> {
    info!("Received incoming call, method: {}", req.method());

    // Check if this is a WebSocket upgrade request
    if req.headers().contains_key(actix_web::http::header::UPGRADE) {
        if let Some(upgrade) = req.headers().get(actix_web::http::header::UPGRADE) {
            if upgrade.to_str().unwrap_or("").to_lowercase() == "websocket" {
                info!("Handling as WebSocket connection");
                return call_ws(req, body, config).await;
            }
        }
    }

    // Handle regular HTTP POST request (Twilio webhook)
    if req.method() == actix_web::http::Method::POST {
        info!("Handling as Twilio webhook");
        return handle_twilio_webhook(req, body).await;
    }

    // Unsupported method
    warn!("Unsupported method for incoming call: {}", req.method());
    Ok(HttpResponse::MethodNotAllowed().body("Method not allowed"))
}

async fn handle_twilio_webhook(
    req: HttpRequest,
    mut body: web::Payload,
) -> Result<HttpResponse, Error> {
    // Read the payload bytes
    let mut bytes = web::BytesMut::new();
    while let Some(chunk) = body.next().await {
        bytes.extend_from_slice(&chunk?);
    }

    // Parse form data from Twilio webhook
    let form: TwilioWebhookForm = serde_urlencoded::from_bytes(&bytes)
        .map_err(|e| actix_web::error::ErrorBadRequest(format!("Failed to parse form data: {}", e)))?;

    info!(
        "Twilio call webhook - CallSid: {:?}, From: {:?}, To: {:?}, Status: {:?}",
        form.call_sid, form.from, form.to, form.call_status
    );

    // Get the host from the request
    let host = req
        .headers()
        .get(actix_web::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost:8080");

    // Respond with TwiML to start media stream
    let twiml_response = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
    <Connect>
        <Stream url="wss://{}/call" />
    </Connect>
</Response>"#,
        host
    );

    info!("Responding with TwiML: {}", twiml_response);

    Ok(HttpResponse::Ok()
        .content_type("text/xml")
        .body(twiml_response))
}

/// WebSocket handler for Twilio calls
pub async fn call_ws(
    req: HttpRequest,
    body: web::Payload,
    config: web::Data<Config>,
) -> Result<HttpResponse, Error> {
    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, body)?;

    info!("WebSocket connection established for Twilio call");

    let config = config.get_ref().clone();

    actix_web::rt::spawn(async move {
        // Create channels for VoiceAgent
        let (capture_tx, capture_rx) = unbounded_channel::<AudioCaptureMessage>();
        let (playback_tx, playback_rx) = unbounded_channel::<AudioPlaybackMessage>();
        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();

        // Wrap in Option so we can take them once when starting the agent
        let mut capture_rx = Some(capture_rx);
        let mut playback_tx = Some(playback_tx);

        // Create VoiceAgent
        let mut agent = VoiceAgent::new(config);
        let event_handler = Arc::new(VoiceAgentNoOpEventHandler);

        // Track stream state
        let mut stream_sid = String::new();
        let mut agent_started = false;
        let mut last_heartbeat = Instant::now();
        let mut interval = actix_web::rt::time::interval(Duration::from_millis(100));
        let mut twilio_frame_counter = 0u64;
        let mut audio_resampler: Option<AudioResampler> = None;
        let mut playback_rx = Some(playback_rx);

        loop {
            tokio::select! {
                // Handle outgoing playback messages from VoiceAgent
                msg = async {
                    if let Some(ref mut rx) = playback_rx {
                        rx.recv().await
                    } else {
                        std::future::pending::<Option<AudioPlaybackMessage>>().await
                    }
                } => {
                    if let Some(msg) = msg {
                        match msg {
                            AudioPlaybackMessage::Play(samples) => {
                                // Convert 24kHz PCM to 8kHz µ-law for Twilio
                                let ulaw_data = pcm24k_to_ulaw8k(&samples);
                                if let Err(e) = send_media_to_twilio(
                                    &mut session,
                                    &stream_sid,
                                    &ulaw_data,
                                    &mut twilio_frame_counter,
                                ).await {
                                    error!("Error sending media to Twilio: {}", e);
                                }
                            }
                            AudioPlaybackMessage::Reset => {
                                // Send clear message to stop current audio
                                if let Err(e) = send_clear_to_twilio(&mut session, &stream_sid).await {
                                    error!("Error sending clear to Twilio: {}", e);
                                }
                            }
                        }
                    }
                }

                // Handle incoming WebSocket messages from Twilio
                Some(Ok(msg)) = msg_stream.recv() => {
                    match msg {
                        Message::Ping(bytes) => {
                            last_heartbeat = Instant::now();
                            if session.pong(&bytes).await.is_err() {
                                warn!("Failed to send pong");
                                break;
                            }
                        }
                        Message::Pong(_) => {
                            last_heartbeat = Instant::now();
                        }
                        Message::Text(text) => {
                            // Parse Twilio event
                            match serde_json::from_str::<TwilioGenericEvent>(&text) {
                                Ok(event) => {
                                    let event_type = event
                                        .get("event")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown");

                                    if event_type != "media" {
                                        info!("Received Twilio event: {}", event_type);
                                    }

                                    match event_type {
                                        "connected" => {
                                            if let Ok(evt) = serde_json::from_str::<TwilioConnectedEvent>(&text) {
                                                info!(
                                                    "Twilio connected: protocol={}, version={}",
                                                    evt.protocol, evt.version
                                                );
                                            }
                                        }
                                        "start" => {
                                            if let Ok(evt) = serde_json::from_str::<TwilioStartEvent>(&text) {
                                                stream_sid = evt.stream_sid.clone();
                                                info!(
                                                    "Twilio stream started: stream_sid={}, call_sid={}",
                                                    stream_sid, evt.start.call_sid
                                                );

                                                // Initialize audio resampler (160 = 20ms at 8kHz)
                                                match AudioResampler::new(160) {
                                                    Ok(resampler) => {
                                                        audio_resampler = Some(resampler);
                                                    }
                                                    Err(e) => {
                                                        error!("Failed to create audio resampler: {}", e);
                                                        let _ = session.close(None).await;
                                                        return;
                                                    }
                                                }

                                                // Start VoiceAgent (take channels once)
                                                if let (Some(rx), Some(tx)) = (capture_rx.take(), playback_tx.take()) {
                                                    match agent.start(rx, tx, event_handler.clone()).await {
                                                        Ok(()) => {
                                                            info!("VoiceAgent started successfully");
                                                            agent_started = true;
                                                        }
                                                        Err(e) => {
                                                            error!("Failed to start VoiceAgent: {}", e);
                                                            let _ = session.close(None).await;
                                                            return;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        "media" => {
                                            if let Ok(evt) = serde_json::from_str::<TwilioMediaEvent>(&text) {
                                                if agent_started {
                                                    // Decode base64 µ-law audio
                                                    if let Ok(ulaw_bytes) = general_purpose::STANDARD.decode(&evt.media.payload) {
                                                        // Convert to 24kHz PCM
                                                        if let Some(ref mut resampler) = audio_resampler {
                                                            let pcm_samples = resampler.ulaw8k_to_pcm24k(&ulaw_bytes);
                                                            if !pcm_samples.is_empty() {
                                                                // Send to VoiceAgent
                                                                if let Err(e) = capture_tx.send(AudioCaptureMessage::Chunk(pcm_samples)) {
                                                                    error!("Failed to send audio to VoiceAgent: {}", e);
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        "mark" => {
                                            if let Ok(evt) = serde_json::from_str::<TwilioMarkEvent>(&text) {
                                                debug!("Twilio mark: {}", evt.mark.name);
                                            }
                                        }
                                        "dtmf" => {
                                            if let Ok(evt) = serde_json::from_str::<TwilioDtmfEvent>(&text) {
                                                info!("Twilio DTMF: {}", evt.dtmf.digit);
                                            }
                                        }
                                        "stop" => {
                                            if let Ok(evt) = serde_json::from_str::<TwilioStopEvent>(&text) {
                                                info!(
                                                    "Twilio stream stopped: call_sid={}",
                                                    evt.stop.call_sid
                                                );
                                            }
                                            let _ = session.close(None).await;
                                            break;
                                        }
                                        _ => {
                                            warn!("Unknown Twilio event: {}", event_type);
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("Error parsing Twilio event: {}", e);
                                }
                            }
                        }
                        Message::Close(reason) => {
                            info!("WebSocket closed: {:?}", reason);
                            let _ = session.close(reason).await;
                            break;
                        }
                        _ => {}
                    }
                }

                // Check for stop signal
                _ = &mut stop_rx => {
                    info!("Stop signal received");
                    break;
                }

                // Periodic heartbeat check
                _ = interval.tick() => {
                    // Check heartbeat timeout (10 minutes)
                    if Instant::now().duration_since(last_heartbeat) > Duration::from_secs(600) {
                        warn!("Client timeout, closing connection");
                        let _ = session.close(None).await;
                        break;
                    }
                }
            }
        }

        // Cleanup
        info!("Cleaning up Twilio call handler");
        if agent_started {
            agent.set_stopping();
            agent.shutdown().await;
        }
        drop(stop_tx);
    });

    Ok(response)
}
