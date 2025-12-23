use futures_util::StreamExt;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{error, info};
use voice_agent::llm::{LlmChunk, LlmClient, LlmConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Configure tracing with microseconds precision
    tracing_subscriber::fmt()
        .with_timer(tracing_subscriber::fmt::time::ChronoUtc::new(
            "%Y-%m-%d %H:%M:%S%.6f UTC".to_string(),
        ))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("Starting console LLM client");

    // Read LLM configuration from environment variables (same as voice-agent)
    let llm_api_key = std::env::var("OPENAI_API_KEY")
        .or_else(|_| std::env::var("LLM_API_KEY"))
        .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY or LLM_API_KEY environment variable not set"))?;

    let llm_model = std::env::var("OPENAI_MODEL")
        .or_else(|_| std::env::var("LLM_MODEL"))
        .unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let llm_endpoint = std::env::var("LLM_ENDPOINT")
        .unwrap_or_else(|_| voice_agent::llm::endpoints::OPENAI.to_string());

    let llm_system_prompt = std::env::var("LLM_SYSTEM_PROMPT")
        .unwrap_or_else(|_| "You are a helpful assistant. Keep responses brief.".to_string());

    // Create LLM client
    let llm_config = LlmConfig::new(
        llm_endpoint,
        llm_api_key,
        llm_model,
    )
    .with_system_prompt(&llm_system_prompt);

    let llm_client = LlmClient::new(llm_config);
    info!("LLM client created and ready");

    // Read from stdin line by line
    let stdin = tokio::io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    info!("Enter prompts (one per line). Press Ctrl+D or Ctrl+C to exit.");

    loop {
        tokio::select! {
            // Read line from stdin
            result = lines.next_line() => {
                match result {
                    Ok(Some(line)) => {
                        let prompt = line.trim().to_string();
                        if prompt.is_empty() {
                            continue;
                        }

                        let start_time = Instant::now();
                        info!(prompt = %prompt, "Received prompt");

                        // Stream the response
                        match llm_client.chat_stream(&prompt).await {
                            Ok(mut stream) => {
                                let mut full_response = String::new();
                                let mut first_chunk_time = None;

                                while let Some(chunk) = stream.next().await {
                                    match chunk {
                                        LlmChunk::Delta(text) => {
                                            if first_chunk_time.is_none() {
                                                first_chunk_time = Some(Instant::now());
                                                let time_to_first_chunk = first_chunk_time.unwrap().duration_since(start_time);
                                                info!(
                                                    time_to_first_chunk_us = time_to_first_chunk.as_micros(),
                                                    "First chunk received"
                                                );
                                            }
                                            info!(text = %text, "LLM delta");
                                            full_response.push_str(&text);
                                        }
                                        LlmChunk::Done => {
                                            let elapsed = start_time.elapsed();
                                            let time_to_first_chunk = first_chunk_time
                                                .map(|t| t.duration_since(start_time).as_micros())
                                                .unwrap_or(0);
                                            info!(
                                                elapsed_us = elapsed.as_micros(),
                                                time_to_first_chunk_us = time_to_first_chunk,
                                                response_length = full_response.len(),
                                                "Response completed"
                                            );
                                            info!("Response completed");
                                            break;
                                        }
                                        LlmChunk::Cancelled => {
                                            error!("Response was cancelled");
                                            break;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                error!(error = %e, "Failed to stream response");
                            }
                        }
                    }
                    Ok(None) => {
                        // EOF
                        info!("EOF reached, exiting");
                        break;
                    }
                    Err(e) => {
                        error!(error = %e, "Error reading from stdin");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

