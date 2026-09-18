//! Hello-world proof of concept: load a scene, fire a trigger, advance
//! frames, render each step offscreen and dump PNGs.

use cuelight::render::Renderer;
use cuelight::Engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let json = include_str!("scenes/minigolf.json");

    let mut engine = Engine::new();
    engine.load_scene(json)?;
    engine.set_variable("score", 500.0);
    engine.trigger("go");

    let mut renderer = Renderer::new()?;

    let out_dir = std::path::Path::new("target/frames");
    std::fs::create_dir_all(out_dir)?;

    const FPS: f64 = 60.0;
    for frame in 0..=60u32 {
        if frame > 0 {
            engine.advance_frame(1.0 / FPS);
        }
        if frame % 10 == 0 {
            let path = out_dir.join(format!("minigolf_{frame:03}.png"));
            renderer.render_to_rgba(&engine)?.write_png(&path)?;
            println!("wrote {}", path.display());
        }
    }
    Ok(())
}
