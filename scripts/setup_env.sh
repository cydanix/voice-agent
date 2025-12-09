#!/bin/bash

# Check if script is being sourced (not executed directly)
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    echo "Error: This script must be sourced, not executed directly."
    echo "Usage: source $0"
    echo "   or: . $0"
    exit 1
fi

export GRADIUM_API_KEY=""
export GRADIUM_STT_ENDPOINT="wss://eu.api.gradium.ai/api/speech/asr"
export GRADIUM_STT_LANGUAGE="en"
export GRADIUM_TTS_ENDPOINT="wss://eu.api.gradium.ai/api/speech/tts"
export GRADIUM_TTS_VOICE_ID="YTpq7expH9539ERJ" # Emma US

export LLM_API_KEY=""
export LLM_ENDPOINT="https://openrouter.ai/api/v1/chat/completions"
export LLM_MODEL="openai/gpt-oss-safeguard-20b"
export LLM_SYSTEM_PROMPT="You are a helpful assistant. Keep responses brief."

echo "Default environment variables set"
echo "Set GRADIUM_API_KEY to your Gradium API key"
echo "Set LLM_API_KEY to OpenRouter API key"