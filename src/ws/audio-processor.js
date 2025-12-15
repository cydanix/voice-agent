// AudioWorklet processor for capturing microphone input
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
registerProcessor('audio-capture-processor', AudioCaptureProcessor);
