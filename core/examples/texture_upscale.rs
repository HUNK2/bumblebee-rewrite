//! Export inspectable upscales from the user's install, never from bundled pixels.
//! cargo run -p tf2-core --example texture_upscale -- [output-directory] [2|4]

use std::path::{Path, PathBuf};
use tf2_core::formats::{model::Library, pack::Pack};
use tf2_core::texture_upscale::{map_kind, save_png, upscale};

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let output = args.first().map_or_else(
        || PathBuf::from("work/textures/bumblebee-4x"),
        PathBuf::from,
    );
    let factor = args
        .get(1)
        .map_or(Ok(4), |s| s.parse::<u32>())
        .map_err(|e| e.to_string())?;
    if !matches!(factor, 2 | 4) {
        return Err("factor must be 2 or 4".into());
    }
    let install = std::env::var("TF2_GAME_DIR").unwrap_or_else(|_| "C:/Games2".into());
    let pack = Pack::open(&Path::new(&install).join("characters/bumblebee.str"))?;
    let library = Library::load(&pack);
    let mut textures: Vec<_> = library
        .textures
        .values()
        .filter_map(|t| map_kind(t).map(|kind| (t, kind)))
        .collect();
    textures.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    std::fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let mut report = "source\tkind\toriginal\tupscaled\tmips\tpng\n".to_string();
    for (source, kind) in textures {
        let name = source
            .name
            .rsplit(['/', '\\'])
            .next()
            .unwrap()
            .trim_end_matches(".dds");
        let result = upscale(source, factor, kind)?;
        save_png(source, &output.join(format!("{name}_original.png")))?;
        let png = format!("{name}_{factor}x.png");
        save_png(&result, &output.join(&png))?;
        report.push_str(&format!(
            "{}\t{kind:?}\t{}x{}\t{}x{}\t{}\t{png}\n",
            source.name, source.width, source.height, result.width, result.height, result.mips
        ));
        println!(
            "{name}: {}x{} -> {}x{} ({kind:?})",
            source.width, source.height, result.width, result.height
        );
    }
    std::fs::write(output.join("manifest.tsv"), report).map_err(|e| e.to_string())?;
    println!("Local outputs: {}", output.display());
    Ok(())
}
