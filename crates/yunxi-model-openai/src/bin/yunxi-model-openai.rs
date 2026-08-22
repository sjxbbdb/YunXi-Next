//! Standalone process entry point for the OpenAI-compatible model plugin.

fn main() {
    if let Err(error) = yunxi_model_openai::run_model_plugin_from_env() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
