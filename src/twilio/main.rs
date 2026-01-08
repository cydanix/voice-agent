mod audio;
mod call_handler;
mod twilio;

use actix_web::{web, App, HttpResponse, HttpServer};
use tracing::info;
use voice_agent::voice_agent::Config;

/// Health check endpoint
async fn health() -> HttpResponse {
    HttpResponse::Ok().body("OK")
}

/// Main entry point
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("Starting voice-agent Twilio server");

    let gradium_api_key = std::env::var("GRADIUM_API_KEY").expect("GRADIUM_API_KEY environment variable not set");

    let stt_endpoint = std::env::var("GRADIUM_STT_ENDPOINT")
        .unwrap_or_else(|_| rust_gradium::STT_ENDPOINT.to_string());

    let stt_language = std::env::var("GRADIUM_STT_LANGUAGE")
        .unwrap_or_else(|_| "en".to_string());

    let tts_endpoint = std::env::var("GRADIUM_TTS_ENDPOINT")
        .unwrap_or_else(|_| rust_gradium::TTS_ENDPOINT.to_string());
    let tts_voice_id = std::env::var("GRADIUM_TTS_VOICE_ID")
        .unwrap_or_else(|_| rust_gradium::DEFAULT_VOICE_ID.to_string());

    let llm_api_key = std::env::var("OPENAI_API_KEY")
        .or_else(|_| std::env::var("LLM_API_KEY"))
        .expect("OPENAI_API_KEY or LLM_API_KEY environment variable not set");

    let llm_model = std::env::var("OPENAI_MODEL")
        .or_else(|_| std::env::var("LLM_MODEL"))
        .unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let llm_endpoint = std::env::var("LLM_ENDPOINT")
        .unwrap_or_else(|_| voice_agent::llm::endpoints::OPENAI.to_string());

    let llm_system_prompt = std::env::var("LLM_SYSTEM_PROMPT")
        .unwrap_or_else(|_| "You are a helpful voice assistant. Keep your responses concise and conversational.".to_string());

    let config = Config {
        tts_api_key: gradium_api_key.clone(),
        tts_endpoint,
        stt_endpoint,
        stt_api_key: gradium_api_key,
        stt_language,
        tts_voice_id,
        llm_api_key,
        llm_model,
        llm_endpoint,
        llm_system_prompt,
    };

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    info!("Starting server at http://{}", bind_addr);

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(config.clone()))
            .route("/call", web::route().to(call_handler::call_dispatch))
            .route("/health", web::get().to(health))
    })
    .bind(&bind_addr)?
    .run()
    .await
}
