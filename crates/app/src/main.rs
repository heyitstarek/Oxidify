//! Oxidify desktop shell: native boot, single-instance guard, async runtime,
//! and the eframe application loop. Engine/UI construction lives in `shell`.

mod desktop;
mod diag;
mod shell;

use diag::diag;

fn main() -> anyhow::Result<()> {
    diag("main start");
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Only one desktop instance owns the audio device, tray, and hotkeys.
    let _instance = desktop::acquire_single_instance();
    diag("single instance ok");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .thread_name("oxidify-worker")
        .enable_all()
        .build()?;
    diag("runtime built");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Oxidify")
            .with_app_id("com.oxidify.Oxidify")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Oxidify",
        options,
        Box::new(move |cc| {
            diag("creator called");
            let shell = shell::OxidifyShell::create(&cc.egui_ctx, rt.handle().clone())
                .map(|s| Box::new(s) as Box<dyn eframe::App>);
            diag("creator done");
            shell.map_err(|e| e.into())
        }),
    )
    .map_err(|e| anyhow::anyhow!("window error: {e}"))?;
    Ok(())
}
