// WebSocket connection state
let ws = null;
let isRecording = false;
let audioContext = null;
let mediaStream = null;
let sourceNode = null;
let processorNode = null;
let playbackContext = null;
let playbackSource = null;

// Audio configuration
const SAMPLE_RATE = 16000; // 16kHz sample rate
const CHANNELS = 1; // Mono
const BUFFER_SIZE = 4096;

// DOM elements
const connectBtn = document.getElementById('connectBtn');
const startBtn = document.getElementById('startBtn');
const stopBtn = document.getElementById('stopBtn');
const statusDiv = document.getElementById('status');
const errorDiv = document.getElementById('error');
const wsUrlInput = document.getElementById('wsUrl');

// Update status display
function updateStatus(status, className) {
    statusDiv.textContent = status;
    statusDiv.className = `status ${className}`;
}

// Show error message
function showError(message) {
    errorDiv.textContent = message;
    errorDiv.style.display = 'block';
}

// Clear error message
function clearError() {
    errorDiv.textContent = '';
    errorDiv.style.display = 'none';
}

// Connect to WebSocket server
async function connect() {
    const url = wsUrlInput.value.trim();
    if (!url) {
        showError('Please enter a WebSocket URL');
        return;
    }

    try {
        updateStatus('Connecting...', 'connecting');
        clearError();
        
        ws = new WebSocket(url);

        ws.onopen = () => {
            updateStatus('Connected', 'connected');
            connectBtn.disabled = true;
            startBtn.disabled = false;
            clearError();
            
            // Start sending ping messages for heartbeat
            startHeartbeat();
        };

        ws.onmessage = (event) => {
            handleMessage(event.data);
        };

        ws.onerror = (error) => {
            console.error('WebSocket error:', error);
            showError('WebSocket connection error');
        };

        ws.onclose = () => {
            updateStatus('Disconnected', 'disconnected');
            connectBtn.disabled = false;
            startBtn.disabled = true;
            stopBtn.disabled = true;
            stopRecording();
            stopHeartbeat();
        };

    } catch (error) {
        console.error('Connection error:', error);
        showError(`Failed to connect: ${error.message}`);
        updateStatus('Disconnected', 'disconnected');
    }
}

// Handle incoming WebSocket messages
function handleMessage(data) {
    try {
        // Try to parse as JSON first
        const message = JSON.parse(data);
        
        if (message.type === 'audio') {
            // Decode base64 audio data
            const audioData = base64ToPCM(message.data);
            playAudio(audioData);
        } else if (message.type === 'error') {
            showError(`Server error: ${message.message}`);
        } else if (message.type === 'pong') {
            // Heartbeat response, no action needed
        }
    } catch (error) {
        // If not JSON, might be binary or other format
        console.warn('Failed to parse message:', error);
    }
}

// Convert base64 string to PCM i16 array
function base64ToPCM(base64) {
    const binaryString = atob(base64);
    const bytes = new Uint8Array(binaryString.length);
    for (let i = 0; i < binaryString.length; i++) {
        bytes[i] = binaryString.charCodeAt(i);
    }
    
    // Convert bytes to i16 samples (little-endian)
    const samples = new Int16Array(bytes.length / 2);
    for (let i = 0; i < samples.length; i++) {
        samples[i] = (bytes[i * 2 + 1] << 8) | bytes[i * 2];
    }
    
    return samples;
}

// Play PCM audio data
function playAudio(pcmData) {
    if (!playbackContext) {
        playbackContext = new AudioContext({ sampleRate: SAMPLE_RATE });
    }

    // Convert Int16Array to Float32Array for Web Audio API
    const float32Data = new Float32Array(pcmData.length);
    for (let i = 0; i < pcmData.length; i++) {
        // Convert from i16 range (-32768 to 32767) to float range (-1.0 to 1.0)
        float32Data[i] = pcmData[i] / 32768.0;
    }

    // Create audio buffer
    const buffer = playbackContext.createBuffer(CHANNELS, float32Data.length, SAMPLE_RATE);
    buffer.copyToChannel(float32Data, 0);

    // Create and play source
    const source = playbackContext.createBufferSource();
    source.buffer = buffer;
    source.connect(playbackContext.destination);
    source.start();
}

// Start recording audio from microphone
async function startRecording() {
    try {
        clearError();
        
        // Request microphone access
        mediaStream = await navigator.mediaDevices.getUserMedia({
            audio: {
                channelCount: CHANNELS,
                sampleRate: SAMPLE_RATE,
                echoCancellation: true,
                noiseSuppression: true,
            }
        });

        // Create audio context
        audioContext = new AudioContext({ sampleRate: SAMPLE_RATE });
        
        // Create source from microphone
        sourceNode = audioContext.createMediaStreamSource(mediaStream);
        
        // Create script processor to capture audio
        processorNode = audioContext.createScriptProcessor(BUFFER_SIZE, CHANNELS, CHANNELS);
        
        processorNode.onaudioprocess = (event) => {
            if (!isRecording || !ws || ws.readyState !== WebSocket.OPEN) {
                return;
            }

            // Get audio data from input
            const inputData = event.inputBuffer.getChannelData(0);
            
            // Convert Float32Array to Int16Array
            const pcmData = new Int16Array(inputData.length);
            for (let i = 0; i < inputData.length; i++) {
                // Clamp and convert from float (-1.0 to 1.0) to i16 (-32768 to 32767)
                const sample = Math.max(-1, Math.min(1, inputData[i]));
                pcmData[i] = sample < 0 ? sample * 0x8000 : sample * 0x7FFF;
            }

            // Send audio data to server
            sendAudioData(pcmData);
        };

        // Connect nodes
        sourceNode.connect(processorNode);
        processorNode.connect(audioContext.destination);

        isRecording = true;
        startBtn.disabled = true;
        stopBtn.disabled = false;
        
    } catch (error) {
        console.error('Failed to start recording:', error);
        showError(`Failed to access microphone: ${error.message}`);
        isRecording = false;
    }
}

// Stop recording
function stopRecording() {
    isRecording = false;
    
    if (processorNode) {
        processorNode.disconnect();
        processorNode = null;
    }
    
    if (sourceNode) {
        sourceNode.disconnect();
        sourceNode = null;
    }
    
    if (mediaStream) {
        mediaStream.getTracks().forEach(track => track.stop());
        mediaStream = null;
    }
    
    if (audioContext) {
        audioContext.close();
        audioContext = null;
    }
    
    startBtn.disabled = false;
    stopBtn.disabled = true;
}

// Send audio data to WebSocket server
function sendAudioData(pcmData) {
    if (!ws || ws.readyState !== WebSocket.OPEN) {
        return;
    }

    // Convert Int16Array to bytes (little-endian)
    const bytes = new Uint8Array(pcmData.length * 2);
    for (let i = 0; i < pcmData.length; i++) {
        const sample = pcmData[i];
        bytes[i * 2] = sample & 0xFF;
        bytes[i * 2 + 1] = (sample >> 8) & 0xFF;
    }

    // Encode to base64
    const base64 = btoa(String.fromCharCode(...bytes));

    // Send as JSON message
    const message = JSON.stringify({
        type: 'audio',
        data: base64
    });

    ws.send(message);
}

// Heartbeat management
let heartbeatInterval = null;

function startHeartbeat() {
    // Send ping every 5 seconds
    heartbeatInterval = setInterval(() => {
        if (ws && ws.readyState === WebSocket.OPEN) {
            const ping = JSON.stringify({ type: 'ping' });
            ws.send(ping);
        }
    }, 5000);
}

function stopHeartbeat() {
    if (heartbeatInterval) {
        clearInterval(heartbeatInterval);
        heartbeatInterval = null;
    }
}

// Event listeners
connectBtn.addEventListener('click', connect);
startBtn.addEventListener('click', startRecording);
stopBtn.addEventListener('click', stopRecording);

// Cleanup on page unload
window.addEventListener('beforeunload', () => {
    if (ws) {
        ws.close();
    }
    stopRecording();
    stopHeartbeat();
});
