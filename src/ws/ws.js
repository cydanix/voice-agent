// WebSocket connection state
let ws = null;
let isRecording = false;
let audioContext = null;
let mediaStream = null;
let sourceNode = null;
let workletNode = null;
let isWorkletReady = false;
let playbackContext = null;
let playbackSource = null;
let nextPlayTime = 0;
let activeSources = [];
let isTtsPlaying = false; // Track TTS playback state for echo cancellation awareness

// Audio configuration
const INPUT_SAMPLE_RATE = 24000; // 24kHz sample rate
const OUTPUT_SAMPLE_RATE = 48000; // 48kHz sample rate
const CHANNELS = 1; // Mono
const BUFFER_SIZE = 2048;
const PLAYBACK_LEAD_TIME = 0.02; // 20ms safety to avoid starting in the past

// DOM elements
const connectBtn = document.getElementById('connectBtn');
const disconnectBtn = document.getElementById('disconnectBtn');
const startBtn = document.getElementById('startBtn');
const stopBtn = document.getElementById('stopBtn');
const statusDiv = document.getElementById('status');
const errorDiv = document.getElementById('error');
const wsUrlInput = document.getElementById('wsUrl');

// Load worklet code as text (for blob URL fallback)
async function loadWorkletAsBlob() {
    try {
        // Try to fetch the worklet file
        const response = await fetch('audio-processor.js');
        if (response.ok) {
            return await response.text();
        }
    } catch (e) {
        // If fetch fails (file:// protocol), use embedded code
        console.log('Fetch failed, using embedded worklet code');
    }
    
    // Embedded worklet code as fallback
    return `// AudioWorklet processor for capturing microphone input
// This runs in a separate audio thread for better performance and lower latency

class AudioCaptureProcessor extends AudioWorkletProcessor {
    constructor(options) {
        super();
        // Use buffer size from options or default to 2048
        this.bufferSize = options?.processorOptions?.bufferSize || 2048;
        this.buffer = new Float32Array(this.bufferSize);
        this.bufferIndex = 0;
    }

    process(inputs, outputs, parameters) {
        // Get input from microphone (first input, first channel)
        const input = inputs[0];
        if (input && input.length > 0) {
            const inputChannel = input[0];
            const inputLength = inputChannel.length;
            
            // Copy input samples to buffer
            for (let i = 0; i < inputLength; i++) {
                this.buffer[this.bufferIndex++] = inputChannel[i];
                
                // When buffer is full, send it to main thread
                if (this.bufferIndex >= this.bufferSize) {
                    // Send a copy of the buffer to main thread (Float32Array is transferred efficiently)
                    this.port.postMessage({
                        type: 'audio',
                        data: new Float32Array(this.buffer)
                    });
                    this.bufferIndex = 0;
                }
            }
        }
        
        // Return true to keep the processor alive
        return true;
    }
}

// Register the processor
registerProcessor('audio-capture-processor', AudioCaptureProcessor);`;
}

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
            disconnectBtn.disabled = false;
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
            disconnectBtn.disabled = true;
            startBtn.disabled = true;
            stopBtn.disabled = true;
            stopRecording();
            stopHeartbeat();
        };

    } catch (error) {
        console.error('Connection error:', error);
        showError(`Failed to connect: ${error.message}`);
        updateStatus('Disconnected', 'disconnected');
        connectBtn.disabled = false;
        disconnectBtn.disabled = true;
    }
}

// Handle incoming WebSocket messages
function handleMessage(data) {
    try {
        // Try to parse as JSON first
        const message = JSON.parse(data);
        
        if (message.type === 'audio') {
            // If reset is pending, ignore audio until reset is processed
            if (isResetPending) {
                return; // Drop audio that arrived before reset
            }
            // Decode base64 audio data
            const audioData = base64ToPCM(message.data);
            playAudio(audioData);
        } else if (message.type === 'error') {
            showError(`Server error: ${message.message}`);
        } else if (message.type === 'pong') {
            // Heartbeat response, no action needed
        } else if (message.type === 'reset') {
            // Set flag first to block any audio that might arrive during reset processing
            isResetPending = true;
            // Reset playback state - this must be processed before any new audio
            resetPlayback();
            // Clear any queued audio that arrived before reset
            audioQueue = [];
            // Small delay to ensure all old audio sources are stopped
            // Then allow new audio to be processed
            setTimeout(() => {
                isResetPending = false;
            }, 10); // 10ms delay to ensure clean state
        }
    } catch (error) {
        // If not JSON, might be binary or other format
        console.warn('Failed to parse message:', error);
    }
}

// Convert base64 string to PCM i16 array (optimized)
function base64ToPCM(base64) {
    const binaryString = atob(base64);
    const len = binaryString.length;
    const bytes = new Uint8Array(len);
    
    // Optimized: use charCodeAt in a single loop
    for (let i = 0; i < len; i++) {
        bytes[i] = binaryString.charCodeAt(i);
    }
    
    // Convert bytes to i16 samples (little-endian)
    // Use DataView for proper endianness handling
    const sampleCount = len / 2;
    const samples = new Int16Array(sampleCount);
    const view = new DataView(bytes.buffer);
    
    for (let i = 0; i < sampleCount; i++) {
        // Read as little-endian (server sends little-endian)
        samples[i] = view.getInt16(i * 2, true);
    }
    
    return samples;
}

// Reset playback state
function resetPlayback() {
    // Clear audio queue immediately
    audioQueue = [];
    isProcessingQueue = false;
    
    // Stop all active audio sources
    const sources = activeSources.slice(); // Copy array to avoid modification during iteration
    sources.forEach(source => {
        try {
            source.stop();
        } catch (e) {
            // Source may already be stopped, ignore error
        }
    });
    activeSources = [];
    
    // Reset the next play time with a longer buffer to ensure all old audio is stopped
    // Use 50ms buffer to give time for sources to stop and avoid overlap
    if (playbackContext) {
        nextPlayTime = playbackContext.currentTime + 0.05; // 50ms buffer for clean start
    } else {
        // If context doesn't exist yet, it will be initialized in playAudio
        nextPlayTime = 0;
    }
}

// Audio playback queue for batching
let audioQueue = [];
let isProcessingQueue = false;
let isResetPending = false; // Flag to ignore audio until reset is processed
const MAX_QUEUE_SIZE = 10; // Limit queue size to prevent memory issues

// Process audio queue
function processAudioQueue() {
    if (isProcessingQueue || audioQueue.length === 0) {
        return;
    }
    
    isProcessingQueue = true;
    
    // Process all queued audio chunks
    while (audioQueue.length > 0) {
        const pcmData = audioQueue.shift();
        playAudioChunk(pcmData);
    }
    
    isProcessingQueue = false;
}

// Play a single PCM audio chunk
function playAudioChunk(pcmData) {
    if (!playbackContext) {
        playbackContext = new AudioContext({ sampleRate: OUTPUT_SAMPLE_RATE });
        nextPlayTime = playbackContext.currentTime + PLAYBACK_LEAD_TIME;
    }

    if (playbackContext.state === 'suspended') {
        playbackContext.resume();
    }

    // Convert Int16Array to Float32Array for Web Audio API (optimized)
    const len = pcmData.length;
    const float32Data = new Float32Array(len);
    const scale = 1.0 / 32768.0;
    
    // Optimized conversion loop
    for (let i = 0; i < len; i++) {
        // Convert from i16 range (-32768 to 32767) to float range (-1.0 to 1.0)
        const sample = pcmData[i];
        float32Data[i] = sample * scale;
    }

    // Create audio buffer
    const buffer = playbackContext.createBuffer(CHANNELS, float32Data.length, OUTPUT_SAMPLE_RATE);
    buffer.copyToChannel(float32Data, 0);

    // Schedule playback to avoid gaps
    const source = playbackContext.createBufferSource();
    source.buffer = buffer;
    source.connect(playbackContext.destination);
    
    // Track active sources for reset functionality (use Set for O(1) removal)
    activeSources.push(source);
    
    // Remove source from active list when it finishes (optimized)
    source.onended = () => {
        const index = activeSources.indexOf(source);
        if (index > -1) {
            activeSources.splice(index, 1);
        }
        // If this was the last source, mark TTS as not playing
        if (activeSources.length === 0) {
            isTtsPlaying = false;
        }
    };
    
    // Calculate duration and schedule next chunk
    const duration = buffer.duration;
    const startTime = Math.max(nextPlayTime, playbackContext.currentTime + PLAYBACK_LEAD_TIME);
    source.start(startTime);
    nextPlayTime = startTime + duration;
}

// Play PCM audio data (with queuing for batching)
function playAudio(pcmData) {
    // Don't queue audio if reset is pending
    if (isResetPending) {
        return; // Drop audio until reset is processed
    }
    // Mark that TTS is playing to help echo cancellation awareness
    isTtsPlaying = true;
    
    // Add to queue
    audioQueue.push(pcmData);
    
    // Limit queue size to prevent memory issues
    if (audioQueue.length > MAX_QUEUE_SIZE) {
        console.warn('Audio queue overflow, dropping oldest chunk');
        audioQueue.shift();
    }
    
    // Process queue asynchronously to avoid blocking
    if (!isProcessingQueue) {
        // Use requestAnimationFrame for better timing, or setTimeout(0) for immediate processing
        if (typeof requestAnimationFrame !== 'undefined') {
            requestAnimationFrame(processAudioQueue);
        } else {
            setTimeout(processAudioQueue, 0);
        }
    }
    
    // Note: isTtsPlaying will be reset when the last audio source finishes (in onended callback)
}

// Start recording audio from microphone using AudioWorklet
async function startRecording() {
    try {
        clearError();
        
        // Request microphone access with enhanced echo cancellation settings
        // These settings help prevent TTS output from interfering with microphone input
        const audioConstraints = {
            channelCount: CHANNELS,
            sampleRate: INPUT_SAMPLE_RATE,
            echoCancellation: true,
            noiseSuppression: true,
            autoGainControl: true, // Helps maintain consistent input levels
            // Chrome-specific settings for better echo cancellation
            googEchoCancellation: true,
            googAutoGainControl: true,
            googNoiseSuppression: true,
            googHighpassFilter: true,
            googTypingNoiseDetection: true,
            // Additional constraints for better isolation
            latency: 0.01, // Low latency
            sampleSize: 16,
        };
        
        mediaStream = await navigator.mediaDevices.getUserMedia({
            audio: audioConstraints
        });

        // Create audio context
        audioContext = new AudioContext({ sampleRate: INPUT_SAMPLE_RATE });
        
        // Load and add the audio worklet module
        // AudioWorklet requires HTTP/HTTPS, not file:// protocol
        // Try multiple methods to support both HTTP and file:// protocols
        try {
            // First, try loading from URL (works with HTTP/HTTPS)
            try {
                await audioContext.audioWorklet.addModule('audio-processor.js');
                isWorkletReady = true;
            } catch (urlError) {
                // If URL loading fails (file:// protocol), use embedded code as blob
                console.log('URL loading failed, trying blob method for file:// protocol:', urlError);
                const workletCode = await loadWorkletAsBlob();
                const blob = new Blob([workletCode], { type: 'application/javascript' });
                const blobUrl = URL.createObjectURL(blob);
                try {
                    await audioContext.audioWorklet.addModule(blobUrl);
                    isWorkletReady = true;
                } finally {
                    URL.revokeObjectURL(blobUrl); // Clean up
                }
            }
        } catch (error) {
            console.error('Failed to load audio worklet:', error);
            const isFileProtocol = window.location.protocol === 'file:';
            if (isFileProtocol) {
                showError(`AudioWorklet requires HTTP/HTTPS. Please use a local server:\n\n` +
                    `Option 1: Run: ./serve.sh (in this directory)\n` +
                    `Option 2: Run: python3 -m http.server 8000\n` +
                    `Then open: http://localhost:8000/ws.html\n\n` +
                    `Error: ${error.message}`);
            } else {
                showError(`Failed to load audio processor: ${error.message}. Please ensure audio-processor.js is accessible.`);
            }
            isRecording = false;
            return;
        }
        
        // Create source from microphone
        sourceNode = audioContext.createMediaStreamSource(mediaStream);
        
        // Create AudioWorkletNode for capturing audio
        workletNode = new AudioWorkletNode(audioContext, 'audio-capture-processor', {
            numberOfInputs: 1,
            numberOfOutputs: 0, // We don't need output, just capture
            channelCount: CHANNELS,
            processorOptions: {
                bufferSize: BUFFER_SIZE, // Pass buffer size to processor
            },
        });
        
        // Track audio levels to detect echo cancellation interference
        let lastAudioLevel = 0;
        let lowLevelCount = 0;
        const LOW_LEVEL_THRESHOLD = 0.005; // Threshold for detecting suppressed input
        const LOW_LEVEL_WARN_COUNT = 10; // Number of consecutive low-level chunks before warning
        
        // Handle messages from the worklet processor
        workletNode.port.onmessage = (event) => {
            if (!isRecording || !ws || ws.readyState !== WebSocket.OPEN) {
                return;
            }

            if (event.data.type === 'audio') {
                // Get audio data from worklet
                const inputData = event.data.data;
                const len = inputData.length;
                
                // Calculate audio level to detect if input is being suppressed
                let maxLevel = 0;
                for (let i = 0; i < len; i++) {
                    const abs = Math.abs(inputData[i]);
                    if (abs > maxLevel) {
                        maxLevel = abs;
                    }
                }
                lastAudioLevel = maxLevel;
                
                // Detect if audio level is suspiciously low (possible echo cancellation interference)
                // Normal speech should have levels above 0.01, silence is near 0
                // Only check when TTS is NOT playing to avoid false positives
                if (maxLevel < LOW_LEVEL_THRESHOLD && !isTtsPlaying) {
                    lowLevelCount++;
                    // If we see consistently low levels, it might indicate echo cancellation is too aggressive
                    if (lowLevelCount > LOW_LEVEL_WARN_COUNT) {
                        console.warn(`Audio input levels are very low (${(maxLevel * 100).toFixed(2)}%) - echo cancellation may be interfering. Consider using headphones or reducing speaker volume.`);
                        lowLevelCount = 0; // Reset counter
                    }
                } else {
                    lowLevelCount = 0;
                }
                
                // Convert Float32Array to Int16Array (optimized)
                const pcmData = new Int16Array(len);
                for (let i = 0; i < len; i++) {
                    // Clamp and convert from float (-1.0 to 1.0) to i16 (-32768 to 32767)
                    const sample = Math.max(-1, Math.min(1, inputData[i]));
                    // Optimized: use bit shift instead of multiplication where possible
                    pcmData[i] = sample < 0 ? (sample * 32768) | 0 : (sample * 32767) | 0;
                }

                // Send audio data to server
                sendAudioData(pcmData);
            }
        };

        // Connect nodes: source -> worklet
        sourceNode.connect(workletNode);

        isRecording = true;
        startBtn.disabled = true;
        stopBtn.disabled = false;
        
    } catch (error) {
        console.error('Failed to start recording:', error);
        showError(`Failed to access microphone: ${error.message}`);
        isRecording = false;
        isWorkletReady = false;
    }
}

// Stop recording
function stopRecording() {
    isRecording = false;
    
    if (workletNode) {
        workletNode.disconnect();
        workletNode.port.close();
        workletNode = null;
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
    
    isWorkletReady = false;
    startBtn.disabled = false;
    stopBtn.disabled = true;
}

// Send audio data to WebSocket server (optimized)
function sendAudioData(pcmData) {
    if (!ws || ws.readyState !== WebSocket.OPEN) {
        return;
    }

    // Convert Int16Array to bytes (little-endian)
    // Most systems are little-endian, so we can use the buffer directly
    // But we need to ensure little-endian byte order
    const len = pcmData.length;
    const bytes = new Uint8Array(len * 2);
    const view = new DataView(bytes.buffer);
    
    // Write samples as little-endian
    for (let i = 0; i < len; i++) {
        view.setInt16(i * 2, pcmData[i], true); // true = little-endian
    }
    
    // Encode to base64 - use chunked approach to avoid stack overflow
    let binaryString = '';
    const chunkSize = 8192; // Process in chunks
    for (let i = 0; i < bytes.length; i += chunkSize) {
        const chunk = bytes.subarray(i, Math.min(i + chunkSize, bytes.length));
        // Convert chunk to string efficiently
        binaryString += String.fromCharCode.apply(null, Array.from(chunk));
    }
    const base64 = btoa(binaryString);

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

// Disconnect from WebSocket server
function disconnect() {
    if (ws) {
        // Stop recording first
        stopRecording();
        stopHeartbeat();
        // Stop any ongoing playback and clear queued audio
        resetPlayback();
        if (playbackContext) {
            playbackContext.close();
            playbackContext = null;
        }
        
        // Close WebSocket connection
        ws.close();
        ws = null;
        
        updateStatus('Disconnected', 'disconnected');
        connectBtn.disabled = false;
        disconnectBtn.disabled = true;
        startBtn.disabled = true;
        stopBtn.disabled = true;
        clearError();
    }
}

// Event listeners
connectBtn.addEventListener('click', connect);
disconnectBtn.addEventListener('click', disconnect);
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
