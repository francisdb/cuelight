//! Render a show's frames at chosen times, and say what it fired and when.
//!
//! ```sh
//! cuelight-render eclipse/ --at 4.1,19.5,26 -o frames/
//! cuelight-render eclipse/ --every 0.5 --until 52 -o frames/
//! cuelight-render eclipse/ --until 52 --events
//! ```
//!
//! Time is walked in fixed steps of `--fps`, from 0, so a run is
//! repeatable: the same command writes the same bytes. That is also what
//! makes it a test of whether a show is deterministic at all.

use cuelight::render::Renderer;
use cuelight::Engine;
use cuelight_loader::{DriverPlayer, Step};
use std::path::PathBuf;

#[derive(clap::Parser)]
#[command(
    name = "cuelight-render",
    about = "Render a show's frames at chosen times"
)]
struct Cli {
    /// Show folder, loose show file or packed show.
    show: PathBuf,
    /// Times to render, in seconds: `4.1,19.5,26`.
    #[arg(long, value_delimiter = ',')]
    at: Vec<f64>,
    /// Render every this many seconds instead, up to `--until`.
    #[arg(long)]
    every: Option<f64>,
    /// How far to run, in seconds. Required with `--every`, and the end
    /// of the run with `--events`.
    #[arg(long)]
    until: Option<f64>,
    /// Steps per second the show is advanced in. Part of what is being
    /// tested: a chain of timelines can land differently at another rate.
    #[arg(long, default_value_t = 60.0)]
    fps: f64,
    /// Where the frames go; created if it is not there.
    #[arg(short, long, default_value = "frames")]
    out: PathBuf,
    /// Print what the show fired, with the time, and render nothing.
    #[arg(long)]
    events: bool,
    /// Ignore the folder's driver script.
    #[arg(long)]
    no_driver: bool,
    /// Fire a trigger at a time: `--trigger 2.5:go`, repeatable.
    #[arg(long = "trigger", value_name = "TIME:NAME")]
    triggers: Vec<String>,
    /// Set a variable at a time: `--set 0:score=1500`, repeatable.
    #[arg(long = "set", value_name = "TIME:VAR=VALUE")]
    sets: Vec<String>,
}

/// An input the command line asks for, at the time it asks for it.
#[derive(Debug)]
struct Input {
    at: f64,
    what: Step,
}

fn inputs(cli: &Cli) -> Result<Vec<Input>, String> {
    let split = |s: &str| -> Result<(f64, String), String> {
        let (time, rest) = s
            .split_once(':')
            .ok_or_else(|| format!("{s:?} needs a time, as in 2.5:name"))?;
        let at: f64 = time
            .parse()
            .map_err(|_| format!("{time:?} in {s:?} is not a time"))?;
        Ok((at, rest.to_owned()))
    };
    let mut out = Vec::new();
    for arg in &cli.triggers {
        let (at, trigger) = split(arg)?;
        out.push(Input {
            at,
            what: Step::Trigger { trigger },
        });
    }
    for arg in &cli.sets {
        let (at, rest) = split(arg)?;
        let (name, value) = rest
            .split_once('=')
            .ok_or_else(|| format!("{arg:?} needs a value, as in 0:score=1500"))?;
        let value = match value.parse::<f64>() {
            Ok(number) => cuelight::Value::Number(number),
            Err(_) => cuelight::Value::Text(value.to_owned()),
        };
        out.push(Input {
            at,
            what: Step::Set {
                set: std::collections::BTreeMap::from([(name.to_owned(), value)]),
            },
        });
    }
    out.sort_by(|a, b| a.at.total_cmp(&b.at));
    Ok(out)
}

/// The times to render, in order.
fn wanted(cli: &Cli) -> Result<Vec<f64>, String> {
    if let Some(every) = cli.every {
        if !(every.is_finite() && every > 0.0) {
            return Err("--every wants a number above 0".into());
        }
        let until = cli.until.ok_or("--every needs --until")?;
        let mut times = Vec::new();
        let mut t = 0.0;
        while t <= until + f64::EPSILON {
            times.push(t);
            t += every;
        }
        return Ok(times);
    }
    let mut times = cli.at.clone();
    times.sort_by(f64::total_cmp);
    Ok(times)
}

fn run(cli: &Cli) -> Result<(), String> {
    let mut engine = Engine::new();
    let loaded = cuelight_loader::load(&mut engine, &cli.show).map_err(|e| e.to_string())?;
    for name in &loaded.skipped {
        eprintln!("skipped {name}");
    }
    for warning in engine.load_warnings() {
        eprintln!("warning: {warning}");
    }
    let mut driver = loaded
        .driver
        .filter(|_| !cli.no_driver)
        .map(DriverPlayer::new);
    let mut inputs = inputs(cli)?.into_iter().peekable();
    let mut times = wanted(cli)?.into_iter().peekable();

    if !cli.events && times.peek().is_none() {
        return Err("nothing to render: give --at or --every, or ask for --events".into());
    }
    let last = cli
        .until
        .or_else(|| wanted(cli).ok().and_then(|t| t.last().copied()))
        .unwrap_or(0.0);
    let step = 1.0 / cli.fps.max(1.0);

    let mut renderer = match cli.events {
        true => None,
        false => Some(Renderer::new().map_err(|e| format!("no renderer: {e}"))?),
    };
    if renderer.is_some() {
        std::fs::create_dir_all(&cli.out).map_err(|e| format!("{}: {e}", cli.out.display()))?;
    }

    // Walked in fixed steps from 0, so the run is repeatable and a frame
    // at a time is reached the same way however many were asked for.
    let mut time = 0.0;
    let mut steps = 0u64;
    let mut frames = 0;
    loop {
        while inputs.peek().is_some_and(|i| i.at <= time) {
            match inputs.next().expect("peeked").what {
                Step::Trigger { trigger } => engine.trigger(&trigger),
                Step::Set { set } => {
                    for (name, value) in set {
                        engine.set_variable(&name, value);
                    }
                }
                _ => {}
            }
        }
        // A frame is due once the clock has reached it.
        while times.peek().is_some_and(|t| *t <= time + step / 2.0) {
            let at = times.next().expect("peeked");
            if let Some(renderer) = &mut renderer {
                let file = cli.out.join(format!("t{at:08.3}.png"));
                renderer
                    .render_to_rgba(&engine)
                    .map_err(|e| e.to_string())?
                    .write_png(&file)
                    .map_err(|e| format!("{}: {e}", file.display()))?;
                frames += 1;
            }
        }
        if cli.events {
            for event in engine.drain_events() {
                // The engine's own clock, not the frame clock: an event
                // that landed part way through a frame says when.
                println!("{:8.3}  {event:?}", engine.time());
            }
        }
        if time >= last {
            break;
        }
        if let Some(driver) = &mut driver {
            for played in driver.advance(&mut engine, step) {
                if cli.events {
                    println!("{time:8.3}  driver {played:?}");
                }
            }
        }
        steps += 1;
        // Multiplied, not accumulated, and handed to the engine as the
        // instant to land on rather than as a delta, so a run at one
        // frame rate reaches a given time in exactly the state a run at
        // another does.
        time = steps as f64 * step;
        engine.advance_to(time);
    }
    if frames > 0 {
        println!("{frames} frame(s) in {}", cli.out.display());
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    let cli = <Cli as clap::Parser>::parse();
    match run(&cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cuelight-render: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli() -> Cli {
        Cli {
            show: PathBuf::from("show"),
            at: Vec::new(),
            every: None,
            until: None,
            fps: 60.0,
            out: PathBuf::from("frames"),
            events: false,
            no_driver: false,
            triggers: Vec::new(),
            sets: Vec::new(),
        }
    }

    #[test]
    fn times_come_back_in_order() {
        let mut c = cli();
        c.at = vec![26.0, 4.1, 19.5];
        assert_eq!(wanted(&c).unwrap(), [4.1, 19.5, 26.0]);
    }

    #[test]
    fn a_strip_covers_its_end() {
        let mut c = cli();
        (c.every, c.until) = (Some(0.5), Some(2.0));
        assert_eq!(wanted(&c).unwrap(), [0.0, 0.5, 1.0, 1.5, 2.0]);
    }

    #[test]
    fn a_strip_needs_an_end_and_a_step_above_zero() {
        let mut c = cli();
        c.every = Some(0.5);
        assert!(wanted(&c).is_err(), "no --until");
        (c.every, c.until) = (Some(0.0), Some(2.0));
        assert!(wanted(&c).is_err(), "a step of nothing never arrives");
    }

    #[test]
    fn inputs_are_read_and_ordered_by_their_time() {
        let mut c = cli();
        c.triggers = vec!["2.5:go".into()];
        c.sets = vec!["0:score=1500".into(), "1:mode=multiball".into()];
        let inputs = inputs(&c).unwrap();
        let times: Vec<f64> = inputs.iter().map(|i| i.at).collect();
        assert_eq!(times, [0.0, 1.0, 2.5]);
        // A number stays a number and anything else is text.
        match &inputs[0].what {
            Step::Set { set } => assert_eq!(set["score"], cuelight::Value::Number(1500.0)),
            other => panic!("{other:?}"),
        }
        match &inputs[1].what {
            Step::Set { set } => {
                assert_eq!(set["mode"], cuelight::Value::Text("multiball".into()));
            }
            other => panic!("{other:?}"),
        }
        match &inputs[2].what {
            Step::Trigger { trigger } => assert_eq!(trigger, "go"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_malformed_input_says_what_it_wanted() {
        let mut c = cli();
        c.triggers = vec!["go".into()];
        assert!(inputs(&c).unwrap_err().contains("needs a time"));
        c.triggers.clear();
        c.sets = vec!["0:score".into()];
        assert!(inputs(&c).unwrap_err().contains("needs a value"));
    }
}
