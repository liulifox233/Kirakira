use std::{
    env,
    hint::black_box,
    time::{Duration, Instant},
};

use krkr_kag::KagParser;

#[derive(Debug)]
struct Config {
    iterations: usize,
    labels: usize,
    chars_per_label: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            iterations: 2_000,
            labels: 2_000,
            chars_per_label: 16,
        }
    }
}

fn main() {
    let config = parse_args();
    let mut parser = KagParser::new();
    parser
        .load_scenario_text("probe.ks", build_source(&config))
        .expect("probe scenario should load");

    for _ in 0..16 {
        let cloned = black_box(parser.clone());
        black_box(cloned);
    }

    let started = Instant::now();
    let mut checksum = 0_u64;
    for _ in 0..config.iterations {
        let cloned = black_box(parser.clone());
        checksum = checksum
            .wrapping_mul(16_777_619)
            .wrapping_add(cloned.cur_storage().map(str::len).unwrap_or(0) as u64)
            .wrapping_add(cloned.cur_line().unwrap_or(0) as u64);
        black_box(cloned);
    }
    report(&config, started.elapsed(), checksum);
}

fn build_source(config: &Config) -> String {
    let mut source = String::new();
    for index in 0..config.labels {
        source.push('*');
        source.push_str("label");
        source.push_str(&index.to_string());
        source.push('\n');
        for n in 0..config.chars_per_label {
            let ch = (b'a' + (n % 26) as u8) as char;
            source.push(ch);
        }
        source.push_str("[r]\n");
    }
    source
}

fn parse_args() -> Config {
    let mut config = Config::default();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iterations" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage("--iterations requires a value"));
                config.iterations = value
                    .parse()
                    .unwrap_or_else(|_| usage("--iterations must be an integer"));
            }
            "--labels" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage("--labels requires a value"));
                config.labels = value
                    .parse()
                    .unwrap_or_else(|_| usage("--labels must be an integer"));
            }
            "--chars-per-label" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage("--chars-per-label requires a value"));
                config.chars_per_label = value
                    .parse()
                    .unwrap_or_else(|_| usage("--chars-per-label must be an integer"));
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
    eprintln!("usage: kag_probe [--iterations N] [--labels N] [--chars-per-label N]");
    std::process::exit(if message.is_empty() { 0 } else { 2 });
}

fn report(config: &Config, elapsed: Duration, checksum: u64) {
    let iterations_per_second = config.iterations as f64 / elapsed.as_secs_f64();
    eprintln!(
        "kag_probe iterations={} labels={} elapsed={:.3}s iter/s={:.1} checksum={checksum}",
        config.iterations,
        config.labels,
        elapsed.as_secs_f64(),
        iterations_per_second
    );
}
