use std::fs;

#[derive(Debug, serde::Deserialize)]
struct Config {
    council: Option<toml::Value>,
}

fn main() {
    let content = fs::read_to_string("/home/denisjosifoski/.config/swai/config.toml").unwrap();
    let parsed: Config = toml::from_str(&content).unwrap();
    if let Some(council_val) = parsed.council {
        println!("Council section found");
        let config: Result<swai_core::council::types::CouncilPipelineConfig, _> = council_val.clone().try_into();
        match config {
            Ok(c) => println!("Parsed successfully: {:?}", c),
            Err(e) => println!("Failed to parse: {}", e),
        }
    } else {
        println!("No council section");
    }
}
