use clap::Parser;
use publicator::{
    application::Application,
    config::{Config, Paths},
    metadata::{Endpoints, MetadataClient, ModelOption},
    publishing::Publisher,
    store::Store,
    ui::UiOptions,
};
use std::{path::PathBuf, sync::Arc, time::Duration};

#[derive(Parser)]
#[command(name = "publicator-ui")]
struct Args {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    json: Option<PathBuf>,
    /// Initial OpenRouter vision model; unsupported names fail before any provider work.
    #[arg(long, value_parser = publicator::ui::openrouter_model)]
    openrouter_model: Option<ModelOption>,
    #[arg(long, default_value_t = 300)]
    ai_timeout: u64,
}

fn gui_metadata_endpoints() -> Endpoints {
    Endpoints::default()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let paths = Paths::resolve(args.data_dir.as_deref(), args.json.as_deref())?;
    let config = Config::load(&paths.data_dir)?;
    let store = Arc::new(Store::new(paths, config)?);
    let metadata = MetadataClient::new(
        reqwest::Client::new(),
        gui_metadata_endpoints(),
        PathBuf::from("llmfit"),
    );
    let application =
        Arc::new(Application::new(store, metadata).with_publisher(Publisher::from_env()));
    Ok(publicator::ui::run(
        application,
        UiOptions {
            openrouter_model: args.openrouter_model,
            ai_timeout: Duration::from_secs(args.ai_timeout),
        },
    )?)
}

#[cfg(test)]
mod tests {
    use super::{Args, gui_metadata_endpoints};
    use clap::Parser;
    use publicator::metadata::VISION_MODELS;

    #[test]
    fn openrouter_model_selects_a_supported_model_and_refuses_others_at_startup() {
        for name in [
            "google/gemma-4-31b-it:free",
            "openrouter/google/gemma-4-31b-it:free",
        ] {
            let args = Args::try_parse_from(["publicator-ui", "--openrouter-model", name]).unwrap();
            assert_eq!(args.openrouter_model.as_ref(), Some(&VISION_MODELS[1]));
        }
        assert!(
            Args::try_parse_from(["publicator-ui"])
                .unwrap()
                .openrouter_model
                .is_none()
        );
        let refused = Args::try_parse_from(["publicator-ui", "--openrouter-model", "other/model"])
            .err()
            .expect("unsupported model is refused");
        assert!(
            refused.to_string().contains("unsupported OpenRouter model"),
            "{refused}"
        );
    }

    #[test]
    fn gui_ignores_inherited_cli_ollama_endpoint_override() {
        let previous = std::env::var_os("PUBLICATOR_OLLAMA_URL");
        unsafe {
            std::env::set_var("PUBLICATOR_OLLAMA_URL", "http://127.0.0.1:1");
        }
        let ollama = gui_metadata_endpoints().ollama;
        unsafe {
            if let Some(previous) = previous {
                std::env::set_var("PUBLICATOR_OLLAMA_URL", previous);
            } else {
                std::env::remove_var("PUBLICATOR_OLLAMA_URL");
            }
        }
        assert_eq!(ollama, "http://localhost:11434");
    }
}
