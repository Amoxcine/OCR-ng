//! Write a generated scan (no committed scans: see `synth`) for trying scan editing by hand:
//! `cargo run -p pdfcraft-scanedit --example scan_fixture -- out.pdf [--no-text-layer] [--color]`.

use pdfcraft_scanedit::synth::{Degrade, Spec, generate};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args.iter().find(|a| !a.starts_with("--")).ok_or("usage: scan_fixture OUT.pdf [--no-text-layer] [--color]")?;
    let spec = Spec {
        lines: vec![
            "Dear customer, your order number 4521 has been".into(),
            "shipped today and should arrive within three".into(),
            "working days. Thank you for your patience.".into(),
        ],
        dpi: 300.0,
        text_layer: !args.iter().any(|a| a == "--no-text-layer"),
        color: args.iter().any(|a| a == "--color"),
        ink: if args.iter().any(|a| a == "--color") { [0.1, 0.15, 0.55] } else { [0.08, 0.08, 0.1] },
        degrade: Degrade { paper: [250.0, 243.0, 225.0], ..Degrade::default() },
        ..Spec::default()
    };
    std::fs::write(out, generate(&spec)?.pdf)?;
    Ok(())
}
