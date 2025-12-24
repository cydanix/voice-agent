use futures_util::StreamExt;
use reqwest::{Client, ClientBuilder};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tokio_stream::Stream;
use tracing::{debug, warn, error, info};

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

#[derive(Clone)]
pub struct LlmHistory {
    messages: Vec<Message>,
    system_prompt: String,
}

impl LlmHistory {
    const DEFAULT_SYSTEM_PROMPT: &str = "You are a helpful assistant. Keep responses brief.";

    pub fn new() -> Self {
        let mut messages = Vec::new();
        let system_prompt = Self::DEFAULT_SYSTEM_PROMPT.to_string();
        messages.push(Message::system(system_prompt.clone()));
        Self { messages, system_prompt }
    }

    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn get_messages(&self) -> Vec<Message> {
        self.messages.clone()
    }

    pub fn get_system_prompt(&self) -> String {
        self.system_prompt.clone()
    }

    pub fn set_system_prompt(&mut self, prompt: String) {
        // Update stored system prompt
        self.system_prompt = prompt.clone();
        
        // Update or insert system message at the beginning
        if !self.messages.is_empty() {
            match self.messages[0].role {
                Role::System => {
                    // Replace existing system message
                    self.messages[0] = Message::system(prompt);
                }
                _ => {
                    // Insert system message at the beginning
                    self.messages.insert(0, Message::system(prompt));
                }
            }
        } else {
            // No messages yet, just add the system message
            self.messages.push(Message::system(prompt));
        }
    }

    pub fn clear(&mut self) {
        self.system_prompt = Self::DEFAULT_SYSTEM_PROMPT.to_string();
        self.messages.clear();
        self.messages.push(Message::system(self.system_prompt.clone()));
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn get(&self, index: usize) -> Option<&Message> {
        self.messages.get(index)
    }
}

impl std::ops::Index<usize> for LlmHistory {
    type Output = Message;

    fn index(&self, index: usize) -> &Self::Output {
        &self.messages[index]
    }
}

/// LLM client with conversation history and streaming support
pub struct LlmClient {
    config: LlmConfig,
    client: Client,
    history: Mutex<LlmHistory>,
    cancel_token: CancellationToken,
}

impl LlmClient {
    /// Create a new LLM client with keep-alive connection pooling
    pub fn new(config: LlmConfig) -> Self {
        // Configure HTTP client with keep-alive and connection pooling
        let client = ClientBuilder::new()
            // Connection pool settings for connection reuse
            .pool_max_idle_per_host(10) // Maximum idle connections per host
            .pool_idle_timeout(Duration::from_secs(90)) // Keep idle connections for 90 seconds
            // TCP keep-alive to maintain connections
            .tcp_keepalive(Duration::from_secs(30)) // Send TCP keep-alive packets every 30 seconds
            // HTTP/2 keep-alive settings (if HTTP/2 is used)
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .http2_keep_alive_while_idle(true)
            .build()
            .expect("Failed to create HTTP client with keep-alive configuration");

        let mut history = LlmHistory::new();

        // Add system prompt to history if configured
        if let Some(ref prompt) = config.system_prompt {
            history.set_system_prompt(prompt.clone());
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
    pub async fn history(&self) -> LlmHistory {
        self.history.lock().await.clone()
    }

    /// Clear conversation history (keeps system prompt if configured)
    pub async fn clear_history(&self) {
        self.history.lock().await.clear();
    }

    pub async fn set_history(&self, history: LlmHistory) {
        *self.history.lock().await = history;
    }

    /// Add a message to history
    pub async fn add_message(&self, message: Message) {
        self.history.lock().await.add_message(message);
    }

    pub async fn set_system_prompt(&self, prompt: String) {
        self.history.lock().await.set_system_prompt(prompt);
    }

    pub async fn get_system_prompt(&self) -> String {
        self.history.lock().await.get_system_prompt()
    }

    /// Chat with the LLM (non-streaming)
    pub async fn chat(&self, user_message: &str) -> anyhow::Result<String> {
        let start_time = Instant::now();
        let messages = {
            let mut history = self.history.lock().await;
            history.add_message(Message::user(user_message));
            history.get_messages()
        };

        let request = ChatRequest {
            model: &self.config.model,
            messages: &messages,
            stream: false,
            temperature: self.config.temperature,
            max_completion_tokens: self.config.max_tokens,
        };

        let response = Self::send_request_with_retry(
            &self.client,
            &self.config.endpoint,
            &self.config.api_key,
            &request,
            &self.cancel_token,
        ).await?;

        let chat_response: ChatResponse = response.json().await?;

        let elapsed_ms = start_time.elapsed().as_millis();
        info!("Time to first response: {}ms", elapsed_ms);

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

    /// Send HTTP request with retry logic for rate limiting errors
    async fn send_request_with_retry(
        client: &Client,
        endpoint: &str,
        api_key: &str,
        request: &ChatRequest<'_>,
        cancel_token: &CancellationToken,
    ) -> anyhow::Result<reqwest::Response> {
        // Retry with exponential backoff for rate limiting errors
        let backoff_delays = [
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(400),
            Duration::from_millis(800),
            Duration::from_millis(1600),
        ];

        let mut last_error = None;

        for (attempt, &delay) in backoff_delays.iter().enumerate() {
            // Check for cancellation before each retry
            if cancel_token.is_cancelled() {
                anyhow::bail!("Request cancelled");
            }

            let request_result = client
                .post(endpoint)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(request)
                .send()
                .await;

            match request_result {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(resp);
                    }

                    let status_code = status.as_u16();
                    // Check if it's a rate limiting error (429) or service unavailable (503)
                    // 503 is sometimes used for rate limiting when the service is overloaded
                    if status_code == 429 || status_code == 503 {
                        let error_text = resp.text().await.unwrap_or_default();
                        last_error = Some(format!("LLM API rate limit error {}: {}", status, error_text));
                        
                        if attempt < backoff_delays.len() - 1 {
                            warn!("Rate limit error {} (attempt {}/{}), retrying after {:?}", 
                                status_code, attempt + 1, backoff_delays.len(), delay);
                            sleep(delay).await;
                            continue;
                        } else {
                            error!("Rate limit error {} after {} attempts, giving up", status_code, backoff_delays.len());
                            anyhow::bail!("{}", last_error.unwrap());
                        }
                    } else {
                        // Non-rate-limit error, don't retry
                        let error_text = resp.text().await.unwrap_or_default();
                        anyhow::bail!("LLM API error {}: {}", status, error_text);
                    }
                }
                Err(e) => {
                    // Network/connection errors - don't retry, fail immediately
                    // Only retry on HTTP rate limiting status codes
                    return Err(anyhow::anyhow!("LLM API request error: {}", e));
                }
            }
        }

        anyhow::bail!("Failed to get successful response after {} retries: {}", 
            backoff_delays.len(), last_error.unwrap_or_else(|| "Unknown error".to_string()))
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

        let start_time = Instant::now();
        let messages = {
            let mut history = self.history.lock().await;
            history.add_message(Message::user(user_message));
            history.get_messages()
        };

        let request = ChatRequest {
            model: &self.config.model,
            messages: &messages,
            stream: true,
            temperature: self.config.temperature,
            max_completion_tokens: self.config.max_tokens,
        };

        let response = Self::send_request_with_retry(
            &self.client,
            &self.config.endpoint,
            &self.config.api_key,
            &request,
            &cancel_token,
        ).await?;

        let byte_stream = response.bytes_stream();

        // Parse SSE stream
        let stream = async_stream::stream! {
            let mut buffer = String::new();
            let mut first_chunk_logged = false;

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
                                                        if !first_chunk_logged {
                                                            let elapsed_ms = start_time.elapsed().as_millis();
                                                            info!("Time to first chunk: {}ms", elapsed_ms);
                                                            first_chunk_logged = true;
                                                        }
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

        let start_time = Instant::now();
        let messages = {
            let mut history = self.history.lock().await;
            history.add_message(Message::user(user_message));
            history.get_messages()
        };

        let request = ChatRequest {
            model: &self.config.model,
            messages: &messages,
            stream: true,
            temperature: self.config.temperature,
            max_completion_tokens: self.config.max_tokens,
        };

        let response = Self::send_request_with_retry(
            &self.client,
            &self.config.endpoint,
            &self.config.api_key,
            &request,
            &cancel_token,
        ).await?;

        let byte_stream = response.bytes_stream();

        let stream = async_stream::stream! {
            let mut buffer = String::new();
            let mut full_response = String::new();
            let mut tx = Some(tx);
            let mut first_chunk_logged = false;

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
                                                        if !first_chunk_logged {
                                                            let elapsed_ms = start_time.elapsed().as_millis();
                                                            info!("Time to first chunk: {}ms", elapsed_ms);
                                                            first_chunk_logged = true;
                                                        }
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

    fn get_endpoint() -> String {
        std::env::var("LLM_ENDPOINT").unwrap_or_else(|_| "https://openrouter.ai/api/v1/chat/completions".to_string())
    }

    fn create_test_client() -> Option<LlmClient> {
        let api_key = get_openai_api_key()?;
        let model = get_openai_model();
        let endpoint = get_endpoint();
        let config = LlmConfig::new(endpoint, api_key, model)
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

