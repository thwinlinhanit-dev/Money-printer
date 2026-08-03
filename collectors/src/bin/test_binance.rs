#[cfg(feature = "live-ws")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = "wss://fstream.binance.com/ws";
    println!("Connecting to {}...", url);

    let _ = rustls::crypto::ring::default_provider().install_default();

    match tokio_tungstenite::connect_async(url).await {
        Ok((_ws_stream, response)) => {
            println!("Successfully connected!");
            println!("Response status: {}", response.status());
            println!("Response headers: {:?}", response.headers());
        }
        Err(e) => {
            println!("Failed to connect: {:?}", e);
        }
    }
    Ok(())
}

#[cfg(not(feature = "live-ws"))]
fn main() {
    println!("live-ws feature is required");
}
