use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use tokio_stream::Stream;
use tracing::{debug, warn};

/// OpenAI-compatible API endpoints for different providers
pub mod endpoints {
    pub const OPENAI: &str = "https://api.openai.com/v1/chat/completions";
    pub const GROQ: &str = "https://api.groq.com/openai/v1/chat/completions";
}

/// Message role in the conversation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// A single message in the conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: Role::System, content: content.into() }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: content.into() }
    }
}

/// Configuration for the LLM client
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// API endpoint (e.g., OpenAI, Groq, or custom)
    pub endpoint: String,
    /// API key for authentication
    pub api_key: String,
    /// Model name (e.g., "gpt-4o", "llama-3.3-70b-versatile")
    pub model: String,
    /// Optional system prompt
    pub system_prompt: Option<String>,
    /// Temperature for response generation (0.0 - 2.0)
    pub temperature: Option<f32>,
    /// Maximum tokens in response
    pub max_tokens: Option<u32>,
}

impl LlmConfig {
    pub fn new(endpoint: String, api_key: String, model: String) -> Self {
        Self {
            endpoint,
            api_key,
            model,
            system_prompt: None,
            temperature: None,
            max_tokens: None,
        }
    }

    pub fn openai(api_key: String, model: String) -> Self {
        Self::new(endpoints::OPENAI.to_string(), api_key, model)
    }

    pub fn groq(api_key: String, model: String) -> Self {
        Self::new(endpoints::GROQ.to_string(), api_key, model)
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }

    pub fn with_max_tokens(mut self, tokens: u32) -> Self {
        self.max_tokens = Some(tokens);
        self
    }
}

/// OpenAI-compatible request body
#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
}

/// Streaming response delta
#[derive(Debug, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
}

/// Streaming response choice
#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// Streaming response chunk
#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

/// Non-streaming response message
#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: String,
}

/// Non-streaming response choice
#[derive(Debug, Deserialize)]
struct ResponseChoice {
    message: ResponseMessage,
}

/// Non-streaming response
#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ResponseChoice>,
}

/// A streaming chunk from the LLM
#[derive(Debug, Clone, PartialEq)]
pub enum LlmChunk {
    /// A text delta
    Delta(String),
    /// Stream finished normally
    Done,
    /// Stream was cancelled
    Cancelled,
}

/// Cancellation token for LLM requests
#[derive(Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// Create a new cancellation token
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Cancel the token
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Check if cancelled
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Reset the token for reuse
    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }
}

/// LLM client with conversation history and streaming support
pub struct LlmClient {
    config: LlmConfig,
    client: Client,
    history: Mutex<Vec<Message>>,
    cancel_token: CancellationToken,
}

impl LlmClient {
    /// Create a new LLM client
    pub fn new(config: LlmConfig) -> Self {
        let client = Client::new();
        let mut history = Vec::new();

        // Add system prompt to history if configured
        if let Some(ref prompt) = config.system_prompt {
            history.push(Message::system(prompt.clone()));
        }

        Self {
            config,
            client,
            history: Mutex::new(history),
            cancel_token: CancellationToken::new(),
        }
    }

    /// Cancel any pending streaming or generation
    pub fn cancel(&self) {
        self.cancel_token.cancel();
    }

    /// Reset cancellation state (call before starting new request after cancel)
    pub fn reset_cancel(&self) {
        self.cancel_token.reset();
    }

    /// Check if currently cancelled
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    /// Get a clone of the cancellation token for external use
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Get the current conversation history
    pub async fn history(&self) -> Vec<Message> {
        self.history.lock().await.clone()
    }

    /// Clear conversation history (keeps system prompt if configured)
    pub async fn clear_history(&self) {
        let mut history = self.history.lock().await;
        history.clear();
        if let Some(ref prompt) = self.config.system_prompt {
            history.push(Message::system(prompt.clone()));
        }
    }

    /// Add a message to history
    pub async fn add_message(&self, message: Message) {
        self.history.lock().await.push(message);
    }

    /// Chat with the LLM (non-streaming)
    pub async fn chat(&self, user_message: &str) -> anyhow::Result<String> {
        // Add user message to history
        self.add_message(Message::user(user_message)).await;

        let history = self.history.lock().await;
        let request = ChatRequest {
            model: &self.config.model,
            messages: &history,
            stream: false,
            temperature: self.config.temperature,
            max_completion_tokens: self.config.max_tokens,
        };

        let response = self.client
            .post(&self.config.endpoint)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LLM API error {}: {}", status, text);
        }

        let chat_response: ChatResponse = response.json().await?;
        drop(history);

        let content = chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();

        // Add assistant response to history
        self.add_message(Message::assistant(&content)).await;

        Ok(content)
    }

    /// Chat with streaming response
    pub async fn chat_stream(
        &self,
        user_message: &str,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = LlmChunk> + Send>>> {
        self.chat_stream_with_token(user_message, self.cancel_token.clone()).await
    }

    /// Chat with streaming response using a custom cancellation token
    pub async fn chat_stream_with_token(
        &self,
        user_message: &str,
        cancel_token: CancellationToken,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = LlmChunk> + Send>>> {
        // Check if already cancelled
        if cancel_token.is_cancelled() {
            return Ok(Box::pin(async_stream::stream! {
                yield LlmChunk::Cancelled;
            }));
        }

        // Add user message to history
        self.add_message(Message::user(user_message)).await;

        let history = self.history.lock().await.clone();
        let request = ChatRequest {
            model: &self.config.model,
            messages: &history,
            stream: true,
            temperature: self.config.temperature,
            max_completion_tokens: self.config.max_tokens,
        };

        let response = self.client
            .post(&self.config.endpoint)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LLM API error {}: {}", status, text);
        }

        let byte_stream = response.bytes_stream();

        // Parse SSE stream
        let stream = async_stream::stream! {
            let mut buffer = String::new();

            tokio::pin!(byte_stream);

            while let Some(chunk_result) = byte_stream.next().await {
                // Check for cancellation
                if cancel_token.is_cancelled() {
                    yield LlmChunk::Cancelled;
                    return;
                }

                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));

                        // Process complete SSE events
                        while let Some(pos) = buffer.find("\n\n") {
                            // Check for cancellation before processing each event
                            if cancel_token.is_cancelled() {
                                yield LlmChunk::Cancelled;
                                return;
                            }

                            let event = buffer[..pos].to_string();
                            buffer = buffer[pos + 2..].to_string();

                            for line in event.lines() {
                                if let Some(data) = line.strip_prefix("data: ") {
                                    if data.trim() == "[DONE]" {
                                        yield LlmChunk::Done;
                                        continue;
                                    }

                                    match serde_json::from_str::<StreamChunk>(data) {
                                        Ok(chunk) => {
                                            for choice in chunk.choices {
                                                if let Some(content) = choice.delta.content {
                                                    if !content.is_empty() {
                                                        yield LlmChunk::Delta(content);
                                                    }
                                                }
                                                if choice.finish_reason.is_some() {
                                                    yield LlmChunk::Done;
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            debug!("Failed to parse SSE chunk: {e}, data: {data}");
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Stream error: {e}");
                        break;
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    /// Chat with streaming, collecting full response and adding to history
    pub async fn chat_stream_with_history(
        &self,
        user_message: &str,
    ) -> anyhow::Result<(Pin<Box<dyn Stream<Item = LlmChunk> + Send + 'static>>, tokio::sync::oneshot::Receiver<String>)> {
        self.chat_stream_with_history_and_token(user_message, self.cancel_token.clone()).await
    }

    /// Chat with streaming, collecting full response, with custom cancellation token
    pub async fn chat_stream_with_history_and_token(
        &self,
        user_message: &str,
        cancel_token: CancellationToken,
    ) -> anyhow::Result<(Pin<Box<dyn Stream<Item = LlmChunk> + Send + 'static>>, tokio::sync::oneshot::Receiver<String>)> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Check if already cancelled
        if cancel_token.is_cancelled() {
            let stream = async_stream::stream! {
                yield LlmChunk::Cancelled;
            };
            return Ok((Box::pin(stream), rx));
        }

        // Add user message to history
        self.add_message(Message::user(user_message)).await;

        let history = self.history.lock().await.clone();
        let request_body = serde_json::json!({
            "model": self.config.model,
            "messages": history,
            "stream": true,
            "temperature": self.config.temperature,
            "max_completion_tokens": self.config.max_tokens,
        });

        let response = self.client
            .post(&self.config.endpoint)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LLM API error {}: {}", status, text);
        }

        let byte_stream = response.bytes_stream();

        let stream = async_stream::stream! {
            let mut buffer = String::new();
            let mut full_response = String::new();
            let mut tx = Some(tx);

            tokio::pin!(byte_stream);

            while let Some(chunk_result) = byte_stream.next().await {
                // Check for cancellation
                if cancel_token.is_cancelled() {
                    if let Some(sender) = tx.take() {
                        let _ = sender.send(full_response.clone());
                    }
                    yield LlmChunk::Cancelled;
                    return;
                }

                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));

                        while let Some(pos) = buffer.find("\n\n") {
                            // Check for cancellation before processing each event
                            if cancel_token.is_cancelled() {
                                if let Some(sender) = tx.take() {
                                    let _ = sender.send(full_response.clone());
                                }
                                yield LlmChunk::Cancelled;
                                return;
                            }

                            let event = buffer[..pos].to_string();
                            buffer = buffer[pos + 2..].to_string();

                            for line in event.lines() {
                                if let Some(data) = line.strip_prefix("data: ") {
                                    if data.trim() == "[DONE]" {
                                        if let Some(sender) = tx.take() {
                                            let _ = sender.send(full_response.clone());
                                        }
                                        yield LlmChunk::Done;
                                        continue;
                                    }

                                    match serde_json::from_str::<StreamChunk>(data) {
                                        Ok(chunk) => {
                                            for choice in chunk.choices {
                                                if let Some(content) = choice.delta.content {
                                                    if !content.is_empty() {
                                                        full_response.push_str(&content);
                                                        yield LlmChunk::Delta(content);
                                                    }
                                                }
                                                if choice.finish_reason.is_some() {
                                                    if let Some(sender) = tx.take() {
                                                        let _ = sender.send(full_response.clone());
                                                    }
                                                    yield LlmChunk::Done;
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            debug!("Failed to parse SSE chunk: {e}");
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Stream error: {e}");
                        break;
                    }
                }
            }
        };

        Ok((Box::pin(stream), rx))
    }

    /// Manually add assistant response to history (use after chat_stream)
    pub async fn add_assistant_response(&self, content: &str) {
        self.add_message(Message::assistant(content)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_openai_api_key() -> Option<String> {
        std::env::var("OPENAI_API_KEY").ok()
    }

    fn get_openai_model() -> String {
        std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string())
    }

    fn create_test_client() -> Option<LlmClient> {
        let api_key = get_openai_api_key()?;
        let model = get_openai_model();
        let config = LlmConfig::openai(api_key, model)
            .with_system_prompt("You are a helpful assistant. Keep responses brief.")
            .with_max_tokens(100);
        Some(LlmClient::new(config))
    }

    #[test]
    fn test_message_constructors() {
        let system = Message::system("test system");
        assert_eq!(system.role, Role::System);
        assert_eq!(system.content, "test system");

        let user = Message::user("test user");
        assert_eq!(user.role, Role::User);
        assert_eq!(user.content, "test user");

        let assistant = Message::assistant("test assistant");
        assert_eq!(assistant.role, Role::Assistant);
        assert_eq!(assistant.content, "test assistant");
    }

    #[test]
    fn test_llm_config_builders() {
        let config = LlmConfig::openai("key".to_string(), "gpt-4".to_string())
            .with_system_prompt("prompt")
            .with_temperature(0.5)
            .with_max_tokens(100);

        assert_eq!(config.endpoint, endpoints::OPENAI);
        assert_eq!(config.api_key, "key");
        assert_eq!(config.model, "gpt-4");
        assert_eq!(config.system_prompt, Some("prompt".to_string()));
        assert_eq!(config.temperature, Some(0.5));
        assert_eq!(config.max_tokens, Some(100));
    }

    #[test]
    fn test_groq_config() {
        let config = LlmConfig::groq("key".to_string(), "llama-3.3-70b-versatile".to_string());
        assert_eq!(config.endpoint, endpoints::GROQ);
    }

    #[test]
    fn test_cancellation_token() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());

        token.cancel();
        assert!(token.is_cancelled());

        token.reset();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn test_cancellation_token_clone() {
        let token = CancellationToken::new();
        let cloned = token.clone();

        token.cancel();
        assert!(cloned.is_cancelled());
    }

    #[tokio::test]
    async fn test_history_management() {
        let config = LlmConfig::new(
            "http://localhost".to_string(),
            "fake_key".to_string(),
            "test".to_string(),
        ).with_system_prompt("System prompt");

        let client = LlmClient::new(config);

        // Should have system prompt
        let history = client.history().await;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, Role::System);

        // Add messages
        client.add_message(Message::user("Hello")).await;
        client.add_message(Message::assistant("Hi there")).await;

        let history = client.history().await;
        assert_eq!(history.len(), 3);

        // Clear history
        client.clear_history().await;
        let history = client.history().await;
        assert_eq!(history.len(), 1); // System prompt preserved
        assert_eq!(history[0].role, Role::System);
    }

    #[tokio::test]
    async fn test_client_cancel_interface() {
        let config = LlmConfig::new(
            "http://localhost".to_string(),
            "fake_key".to_string(),
            "test".to_string(),
        );

        let client = LlmClient::new(config);

        assert!(!client.is_cancelled());
        client.cancel();
        assert!(client.is_cancelled());
        client.reset_cancel();
        assert!(!client.is_cancelled());
    }

    #[tokio::test]
    async fn test_stream_cancelled_before_start() {
        let config = LlmConfig::new(
            "http://localhost".to_string(),
            "fake_key".to_string(),
            "test".to_string(),
        );

        let client = LlmClient::new(config);
        client.cancel();

        let result = client.chat_stream("Hello").await;
        assert!(result.is_ok());

        let mut stream = result.unwrap();
        let chunk = stream.next().await;
        assert_eq!(chunk, Some(LlmChunk::Cancelled));
    }

    // ============================================================================
    // Integration tests (require OPENAI_API_KEY)
    // ============================================================================

    #[tokio::test]
    async fn test_chat_non_streaming() {
        let Some(client) = create_test_client() else {
            eprintln!("Skipping test_chat_non_streaming: OPENAI_API_KEY not set");
            return;
        };

        let response = client.chat("Say 'hello' and nothing else").await;
        assert!(response.is_ok(), "Chat failed: {:?}", response.err());

        let text = response.unwrap().to_lowercase();
        assert!(text.contains("hello"), "Response should contain 'hello': {}", text);

        // Check history was updated
        let history = client.history().await;
        assert!(history.len() >= 3); // system + user + assistant
    }

    #[tokio::test]
    async fn test_chat_streaming() {
        let Some(client) = create_test_client() else {
            eprintln!("Skipping test_chat_streaming: OPENAI_API_KEY not set");
            return;
        };

        let result = client.chat_stream("Count from 1 to 3").await;
        assert!(result.is_ok(), "Stream failed: {:?}", result.err());

        let mut stream = result.unwrap();
        let mut full_response = String::new();
        let mut got_done = false;

        while let Some(chunk) = stream.next().await {
            match chunk {
                LlmChunk::Delta(text) => {
                    full_response.push_str(&text);
                }
                LlmChunk::Done => {
                    got_done = true;
                    break;
                }
                LlmChunk::Cancelled => {
                    panic!("Stream was unexpectedly cancelled");
                }
            }
        }

        assert!(got_done, "Stream should end with Done");
        assert!(!full_response.is_empty(), "Should have received some content");
    }

    #[tokio::test]
    async fn test_chat_stream_with_history() {
        let Some(client) = create_test_client() else {
            eprintln!("Skipping test_chat_stream_with_history: OPENAI_API_KEY not set");
            return;
        };

        let result = client.chat_stream_with_history("Say 'test' only").await;
        assert!(result.is_ok(), "Stream failed: {:?}", result.err());

        let (mut stream, rx) = result.unwrap();
        let mut stream_response = String::new();

        while let Some(chunk) = stream.next().await {
            match chunk {
                LlmChunk::Delta(text) => {
                    stream_response.push_str(&text);
                }
                LlmChunk::Done => break,
                LlmChunk::Cancelled => break,
            }
        }

        // Get full response from oneshot channel
        let full_response = rx.await;
        assert!(full_response.is_ok(), "Should receive full response");

        let full = full_response.unwrap();
        assert_eq!(stream_response, full, "Stream and oneshot should match");
    }

    #[tokio::test]
    async fn test_streaming_cancellation() {
        let Some(client) = create_test_client() else {
            eprintln!("Skipping test_streaming_cancellation: OPENAI_API_KEY not set");
            return;
        };

        let token = client.cancellation_token();

        // Start a stream that should take a while
        let result = client.chat_stream_with_token(
            "Write a very long story about a dragon",
            token.clone(),
        ).await;
        assert!(result.is_ok());

        let mut stream = result.unwrap();
        let mut chunks_received = 0;

        // Cancel after receiving a few chunks
        while let Some(chunk) = stream.next().await {
            match chunk {
                LlmChunk::Delta(_) => {
                    chunks_received += 1;
                    if chunks_received >= 3 {
                        token.cancel();
                    }
                }
                LlmChunk::Cancelled => {
                    // Successfully cancelled
                    return;
                }
                LlmChunk::Done => {
                    // Stream completed before cancellation took effect
                    return;
                }
            }
        }
    }

    #[tokio::test]
    async fn test_conversation_history() {
        let Some(client) = create_test_client() else {
            eprintln!("Skipping test_conversation_history: OPENAI_API_KEY not set");
            return;
        };

        // First message
        let _ = client.chat("My name is TestUser123").await;

        // Second message should remember context
        let response = client.chat("What is my name?").await;
        assert!(response.is_ok());

        let text = response.unwrap();
        assert!(
            text.contains("TestUser123"),
            "Should remember the name from context: {}",
            text
        );
    }
}

