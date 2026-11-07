# mqttea

A high-performance, type-safe MQTT client library with built-in support for protobuf, JSON, YAML, and custom serialization formats for Rust.

mqttea provides a clean, async-first API for MQTT communication with automatic message serialization across multiple formats, client-scoped message registries, and comprehensive connection management. Built on top of rumqttc, it offers production-ready reliability with an ergonomic developer experience that lets you focus on your application logic, not message handling.

## Features

• **Decoupled message processing** - Fast MQTT message ingestion with separate processing queue prevents handler blocking
• **Multiple serialization formats** - Built-in support for protobuf, JSON, YAML, and raw bytes with extensible custom format support
• **Type-safe message handling** - Automatic serialization/deserialization with compile-time type safety
• **Client-scoped message registries** - Multiple clients can register different message types independently
• **Async-first design** - Built on tokio with non-blocking operations throughout
• **Comprehensive statistics** - Built-in tracking for queue depth, message throughput, and publish metrics
• **Flexible QoS support** - Per-message quality of service configuration
• **Connection resilience** - Automatic reconnection and connection state management
• **Zero-copy message handling** - Efficient encoding/decoding with minimal allocations
• **Production monitoring** - Structured logging with tracing integration

## Production Ready

mqttea is designed for production workloads with:

- **Lock-free statistics tracking** using atomic operations for high-throughput scenarios
- **Memory-efficient message processing** with Arc-based sharing and zero-copy operations
- **Robust error handling** with comprehensive error types and recovery mechanisms
- **Thread-safe client sharing** allowing safe concurrent access across async tasks
- **Configurable connection parameters** for tuning to specific network conditions
- **Extensive test coverage** with unit tests for all core functionality

## Architecture & Performance

### Decoupled Message Processing

mqttea uses a sophisticated two-stage architecture that ensures your MQTT broker connection stays responsive even under heavy message processing loads:

```
MQTT Broker → Fast Ingestion → Message Queue → Background Processing → Your Handlers
```

**Stage 1: Fast Message Ingestion**
- Messages are immediately read from the MQTT broker and queued
- No blocking on message processing or handler execution
- Maintains low-latency broker acknowledgments
- Prevents message loss during processing spikes

**Stage 2: Background Message Processing**
- Separate async task processes queued messages
- Messages are deserialized and routed to appropriate handlers
- Handler execution doesn't block new message ingestion
- Failed messages don't affect broker connectivity

### Real-Time Queue Monitoring

Track your message processing pipeline with built-in statistics:

```rust
// Monitor message flow
let queue_stats = client.queue_stats();
println!("Queue: {} pending, {} processed, {} bytes queued",
         queue_stats.pending_messages,
         queue_stats.total_processed,
         queue_stats.pending_bytes);

let publish_stats = client.publish_stats();
println!("Published: {} messages, {} bytes sent",
         publish_stats.total_published,
         publish_stats.total_bytes_published);

// Graceful shutdown - wait for queue to drain
client.wait_for_queue_empty().await;
```

This architecture ensures that even if your message handlers are slow or occasionally fail, your MQTT connection remains healthy and continues ingesting messages at full speed.

## Supported Serialization Formats

mqttea supports multiple serialization formats out of the box, plus the ability to add your own custom formats:

### JSON (with serde)
```rust
#[derive(Serialize, Deserialize)]
struct CatStatus {
    name: String,
    mood: String,
}

client.register_json_message::<CatStatus>("status").await?;
```

### Protobuf (with prost)
```rust
#[derive(prost::Message)]
struct SensorReading {
    #[prost(string, tag = "1")]
    device_id: String,
    #[prost(float, tag = "2")]
    temperature: f32,
}

client.register_protobuf_message::<SensorReading>("sensor").await?;
```

### YAML (with serde)
```rust
#[derive(Serialize, Deserialize)]
struct Configuration {
    host: String,
    port: u16,
    enabled: bool,
}

client.register_yaml_message::<Configuration>("config").await?;
```

### Raw Bytes
```rust
struct LogMessage {
    timestamp: u64,
    data: Vec<u8>,
}

impl RawMessageType for LogMessage {
    fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp() as u64,
            data: bytes,
        }
    }

    fn to_bytes(&self) -> Vec<u8> {
        self.data.clone()
    }
}

client.register_raw_message::<LogMessage>("logs").await?;
```

### Custom Formats
You can easily add support for your own serialization formats by implementing the serialization traits and registering custom handlers. See the documentation for extending mqttea with formats like MessagePack, CBOR, or your own binary protocols.

## Quick Start

### Basic Message Publishing

```rust
use mqttea::{MqtteaClient, ClientOptions, PublishOptions};
use rumqttc::QoS;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// Define your own JSON message type - let's track our pets!
#[derive(Serialize, Deserialize, Debug)]
struct CatStatus {
    name: String,
    mood: String,           // "sleepy", "playful", "hungry", "plotting world domination"
    location: String,       // "windowsill", "cardboard box", "your keyboard"
    timestamp: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a new MQTT client with options
    let client = MqtteaClient::new(
        "localhost",
        1883,
        "pet-tracker",
        Some(ClientOptions::default().with_qos(QoS::AtLeastOnce))
    ).await?;

    // Register your custom JSON message type
    client.register_json_message::<CatStatus>("status").await?;

    // Connect to start background processing
    client.connect().await?;

    // Send a message about Whiskers
    let message = CatStatus {
        name: "Whiskers".to_string(),
        mood: "plotting world domination".to_string(),
        location: "cardboard box fortress".to_string(),
        timestamp: chrono::Utc::now().timestamp() as u64,
    };

    client.send_message("/pets/whiskers/status", &message).await?;

    Ok(())
}
```

### Message Subscription and Handling

```rust
use mqttea::{MqtteaClient, ClientOptions};
use rumqttc::QoS;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// Define your own JSON message types for pet monitoring
#[derive(Serialize, Deserialize, Debug)]
struct DogActivity {
    name: String,
    activity: String,       // "napping", "playing fetch", "begging for treats", "barking at mailman"
    energy_level: u8,       // 1-10 scale
    last_treat_time: u64,
}

#[derive(Serialize, Deserialize, Debug)]
struct HamsterUpdate {
    name: String,
    wheel_distance: f32,    // miles run on wheel today
    cheek_fullness: u8,     // 1-10 scale of how stuffed their cheeks are
    is_building_fort: bool, // are they rearranging their bedding?
    timestamp: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = MqtteaClient::new(
        "localhost",
        1883,
        "pet-monitor",
        Some(ClientOptions::default().with_qos(QoS::AtLeastOnce))
    ).await?;

    // Register your custom JSON message types
    client.register_json_message::<DogActivity>("activity").await?;
    client.register_json_message::<HamsterUpdate>("update").await?;

    // Register message handlers - note the client parameter for responses
    client.on_message(|client: Arc<MqtteaClient>, activity: DogActivity, topic| async move {
        println!("{} is {} (energy: {}/10, last treat: {} mins ago)",
                activity.name, activity.activity, activity.energy_level,
                (chrono::Utc::now().timestamp() as u64 - activity.last_treat_time) / 60);

        // Can send a response if needed
        if activity.energy_level > 8 {
            let response_topic = format!("{}/high-energy-alert", topic.trim_end_matches("/activity"));
            let _ = client.send_message(&response_topic, &activity).await;
        }
    }).await;

    client.on_message(|_client: Arc<MqtteaClient>, update: HamsterUpdate, topic| async move {
        println!("{} ran {:.1} miles today! Cheeks: {}/10 full, Fort building: {}",
                update.name, update.wheel_distance, update.cheek_fullness,
                if update.is_building_fort { "YES!" } else { "nope" });
    }).await;

    // Subscribe to pet updates
    client.subscribe("/pets/+/activity", QoS::AtLeastOnce).await?;
    client.subscribe("/pets/+/update", QoS::AtLeastOnce).await?;

    // Connect and start processing
    client.connect().await?;

    // Keep running to receive messages
    tokio::signal::ctrl_c().await?;

    Ok(())
}
```

### Advanced Usage with Statistics and Custom Options

```rust
use mqttea::{MqtteaClient, ClientOptions, PublishOptions};
use rumqttc::QoS;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

// Define your own JSON message type for pet health monitoring
#[derive(Serialize, Deserialize, Debug)]
struct RabbitHealthCheck {
    name: String,
    weight_grams: u32,
    hay_consumed_grams: u32,
    pellets_eaten: u8,
    binky_count: u8,        // number of happy jumps today!
    litter_box_visits: u8,
    vet_checkup_due: bool,
    timestamp: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = MqtteaClient::new(
        "localhost",
        1883,
        "rabbit-health-monitor",
        Some(ClientOptions::default()
            .with_qos(QoS::AtLeastOnce)
            .with_keep_alive(Duration::from_secs(60))
        )
    ).await?;

    // Register your custom JSON message type with specific QoS
    client.register_json_message_with_opts::<RabbitHealthCheck>(
        "health",
        Some(PublishOptions::default()
            .with_qos(QoS::ExactlyOnce)
            .with_retain(false)
        )
    ).await?;

    // Connect to start processing
    client.connect().await?;

    // Send health data for multiple rabbits
    let rabbit_names = ["Cocoa", "Marshmallow", "Pepper", "Cinnamon", "Nutmeg"];

    for i in 0..100 {
        let rabbit_name = rabbit_names[i % rabbit_names.len()];
        let health_check = RabbitHealthCheck {
            name: rabbit_name.to_string(),
            weight_grams: 1200 + (i % 200) as u32,  // healthy weight variation
            hay_consumed_grams: 80 + (i % 40) as u32,
            pellets_eaten: 15 + (i % 5) as u8,
            binky_count: (i % 8) as u8,             // some days are more exciting!
            litter_box_visits: 8 + (i % 4) as u8,
            vet_checkup_due: i % 30 == 0,           // checkup every 30 reports
            timestamp: chrono::Utc::now().timestamp() as u64,
        };

        client.send_message(&format!("/pets/{}/health", rabbit_name.to_lowercase()), &health_check).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Check statistics
    let queue_stats = client.queue_stats();
    let publish_stats = client.publish_stats();

    println!("Rabbit health monitoring stats:");
    println!("Queue: {} pending, {} processed",
             queue_stats.pending_messages,
             queue_stats.total_processed);
    println!("Published: {} health reports, {} bytes of bunny data!",
             publish_stats.total_published,
             publish_stats.total_bytes_published);

    Ok(())
}
```

## Advanced Features

### Flexible Topic Pattern Registration

```rust
use mqttea::TopicPatterns;

// Single pattern
client.register_protobuf_message::<HelloWorld>("hello-world").await?;

// Multiple patterns for the same message type
client.register_protobuf_message::<HelloWorld>(
    ["hello-world", "greeting", "hi"]
).await?;

// Using TopicPatterns directly
client.register_protobuf_message::<HelloWorld>(
    TopicPatterns::from_multiple(vec!["hello", "hi", "greeting"])
).await?;
```

### Custom Recipients with MqttRecipient Trait

```rust
use mqttea::MqttRecipient;

#[derive(Debug)]
struct Device {
    id: String,
    priority: bool
}

impl MqttRecipient for Device {
    fn to_mqtt_topic(&self) -> String {
        if self.priority {
            format!("/priority/devices/{}", self.id)
        } else {
            format!("/devices/{}", self.id)
        }
    }
}

// Usage
let device = Device {
    id: "sensor-001".to_string(),
    priority: true
};

// This will send to "/priority/devices/sensor-001"
client.send_message(&device.to_mqtt_topic(), &message).await?;
```

### Raw Message Handling

```rust
use mqttea::RawMessage;

// Register catch-all handler for unmapped topics
client.register_raw_message::<RawMessage>(".*").await?;

client.on_message(|_client: Arc<MqtteaClient>, message: RawMessage, topic| async move {
    match String::from_utf8(message.payload.clone()) {
        Ok(text) => println!("Raw text on {}: {}", topic, text),
        Err(_) => println!("Raw binary on {} ({} bytes)", topic, message.payload.len()),
    }
}).await;
```

## Configuration Options

### ClientOptions

```rust
use std::time::Duration;

let client_options = ClientOptions::default()
    .with_qos(QoS::AtLeastOnce)
    .with_keep_alive(Duration::from_secs(30))
    .with_message_channel_capacity(5000);

let client = MqtteaClient::new(
    "localhost",
    1883,
    "my-client",
    Some(client_options)
).await?;
```

### PublishOptions per Message Type

```rust
let publish_options = PublishOptions::default()
    .with_qos(QoS::ExactlyOnce)
    .with_retain(true);

client.register_json_message_with_opts::<ImportantData>(
    "critical-data",
    Some(publish_options)
).await?;
```

## Error Handling

mqttea provides comprehensive error types for different failure modes:

```rust
use mqttea::MqtteaClientError;

match client.send_message("/topic", &message).await {
    Ok(_) => println!("Message sent successfully"),
    Err(MqtteaClientError::ConnectionError(e)) => {
        eprintln!("MQTT connection failed: {}", e);
        // Implement retry logic
    }
    Err(MqtteaClientError::JsonSerializationError(e)) => {
        eprintln!("Failed to serialize message: {}", e);
        // Handle serialization error
    }
    Err(e) => eprintln!("Other error: {}", e),
}
```

## Statistics and Monitoring

### Queue Statistics (Received Messages)
```rust
let stats = client.queue_stats();
println!("Pending: {}, Processed: {}, Failed: {}",
         stats.pending_messages,
         stats.total_processed,
         stats.total_failed);

// Additional queue stats available:
println!("Dropped: {}, Event loop errors: {}, Unmatched topics: {}",
         stats.total_dropped,
         stats.total_event_loop_errors,
         stats.total_unmatched_topics);
```

### Publish Statistics (Sent Messages)
```rust
let stats = client.publish_stats();
println!("Published: {}, Failed: {}, Bytes: {}",
         stats.total_published,
         stats.total_failed,
         stats.total_bytes_published);
```

### Graceful Shutdown
```rust
// Wait for all pending messages to be processed
client.wait_for_queue_empty().await;

// Then disconnect
client.disconnect().await?;
```

## Best Practices

### 1. Message Type Organization
```rust
// Group related message types
mod sensor_messages {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct TemperatureReading { /* ... */ }

    #[derive(Serialize, Deserialize)]
    pub struct HumidityReading { /* ... */ }
}

// Register them together
client.register_json_message::<sensor_messages::TemperatureReading>("temp").await?;
client.register_json_message::<sensor_messages::HumidityReading>("humidity").await?;
```

### 2. Handler Response Patterns
```rust
client.on_message(|client: Arc<MqtteaClient>, request: ServiceRequest, topic| async move {
    // Process the request
    let response = process_request(request).await;

    // Send response to a predictable topic
    let response_topic = topic.replace("/request", "/response");
    if let Err(e) = client.send_message(&response_topic, &response).await {
        eprintln!("Failed to send response: {}", e);
    }
}).await;
```

### 3. Statistics Monitoring
```rust
// Periodic stats reporting
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        let queue_stats = client.queue_stats();
        let publish_stats = client.publish_stats();

        if queue_stats.pending_messages > 1000 {
            eprintln!("Warning: High queue depth: {}", queue_stats.pending_messages);
        }

        println!("Hourly stats - Processed: {}, Published: {}",
                 queue_stats.total_processed,
                 publish_stats.total_published);
    }
});
```

## Performance Considerations

- **Message batching**: For high-throughput scenarios, consider batching multiple data points into single messages
- **QoS selection**: Use `QoS::AtMostOnce` for high-frequency, non-critical data; `QoS::AtLeastOnce` or `QoS::ExactlyOnce` for important messages
- **Handler efficiency**: Keep message handlers lightweight; offload heavy processing to background tasks
- **Connection pooling**: For microservices, consider sharing client instances where possible
- **Statistics monitoring**: Regularly check queue depth and processing rates to detect bottlenecks
