use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Generic Twilio event with dynamic fields (for initial parsing)
pub type TwilioGenericEvent = HashMap<String, serde_json::Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwilioConnectedEvent {
    pub event: String,
    pub protocol: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwilioStartEvent {
    pub event: String,
    pub sequence_number: String,
    pub start: TwilioStartMetadata,
    pub stream_sid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwilioStartMetadata {
    pub stream_sid: String,
    pub account_sid: String,
    pub call_sid: String,
    pub tracks: Vec<String>,
    pub custom_parameters: HashMap<String, serde_json::Value>,
    pub media_format: TwilioMediaFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwilioMediaFormat {
    pub encoding: String,
    pub sample_rate: i32,
    pub channels: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwilioMediaEvent {
    pub event: String,
    pub sequence_number: String,
    pub media: TwilioMediaPayload,
    pub stream_sid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwilioMediaPayload {
    pub track: String,
    pub chunk: String,
    pub timestamp: String,
    pub payload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwilioStopEvent {
    pub event: String,
    pub sequence_number: String,
    pub stop: TwilioStopMetadata,
    pub stream_sid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwilioStopMetadata {
    pub account_sid: String,
    pub call_sid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwilioMarkEvent {
    pub event: String,
    pub stream_sid: String,
    pub sequence_number: String,
    pub mark: TwilioMarkPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwilioDtmfEvent {
    pub event: String,
    pub stream_sid: String,
    pub sequence_number: String,
    pub dtmf: TwilioDtmfPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwilioDtmfPayload {
    pub track: String,
    pub digit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwilioMarkPayload {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwilioMediaMessage {
    pub event: String,
    pub stream_sid: String,
    pub media: TwilioMediaMessagePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwilioMediaMessagePayload {
    pub payload: String,
}

/// Message to send a mark event to Twilio (for tracking audio playback)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct TwilioMarkMessage {
    pub event: String,
    pub stream_sid: String,
    pub mark: TwilioMarkMessagePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct TwilioMarkMessagePayload {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwilioClearMessage {
    pub event: String,
    pub stream_sid: String,
}

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