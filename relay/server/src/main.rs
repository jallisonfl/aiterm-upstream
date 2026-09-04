use aiterm_relay::{run, RelayConfig};

#[tokio::main]
async fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "relay.json".to_string());
    let result = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read {path}: {error}"))
        .and_then(|json| {
            serde_json::from_str::<RelayConfig>(&json)
                .map_err(|error| format!("could not parse {path}: {error}"))
        });
    let config = match result {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    if let Err(error) = run(config).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
