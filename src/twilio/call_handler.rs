use actix_web::{web, Error, HttpRequest, HttpResponse};
use actix_ws::Message;
use base64::{Engine as _, engine::general_purpose};
use futures_util::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender, UnboundedReceiver};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, error, info, warn};

use voice_agent::messages::{AudioCaptureMessage, AudioPlaybackMessage};
use voice_agent::voice_agent::{Config, VoiceAgent, VoiceAgentNoOpEventHandler};

use crate::audio::{AudioResampler, TtsDownsampler};
use crate::twilio::*;

/// Context for a single Twilio call, holding all state and the VoiceAgent
pub struct CallContext {
    /// VoiceAgent instance for this call
    agent: Mutex<VoiceAgent>,
    /// Stream SID from Twilio
    stream_sid: Mutex<String>,
    /// Call SID from Twilio (for logging)
    call_sid: Mutex<String>,
    /// Whether the agent has been started
    agent_started: AtomicBool,
    /// Sender for audio capture (Twilio -> VoiceAgent)
    capture_tx: UnboundedSender<AudioCaptureMessage>,
    /// Audio resampler for incoming Twilio audio (8kHz -> 24kHz)
    audio_resampler: Mutex<Option<AudioResampler>>,
    /// Downsampler for outgoing TTS audio (48kHz -> 8kHz)
    tts_downsampler: Mutex<Option<TtsDownsampler>>,
    /// Flag to signal shutdown
    stopping: AtomicBool,
}

impl CallContext {
    /// Create a new CallContext
    pub fn new(
        config: Config,
        capture_tx: UnboundedSender<AudioCaptureMessage>,
    ) -> Self {
        Self {
            agent: Mutex::new(VoiceAgent::new(config)),
            stream_sid: Mutex::new(String::new()),
            call_sid: Mutex::new(String::new()),
            agent_started: AtomicBool::new(false),
            capture_tx,
            audio_resampler: Mutex::new(None),
            tts_downsampler: Mutex::new(None),
            stopping: AtomicBool::new(false),
        }
    }

    /// Get the stream SID
    pub async fn get_stream_sid(&self) -> String {
        self.stream_sid.lock().await.clone()
    }

    /// Set the stream SID
    pub async fn set_stream_sid(&self, sid: String) {
        *self.stream_sid.lock().await = sid;
    }

    /// Get the call SID (for logging/debugging)
    #[allow(dead_code)]
    pub async fn get_call_sid(&self) -> String {
        self.call_sid.lock().await.clone()
    }

    /// Set the call SID
    pub async fn set_call_sid(&self, sid: String) {
        *self.call_sid.lock().await = sid;
    }

    /// Check if agent is started
    pub fn is_agent_started(&self) -> bool {
        self.agent_started.load(Ordering::SeqCst)
    }

    /// Check if stopping
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Signal shutdown
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }

    /// Initialize audio resamplers
    pub async fn init_resamplers(&self) -> anyhow::Result<()> {
        // Upsampler for incoming Twilio audio (160 = 20ms at 8kHz)
        let resampler = AudioResampler::new(160)
            .map_err(|e| anyhow::anyhow!("Failed to create audio resampler: {}", e))?;
        *self.audio_resampler.lock().await = Some(resampler);

        // Downsampler for outgoing TTS audio
        let downsampler = TtsDownsampler::new()
            .map_err(|e| anyhow::anyhow!("Failed to create TTS downsampler: {}", e))?;
        *self.tts_downsampler.lock().await = Some(downsampler);

        Ok(())
    }

    /// Start the VoiceAgent
    pub async fn start_agent(
        &self,
        capture_rx: UnboundedReceiver<AudioCaptureMessage>,
        playback_tx: UnboundedSender<AudioPlaybackMessage>,
    ) -> anyhow::Result<()> {
        let event_handler = Arc::new(VoiceAgentNoOpEventHandler);
        let mut agent = self.agent.lock().await;
        agent.start(capture_rx, playback_tx, event_handler).await?;
        agent.inject_tts_speech("Hello! How can I help you today?".to_string());
        self.agent_started.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Input audio boost factor to compensate for iPhone AGC suppression
    /// Higher values = louder input = better STT recognition of quiet audio
    const INPUT_BOOST_FACTOR: f32 = 2.0;

    /// Process incoming Twilio media (convert and forward to VoiceAgent)
    pub async fn process_incoming_media(&self, ulaw_bytes: &[u8]) {
        if !self.is_agent_started() {
            return;
        }

        let mut resampler_guard = self.audio_resampler.lock().await;
        if let Some(ref mut resampler) = *resampler_guard {
            let pcm_samples = resampler.ulaw8k_to_pcm24k(ulaw_bytes);
            if !pcm_samples.is_empty() {
                // Boost audio to compensate for iPhone AGC suppression
                let boosted_samples: Vec<i16> = pcm_samples
                    .iter()
                    .map(|&s| {
                        let boosted = (s as f32) * Self::INPUT_BOOST_FACTOR;
                        boosted.clamp(i16::MIN as f32, i16::MAX as f32) as i16
                    })
                    .collect();

                if let Err(e) = self.capture_tx.send(AudioCaptureMessage::Chunk(boosted_samples)) {
                    error!("Failed to send audio to VoiceAgent: {}", e);
                }
            }
        }
    }

    /// TTS volume reduction factor (0.0-1.0) to reduce iPhone AGC suppression
    /// Lower values = quieter TTS = less microphone suppression on caller's phone
    const TTS_VOLUME_FACTOR: f32 = 0.5;

    /// Process outgoing audio (convert TTS output to Twilio frames)
    pub async fn process_outgoing_audio(&self, samples: &[i16]) -> Vec<Vec<u8>> {
        // Reduce TTS volume to minimize iPhone AGC suppression
        let reduced_samples: Vec<i16> = samples
            .iter()
            .map(|&s| ((s as f32) * Self::TTS_VOLUME_FACTOR) as i16)
            .collect();

        let mut downsampler_guard = self.tts_downsampler.lock().await;
        if let Some(ref mut downsampler) = *downsampler_guard {
            downsampler.process(&reduced_samples)
        } else {
            Vec::new()
        }
    }

    /// Flush any remaining buffered TTS audio
    pub async fn flush_tts_buffer(&self) -> Option<Vec<u8>> {
        let mut downsampler_guard = self.tts_downsampler.lock().await;
        if let Some(ref mut downsampler) = *downsampler_guard {
            downsampler.flush()
        } else {
            None
        }
    }

    /// Shutdown the agent
    pub async fn shutdown_agent(&self) {
        if self.is_agent_started() {
            let mut agent = self.agent.lock().await;
            agent.set_stopping();
            agent.shutdown().await;
        }
    }
}

/// Messages sent to the Twilio sender task
#[derive(Debug)]
enum TwilioOutMessage {
    /// Send audio frames to Twilio
    Audio(Vec<Vec<u8>>),
    /// Send clear command to Twilio
    Clear,
    /// Send pong response (to ping from Twilio)
    Pong(Vec<u8>),
    /// Flush remaining TTS buffer and send
    Flush,
    /// Shutdown the sender task
    Shutdown,
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
    let (response, session, msg_stream) = actix_ws::handle(&req, body)?;

    info!("WebSocket connection established for Twilio call");

    let config = config.get_ref().clone();

    actix_web::rt::spawn(async move {
        // Create channels
        let (capture_tx, capture_rx) = unbounded_channel::<AudioCaptureMessage>();
        let (playback_tx, playback_rx) = unbounded_channel::<AudioPlaybackMessage>();
        let (twilio_out_tx, twilio_out_rx) = unbounded_channel::<TwilioOutMessage>();
        let (_shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        // Create call context
        let call_context = Arc::new(CallContext::new(config, capture_tx));

        // Spawn the Twilio sender task
        let sender_handle = spawn_twilio_sender_task(
            session,
            Arc::clone(&call_context),
            twilio_out_rx,
        );

        // Spawn the playback processor task (VoiceAgent -> Twilio)
        let playback_handle = spawn_playback_processor_task(
            Arc::clone(&call_context),
            playback_rx,
            twilio_out_tx.clone(),
        );

        // Run the Twilio receiver task (handles incoming WebSocket messages)
        run_twilio_receiver_task(
            msg_stream,
            Arc::clone(&call_context),
            capture_rx,
            playback_tx,
            twilio_out_tx,
            shutdown_rx,
        ).await;

        // Cleanup
        info!("Cleaning up Twilio call handler");
        call_context.shutdown();
        call_context.shutdown_agent().await;

        // Wait for spawned tasks to finish
        let _ = sender_handle.await;
        let _ = playback_handle.await;
    });

    Ok(response)
}

/// Ping interval for Twilio WebSocket (3 seconds)
const TWILIO_PING_INTERVAL: Duration = Duration::from_secs(3);

/// Task that sends audio and control messages to Twilio WebSocket
fn spawn_twilio_sender_task(
    mut session: actix_ws::Session,
    call_context: Arc<CallContext>,
    mut twilio_out_rx: UnboundedReceiver<TwilioOutMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut frame_counter = 0u64;
        let mut ping_interval = tokio::time::interval(TWILIO_PING_INTERVAL);
        let mut should_exit = false;

        loop {
            tokio::select! {
                // Handle outgoing messages
                msg = twilio_out_rx.recv() => {
                    match msg {
                        Some(TwilioOutMessage::Audio(frames)) => {
                            if call_context.is_stopping() {
                                break;
                            }
                            let stream_sid = call_context.get_stream_sid().await;
                            for (i, frame) in frames.iter().enumerate() {
                                if let Err(e) = send_media_to_twilio(
                                    &mut session,
                                    &stream_sid,
                                    frame,
                                    &mut frame_counter,
                                ).await {
                                    error!("Error sending media to Twilio: {}", e);
                                    should_exit = true;
                                    break;
                                }
                                // Yield every 5 frames (~100ms) to prevent blocking
                                if i > 0 && i % 5 == 0 {
                                    tokio::task::yield_now().await;
                                }
                            }
                            if should_exit {
                                break;
                            }
                        }
                        Some(TwilioOutMessage::Clear) => {
                            let stream_sid = call_context.get_stream_sid().await;
                            if let Err(e) = send_clear_to_twilio(&mut session, &stream_sid).await {
                                error!("Error sending clear to Twilio: {}", e);
                            }
                        }
                        Some(TwilioOutMessage::Pong(data)) => {
                            if let Err(e) = session.pong(&data).await {
                                warn!("Failed to send pong to Twilio: {}", e);
                            }
                        }
                        Some(TwilioOutMessage::Flush) => {
                            // Flush any remaining TTS audio
                            if let Some(frame) = call_context.flush_tts_buffer().await {
                                let stream_sid = call_context.get_stream_sid().await;
                                if let Err(e) = send_media_to_twilio(
                                    &mut session,
                                    &stream_sid,
                                    &frame,
                                    &mut frame_counter,
                                ).await {
                                    error!("Error sending flush frame to Twilio: {}", e);
                                }
                            }
                        }
                        Some(TwilioOutMessage::Shutdown) | None => {
                            break;
                        }
                    }
                }

                // Send periodic ping to keep connection alive
                _ = ping_interval.tick() => {
                    if call_context.is_stopping() {
                        break;
                    }
                    if let Err(e) = session.ping(b"").await {
                        warn!("Failed to send ping to Twilio: {}", e);
                        break;
                    }
                    debug!("Sent ping to Twilio WebSocket");
                }
            }
        }

        let _ = session.close(None).await;
        info!("Twilio sender task stopped");
    })
}

/// Task that processes playback messages from VoiceAgent and sends to Twilio
fn spawn_playback_processor_task(
    call_context: Arc<CallContext>,
    mut playback_rx: UnboundedReceiver<AudioPlaybackMessage>,
    twilio_out_tx: UnboundedSender<TwilioOutMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(msg) = playback_rx.recv().await {
            if call_context.is_stopping() {
                break;
            }

            match msg {
                AudioPlaybackMessage::Play(samples) => {
                    // Convert 48kHz PCM to 8kHz µ-law frames
                    let frames = call_context.process_outgoing_audio(&samples).await;
                    if !frames.is_empty() {
                        if let Err(e) = twilio_out_tx.send(TwilioOutMessage::Audio(frames)) {
                            error!("Failed to send audio to Twilio sender: {}", e);
                            break;
                        }
                    }
                }
                AudioPlaybackMessage::Reset => {
                    if let Err(e) = twilio_out_tx.send(TwilioOutMessage::Clear) {
                        error!("Failed to send clear to Twilio sender: {}", e);
                        break;
                    }
                }
            }
        }

        let _ = twilio_out_tx.send(TwilioOutMessage::Shutdown);
        info!("Playback processor task stopped");
    })
}

/// Heartbeat check interval
const HEARTBEAT_CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// Heartbeat timeout - if no ping/pong for this duration, consider connection dead.
/// Set to 10x ping interval (30 seconds) to handle brief network hiccups.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);

/// Task that handles incoming Twilio WebSocket messages
async fn run_twilio_receiver_task(
    mut msg_stream: actix_ws::MessageStream,
    call_context: Arc<CallContext>,
    capture_rx: UnboundedReceiver<AudioCaptureMessage>,
    playback_tx: UnboundedSender<AudioPlaybackMessage>,
    twilio_out_tx: UnboundedSender<TwilioOutMessage>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let mut last_heartbeat = Instant::now();
    let mut interval = actix_web::rt::time::interval(HEARTBEAT_CHECK_INTERVAL);

    // Wrap channels in Option for one-time consumption
    let mut capture_rx = Some(capture_rx);
    let mut playback_tx = Some(playback_tx);

    loop {
        tokio::select! {
            // Handle incoming WebSocket messages from Twilio
            Some(Ok(msg)) = msg_stream.recv() => {
                match msg {
                    Message::Ping(bytes) => {
                        last_heartbeat = Instant::now();
                        debug!("Received ping from Twilio, sending pong");
                        // Send pong response through the sender task
                        let _ = twilio_out_tx.send(TwilioOutMessage::Pong(bytes.to_vec()));
                    }
                    Message::Pong(_) => {
                        last_heartbeat = Instant::now();
                    }
                    Message::Text(text) => {
                        if !handle_twilio_text_message(
                            &text,
                            &call_context,
                            &mut capture_rx,
                            &mut playback_tx,
                        ).await {
                            // Stop event received
                            break;
                        }
                    }
                    Message::Close(reason) => {
                        info!("WebSocket closed: {:?}", reason);
                        break;
                    }
                    _ => {}
                }
            }

            // Check for shutdown signal
            _ = &mut shutdown_rx => {
                info!("Shutdown signal received");
                break;
            }

            // Periodic heartbeat check
            _ = interval.tick() => {
                if call_context.is_stopping() {
                    break;
                }

                // Check heartbeat timeout
                if Instant::now().duration_since(last_heartbeat) > HEARTBEAT_TIMEOUT {
                    warn!("Client timeout, closing connection");
                    break;
                }
            }
        }
    }

    // Flush any remaining TTS buffer before shutdown
    let _ = twilio_out_tx.send(TwilioOutMessage::Flush);
    // Signal shutdown to sender
    let _ = twilio_out_tx.send(TwilioOutMessage::Shutdown);
    info!("Twilio receiver task stopped");
}

/// Handle a text message from Twilio WebSocket
/// Returns false if the connection should be closed (stop event)
async fn handle_twilio_text_message(
    text: &str,
    call_context: &Arc<CallContext>,
    capture_rx: &mut Option<UnboundedReceiver<AudioCaptureMessage>>,
    playback_tx: &mut Option<UnboundedSender<AudioPlaybackMessage>>,
) -> bool {
    match serde_json::from_str::<TwilioGenericEvent>(text) {
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
                    if let Ok(evt) = serde_json::from_str::<TwilioConnectedEvent>(text) {
                        info!(
                            "Twilio connected: protocol={}, version={}",
                            evt.protocol, evt.version
                        );
                    }
                }
                "start" => {
                    if let Ok(evt) = serde_json::from_str::<TwilioStartEvent>(text) {
                        call_context.set_stream_sid(evt.stream_sid.clone()).await;
                        call_context.set_call_sid(evt.start.call_sid.clone()).await;
                        info!(
                            "Twilio stream started: stream_sid={}, call_sid={}",
                            evt.stream_sid, evt.start.call_sid
                        );

                        // Initialize audio resamplers
                        if let Err(e) = call_context.init_resamplers().await {
                            error!("Failed to create audio resamplers: {}", e);
                            return false;
                        }

                        // Start VoiceAgent (take channels once)
                        if let (Some(rx), Some(tx)) = (capture_rx.take(), playback_tx.take()) {
                            if let Err(e) = call_context.start_agent(rx, tx).await {
                                error!("Failed to start VoiceAgent: {}", e);
                                return false;
                            }
                            info!("VoiceAgent started successfully for call_sid={}", evt.start.call_sid);
                        }
                    }
                }
                "media" => {
                    if let Ok(evt) = serde_json::from_str::<TwilioMediaEvent>(text) {
                        if let Ok(ulaw_bytes) = general_purpose::STANDARD.decode(&evt.media.payload) {
                            call_context.process_incoming_media(&ulaw_bytes).await;
                        }
                    }
                }
                "mark" => {
                    if let Ok(evt) = serde_json::from_str::<TwilioMarkEvent>(text) {
                        debug!("Twilio mark: {}", evt.mark.name);
                    }
                }
                "dtmf" => {
                    if let Ok(evt) = serde_json::from_str::<TwilioDtmfEvent>(text) {
                        info!("Twilio DTMF: {}", evt.dtmf.digit);
                    }
                }
                "stop" => {
                    if let Ok(evt) = serde_json::from_str::<TwilioStopEvent>(text) {
                        info!(
                            "Twilio stream stopped: call_sid={}",
                            evt.stop.call_sid
                        );
                    }
                    return false;
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

    true
}
