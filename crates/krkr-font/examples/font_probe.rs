use std::{
    env,
    hint::black_box,
    time::{Duration, Instant},
};

use krkr_font::{FontSpec, FontSystem, TextStyle};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Metrics,
    Draw,
    Effect,
}

impl Mode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "metrics" => Some(Self::Metrics),
            "draw" => Some(Self::Draw),
            "effect" => Some(Self::Effect),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct Config {
    mode: Mode,
    iterations: usize,
    width: u32,
    height: u32,
    face: String,
    font_height: f32,
    effect_width: i32,
    text: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Effect,
            iterations: 2_000,
            width: 960,
            height: 540,
            face: String::new(),
            font_height: 28.0,
            effect_width: 1,
            text: default_probe_text(),
        }
    }
}

fn main() {
    let config = parse_args();
    let system = FontSystem::new();
    let font = FontSpec {
        face: config.face.clone(),
        height: config.font_height,
        ..FontSpec::default()
    };
    let style = TextStyle {
        color: [245, 241, 232, 255],
        anti_alias: true,
        shadow: None,
    };
    let effect_style = TextStyle {
        color: [16, 20, 24, 220],
        anti_alias: true,
        shadow: None,
    };
    let mut pixels = vec![0; config.width as usize * config.height as usize * 4];

    for _ in 0..32 {
        run_iteration(&system, &font, style, effect_style, &config, &mut pixels);
    }

    let started = Instant::now();
    let mut checksum = 0_u64;
    for _ in 0..config.iterations {
        checksum = checksum.wrapping_add(run_iteration(
            &system,
            &font,
            style,
            effect_style,
            &config,
            &mut pixels,
        ));
    }
    let elapsed = started.elapsed();
    report(&config, elapsed, checksum);
}

fn run_iteration(
    system: &FontSystem,
    font: &FontSpec,
    style: TextStyle,
    effect_style: TextStyle,
    config: &Config,
    pixels: &mut [u8],
) -> u64 {
    match config.mode {
        Mode::Metrics => {
            let metrics = system.text_metrics(font, black_box(&config.text));
            u64::from(metrics.width.to_bits()) ^ u64::from(metrics.height.to_bits())
        }
        Mode::Draw => {
            pixels.fill(0);
            let layout = system.layout_text(font, black_box(&config.text));
            system.draw_text_layout_to_rgba(
                font,
                style,
                pixels,
                config.width,
                config.height,
                32,
                32,
                &layout,
            );
            checksum(pixels)
        }
        Mode::Effect => {
            pixels.fill(0);
            let layout = system.layout_text(font, black_box(&config.text));
            let metrics = layout.metrics();
            black_box(metrics);
            draw_effect(system, font, effect_style, &layout, config, pixels);
            system.draw_text_layout_to_rgba(
                font,
                style,
                pixels,
                config.width,
                config.height,
                32,
                32,
                &layout,
            );
            checksum(pixels)
        }
    }
}

fn draw_effect(
    system: &FontSystem,
    font: &FontSpec,
    style: TextStyle,
    layout: &krkr_font::TextLayout,
    config: &Config,
    pixels: &mut [u8],
) {
    let spread = config.effect_width.max(0);
    for dy in -spread..=spread {
        for dx in -spread..=spread {
            if dx == 0 && dy == 0 {
                continue;
            }
            system.draw_text_layout_to_rgba(
                font,
                style,
                pixels,
                config.width,
                config.height,
                32 + dx,
                32 + dy,
                layout,
            );
        }
    }
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().step_by(257).fold(0_u64, |sum, byte| {
        sum.wrapping_mul(16777619) ^ u64::from(*byte)
    })
}

fn parse_args() -> Config {
    let mut config = Config::default();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage("--mode requires a value"));
                config.mode = Mode::parse(&value)
                    .unwrap_or_else(|| usage("mode must be metrics, draw, or effect"));
            }
            "--iterations" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage("--iterations requires a value"));
                config.iterations = value
                    .parse()
                    .unwrap_or_else(|_| usage("--iterations must be an integer"));
            }
            "--width" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage("--width requires a value"));
                config.width = value
                    .parse()
                    .unwrap_or_else(|_| usage("--width must be an integer"));
            }
            "--height" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage("--height requires a value"));
                config.height = value
                    .parse()
                    .unwrap_or_else(|_| usage("--height must be an integer"));
            }
            "--face" => {
                config.face = args
                    .next()
                    .unwrap_or_else(|| usage("--face requires a value"));
            }
            "--font-height" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage("--font-height requires a value"));
                config.font_height = value
                    .parse()
                    .unwrap_or_else(|_| usage("--font-height must be a number"));
            }
            "--effect-width" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage("--effect-width requires a value"));
                config.effect_width = value
                    .parse()
                    .unwrap_or_else(|_| usage("--effect-width must be an integer"));
            }
            "--text" => {
                config.text = args
                    .next()
                    .unwrap_or_else(|| usage("--text requires a value"));
            }
            "--help" | "-h" => usage(""),
            _ => usage(&format!("unknown argument `{arg}`")),
        }
    }
    config
}

fn usage(message: &str) -> ! {
    if !message.is_empty() {
        eprintln!("{message}");
    }
    eprintln!(
        "usage: font_probe [--mode metrics|draw|effect] [--iterations N] [--face NAME] \
         [--font-height PX] [--effect-width PX] [--width PX] [--height PX] [--text TEXT]"
    );
    std::process::exit(if message.is_empty() { 0 } else { 2 });
}

fn report(config: &Config, elapsed: Duration, checksum: u64) {
    let iterations_per_second = config.iterations as f64 / elapsed.as_secs_f64();
    eprintln!(
        "font_probe mode={:?} iterations={} elapsed={:.3}s iter/s={:.1} checksum={checksum}",
        config.mode,
        config.iterations,
        elapsed.as_secs_f64(),
        iterations_per_second
    );
}

fn default_probe_text() -> String {
    let line = concat!(
        "\u{6211}\u{628a}\u{9b54}\u{6cd5}\u{7eb8}\u{7247}\u{653e}\u{5728}",
        "\u{684c}\u{4e0a}\u{ff0c}\u{58a8}\u{6c34}\u{987a}\u{7740}\u{6298}",
        "\u{75d5}\u{6162}\u{6162}\u{6697}\u{4e0b}\u{53bb}\u{3002}",
        "\u{5979}\u{6ca1}\u{6709}\u{56de}\u{5934}\u{ff0c}\u{53ea}\u{662f}",
        "\u{8f7b}\u{58f0}\u{95ee}\u{6211}\u{662f}\u{5426}\u{8fd8}",
        "\u{8bb0}\u{5f97}\u{90a3}\u{4e2a}\u{540d}\u{5b57}\u{3002}"
    );
    [line, line, line].join("\n")
}
